//! Persistence + snapshot helpers for [`CodeIndexer`].
//!
//! Why: snapshotting the chunk corpus and HNSW graph to disk is a self-contained
//! concern (issue #85). Lifting it out of the god `impl CodeIndexer` block keeps
//! the search/ingest paths focused on their own state mutation logic.
//! What: holds `save_chunks_to_disk`, `load_chunks_from_disk`, `save_vector_store`,
//! `set_store`, the BM25 document-text helper, and the coalesced background
//! persist task (`spawn_incremental_persist`).
//! Test: covered by `test_save_chunks_roundtrip`, `test_load_chunks_missing_file_returns_zero`,
//! and `test_persist_coalesces_concurrent_calls` in `indexer::tests`.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::core::chunker::RawChunk;
use crate::core::entity::RawEntity;
use crate::core::store::VectorStore;

use super::{ChunkSnapshot, CodeIndexer};

/// Restored corpus payload: the full chunk list paired with the per-file
/// entity lists. Named so the `spawn_blocking` closure that produces it on the
/// redb warm-boot path (`load_chunks_from_redb`) has a readable signature.
type RestoredCorpus = (Vec<RawChunk>, Vec<(String, Vec<RawEntity>)>);

impl CodeIndexer {
    /// Snapshot the in-memory chunk corpus + entities to disk as JSON.
    ///
    /// Why (issue #85): on graceful shutdown (and incrementally after each
    /// committed batch) we persist the corpus so a restart can rebuild BM25
    /// and the symbol graph without re-parsing the source tree. Pairs with
    /// [`VectorStore::save_to`] which persists the HNSW vectors.
    /// What: copies chunks + entities under read locks (releasing them before
    /// the I/O), then writes JSON atomically via tmp + rename. Empty corpus
    /// is still written so the on-disk file accurately reflects state.
    /// Test: see `tests::test_save_chunks_roundtrip`.
    pub async fn save_chunks_to_disk(&self, path: &std::path::Path) -> Result<()> {
        // Snapshot under read locks, then drop them before doing I/O so
        // concurrent searches never block on the JSON serialize.
        let chunks_vec: Vec<RawChunk> = {
            let chunks = self.chunks.read().await;
            chunks.values().cloned().collect()
        };
        let entities_vec: Vec<(String, Vec<RawEntity>)> = {
            let entities = self.entities.read().await;
            entities
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        let snapshot = ChunkSnapshot {
            version: 1,
            chunks: chunks_vec,
            entities: entities_vec,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create parent of {}", path.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(&snapshot).context("serialize chunk corpus snapshot")?;
        std::fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
        Ok(())
    }

    /// Restore the chunk corpus + entities from a previous snapshot. After
    /// load, rebuilds BM25 + the symbol graph so the search pipeline is
    /// immediately usable. The HNSW vectors must be restored separately via
    /// `UsearchStore::load_from` before this is called.
    ///
    /// Why (issue #85): the daemon's `restore_indexes` startup hook calls
    /// this so registered indexes come back warm without re-embedding.
    /// What: reads the JSON snapshot, repopulates `chunks` + `entities`,
    /// runs `commit_bm25_batch` against the restored chunks to refill the
    /// posting list, then rebuilds the symbol graph. Returns the number of
    /// chunks restored. Missing/corrupt file → `Ok(0)` (graceful fallback).
    /// Test: see `tests::test_save_chunks_roundtrip`.
    pub async fn load_chunks_from_disk(&self, path: &std::path::Path) -> Result<usize> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        let snapshot: ChunkSnapshot = match serde_json::from_slice(&bytes) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "chunk snapshot at {} is corrupt ({e}) — starting with empty corpus",
                    path.display()
                );
                return Ok(0);
            }
        };

        let total = snapshot.chunks.len();
        // Phase 1: refill BM25 from the restored corpus before publishing the
        // chunks map so concurrent reads can't observe a half-state.
        {
            let mut bm25 = self.bm25.write().await;
            for chunk in &snapshot.chunks {
                let text = Self::bm25_doc_text(chunk);
                bm25.upsert_document(&chunk.id, &text);
            }
        }
        // Phase 2: publish chunks under a single write lock.
        {
            let mut corpus = self.chunks.write().await;
            for chunk in snapshot.chunks {
                corpus.insert(chunk.id.clone(), chunk);
            }
        }
        // Phase 3: publish entities.
        {
            let mut emap = self.entities.write().await;
            for (file, ents) in snapshot.entities {
                emap.insert(file, ents);
            }
        }
        // Phase 4: rebuild the symbol graph so KG expansion works on the
        // restored corpus immediately. Cheap relative to re-embedding.
        self.rebuild_symbol_graph().await;
        tracing::info!(
            "restored {} chunks for index '{}' from {}",
            total,
            self.index_id,
            path.display()
        );
        Ok(total)
    }

    /// Restore the chunk corpus + entities from the durable redb store
    /// (issue #28). Counterpart of [`Self::load_chunks_from_disk`], which read
    /// the legacy `chunks.json` snapshot.
    ///
    /// Why: redb replaces the full-rewrite JSON snapshot. The warm-boot path
    /// calls this first; only when the redb corpus is empty (a fresh install,
    /// or the first boot after upgrading from a JSON-snapshot build) does the
    /// caller fall back to [`Self::load_chunks_from_disk`] for a one-time
    /// migration read.
    /// What: reads every chunk + entity row from `CorpusStore` on a blocking
    /// worker (redb's API is sync), refills BM25, publishes `chunks` +
    /// `entities`, and rebuilds the symbol graph — mirroring the JSON path's
    /// four-phase publish. Returns the number of chunks restored. A `None`
    /// corpus store (test indexer) or an empty store yields `Ok(0)` so the
    /// caller cleanly falls through to the JSON migration branch.
    /// Test: `tests::test_corpus_store_roundtrip`.
    pub async fn load_chunks_from_redb(&self) -> Result<usize> {
        let Some(corpus) = self.corpus.clone() else {
            return Ok(0);
        };
        // redb's transaction API is synchronous; do the (potentially large)
        // deserialize on a blocking worker so we don't pin a runtime thread.
        let (chunks, entities) = tokio::task::spawn_blocking(move || -> Result<RestoredCorpus> {
            let chunks = corpus.load_all_chunks()?;
            let entities = corpus.load_all_entities()?;
            Ok((chunks, entities))
        })
        .await
        .context("redb corpus load task panicked")??;

        let total = chunks.len();
        if total == 0 {
            return Ok(0);
        }
        // Phase 1: refill BM25 before publishing chunks so concurrent reads
        // can't observe a half-state (mirrors `load_chunks_from_disk`).
        {
            let mut bm25 = self.bm25.write().await;
            for chunk in &chunks {
                let text = Self::bm25_doc_text(chunk);
                bm25.upsert_document(&chunk.id, &text);
            }
        }
        // Phase 2: publish chunks.
        {
            let mut corpus_map = self.chunks.write().await;
            for chunk in chunks {
                corpus_map.insert(chunk.id.clone(), chunk);
            }
        }
        // Phase 3: publish entities.
        {
            let mut emap = self.entities.write().await;
            for (file, ents) in entities {
                emap.insert(file, ents);
            }
        }
        // Phase 4: bring the symbol graph back online. Issue #41 phase 2:
        // prefer the persisted graph (O(nodes + edges) load) over a full
        // `build_from_chunks` rebuild (O(N chunks)). A `None`/empty persisted
        // graph (fresh redb, never-saved) falls through to the rebuild path
        // so first-boot still produces a working KG.
        self.load_or_rebuild_symbol_graph().await;
        tracing::info!(
            "restored {} chunks for index '{}' from redb corpus",
            total,
            self.index_id
        );
        Ok(total)
    }

    /// Warm-boot helper: load the persisted KG when present, otherwise fall
    /// back to a full `rebuild_symbol_graph` (issue #41 phase 2).
    ///
    /// Why: cold-start cost of `build_from_chunks` on a 100k-chunk corpus is
    /// the dominant non-embedding cost of a warm-boot. Loading the persisted
    /// graph collapses that to a redb read; the rebuild is only paid on the
    /// genuine first-boot (when nothing has been saved yet) or after a schema
    /// migration that clears `KG_NODES_TABLE`.
    /// What: if a `CorpusStore` is wired, calls
    /// `SymbolGraph::load_from_corpus` on a blocking worker. On `Ok(Some)`
    /// installs that graph directly; on `Ok(None)` or `Err` (logged at
    /// `warn`) falls back to `rebuild_symbol_graph`.
    /// Test: covered by the `corpus_kg_warm_boot_roundtrip` integration
    /// test path; the symbol-graph round-trip itself is unit-tested in
    /// `core::symbol_graph::tests`.
    pub(super) async fn load_or_rebuild_symbol_graph(&self) {
        let Some(corpus) = self.corpus.clone() else {
            self.rebuild_symbol_graph().await;
            return;
        };
        let index_id = self.index_id.clone();
        let join = tokio::task::spawn_blocking(move || {
            crate::core::symbol_graph::SymbolGraph::load_from_corpus(&corpus)
        })
        .await;
        match join {
            Ok(Ok(Some(graph))) => {
                tracing::info!(
                    "warm-boot: loaded persisted symbol graph for '{index_id}' \
                     ({} nodes / {} edges)",
                    graph.node_count(),
                    graph.edge_count()
                );
                *self.symbol_graph.write().await = std::sync::Arc::new(graph);
            }
            Ok(Ok(None)) => {
                tracing::info!(
                    "warm-boot: no persisted KG for '{index_id}' — \
                     rebuilding from chunk corpus"
                );
                self.rebuild_symbol_graph().await;
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    "warm-boot: KG load for '{index_id}' failed ({e}) — \
                     falling back to rebuild_symbol_graph"
                );
                self.rebuild_symbol_graph().await;
            }
            Err(e) => {
                tracing::warn!(
                    "warm-boot: KG load task panicked for '{index_id}' ({e}) — \
                     falling back to rebuild_symbol_graph"
                );
                self.rebuild_symbol_graph().await;
            }
        }
    }

    /// One-time migration: copy a legacy `chunks.json` snapshot into the redb
    /// corpus store (issue #28).
    ///
    /// Why: daemons upgraded from a JSON-snapshot build have a populated
    /// `chunks.json` but an empty `index.redb`. After the warm-boot path reads
    /// the JSON snapshot into memory it calls this to seed redb so every
    /// subsequent restart uses the fast redb path and the JSON file becomes
    /// inert. Best-effort: a failure is logged, not fatal — the in-memory
    /// corpus is already live and the next reindex will populate redb anyway.
    /// What: snapshots the current in-memory `chunks` + `entities` under read
    /// locks and writes them to the `CorpusStore` on a blocking worker.
    /// Test: `tests::test_corpus_store_migrates_from_json`.
    pub async fn migrate_corpus_to_redb(&self) {
        let Some(corpus) = self.corpus.clone() else {
            return;
        };
        let chunks: Vec<RawChunk> = {
            let g = self.chunks.read().await;
            g.values().cloned().collect()
        };
        let entities: Vec<(String, Vec<RawEntity>)> = {
            let g = self.entities.read().await;
            g.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        if chunks.is_empty() {
            return;
        }
        let total = chunks.len();
        let index_id = self.index_id.clone();
        // Issue #29: write chunks + entities in one atomic redb transaction so
        // a crash mid-migration never leaves the two tables inconsistent.
        let result = tokio::task::spawn_blocking(move || -> Result<()> {
            corpus.upsert_batch(&chunks, &entities)
        })
        .await;
        match result {
            Ok(Ok(())) => tracing::info!(
                "index '{index_id}': migrated {total} chunks from chunks.json to redb"
            ),
            Ok(Err(e)) => {
                tracing::warn!("index '{index_id}': redb corpus migration failed ({e})")
            }
            Err(e) => {
                tracing::warn!("index '{index_id}': redb corpus migration task panicked ({e})")
            }
        }
    }

    /// Flush the chunk corpus durably on shutdown, picking the right backend.
    ///
    /// Why (issue #28): the shutdown hook must persist the corpus, but the
    /// path differs by backend. A redb-backed index has already committed
    /// every batch transactionally, so shutdown only needs a final consistency
    /// sweep (re-upserting the in-memory map covers the rare case of an
    /// in-flight batch whose redb write lost a race with SIGTERM) — crucially
    /// **without** the full-rewrite JSON encode that triggered the
    /// memory-explosion. A legacy index (no `CorpusStore`) still needs the
    /// JSON snapshot at `path`.
    /// What: when a `CorpusStore` is wired, snapshots the in-memory corpus and
    /// upserts it into redb in a single atomic transaction (issue #29);
    /// otherwise delegates to `save_chunks_to_disk`.
    /// Test: covered by the corpus roundtrip integration test plus the
    /// existing shutdown integration test.
    pub async fn flush_corpus_to_disk(&self, path: &std::path::Path) -> Result<()> {
        let Some(corpus) = self.corpus.clone() else {
            // Legacy path: no redb store wired — write the JSON snapshot.
            return self.save_chunks_to_disk(path).await;
        };
        let chunks: Vec<RawChunk> = {
            let g = self.chunks.read().await;
            g.values().cloned().collect()
        };
        let entities: Vec<(String, Vec<RawEntity>)> = {
            let g = self.entities.read().await;
            g.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        // Issue #29: one atomic transaction covering both tables so a crash
        // during the shutdown flush never leaves chunks and entities torn.
        tokio::task::spawn_blocking(move || -> Result<()> {
            corpus.upsert_batch(&chunks, &entities)
        })
        .await
        .context("redb corpus shutdown-flush task panicked")?
    }

    /// Snapshot the HNSW vector store, if one is wired. Best-effort: returns
    /// `Ok(false)` if no store is attached (BM25-only mode) so callers can
    /// chain without checking.
    pub async fn save_vector_store(&self, path: &std::path::Path) -> Result<bool> {
        let Some(store) = &self.store else {
            return Ok(false);
        };
        store.save_to(path).await?;
        Ok(true)
    }

    /// Install a pre-loaded `VectorStore` (typically a restored `UsearchStore`)
    /// onto this indexer. Used by the warm-boot path so the persisted HNSW
    /// graph is wired in before `load_chunks_from_disk` runs.
    pub fn set_store(&mut self, store: Arc<dyn VectorStore>) {
        self.store = Some(store);
    }

    /// Compose the BM25 document text for a chunk: body + virtual_terms,
    /// matching the layout the per-query rebuild used to construct.
    pub(super) fn bm25_doc_text(chunk: &RawChunk) -> String {
        if chunk.virtual_terms.is_empty() {
            chunk.content.clone()
        } else {
            let mut s = String::with_capacity(
                chunk.content.len()
                    + chunk
                        .virtual_terms
                        .iter()
                        .map(|t| t.len() + 1)
                        .sum::<usize>(),
            );
            s.push_str(&chunk.content);
            for t in &chunk.virtual_terms {
                s.push(' ');
                s.push_str(t);
            }
            s
        }
    }

    /// Force an HNSW snapshot now, bypassing the per-batch throttle
    /// ([`crate::core::indexer::HNSW_SNAPSHOT_BATCH_INTERVAL`], issue #29).
    ///
    /// Why: `commit_parsed_batch` only triggers the (expensive) background
    /// HNSW snapshot every 16 batches. After the reindex orchestrator's batch
    /// loop ends, the most recent ≤15 batches' vectors may not yet be on disk.
    /// Calling this once after the loop guarantees the final HNSW state is
    /// persisted so a crash before the next reindex doesn't lose those
    /// vectors. (The chunk corpus is already durable per-batch via redb.)
    /// What: delegates to `spawn_incremental_persist(true)`, which always
    /// spawns the persist task regardless of the batch counter.
    /// Test: `tests::test_incremental_persist_throttles_to_interval` asserts a
    /// forced call persists even when the throttle would otherwise skip.
    pub fn force_incremental_persist(&self) {
        self.spawn_incremental_persist(true);
    }

    /// Spawn a background task that snapshots the HNSW graph + chunk corpus
    /// for this index to disk. Best-effort: a failure is logged but never
    /// returned to the caller — persistence is a "backup", not the source of
    /// truth, so a partial save can't corrupt live state.
    ///
    /// Why: called from `commit_parsed_batch` so incremental progress is
    /// preserved across crashes. The actual save runs on a detached task so
    /// the commit path returns immediately. Issue #29: a full `Index::save`
    /// costs hundreds of ms on a large HNSW graph and the chunk corpus is
    /// already persisted transactionally per batch by `commit_corpus_to_redb`,
    /// so a non-`force` call only proceeds once every
    /// [`crate::core::indexer::HNSW_SNAPSHOT_BATCH_INTERVAL`] batches. The
    /// reindex orchestrator calls [`Self::force_incremental_persist`] after
    /// its batch loop so the final state is always durable.
    /// What: when `force` is false, increments the per-index batch counter and
    /// returns early unless the counter is a multiple of the snapshot interval.
    /// Otherwise skips when the daemon's data dir is unresolvable (tests,
    /// broken HOME env), then snapshots HNSW (via `VectorStore::save_to`) and
    /// chunks (via `save_chunks_to_disk`) concurrently with regular search
    /// traffic — both snapshot under read locks before doing I/O.
    /// Test: `tests::test_incremental_persist_throttles_to_interval` plus the
    /// integration tests that mutate an index then assert the on-disk file
    /// appears within a short timeout.
    pub(super) fn spawn_incremental_persist(&self, force: bool) {
        // Issue #29: throttle the per-batch HNSW snapshot. A non-forced caller
        // (every committed batch) bumps the counter; only every Nth batch
        // actually spawns the save. `fetch_add` returns the *previous* value,
        // so the first batch is index 1 — the modulo is checked against that
        // post-increment value so batch 16, 32, … trigger a snapshot.
        if !force {
            let n = self
                .persist_state
                .batch_counter
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1);
            if !n.is_multiple_of(crate::core::indexer::HNSW_SNAPSHOT_BATCH_INTERVAL) {
                return;
            }
        }
        // Memory-explosion fix: coalesce concurrent calls so at most ONE
        // persist task is alive per index. Each task allocates ~1× the corpus
        // footprint (clone all RawChunks + serialize to JSON bytes); without
        // this guard, a 600-batch reindex stacked 600 such tasks and the
        // daemon was OOM-killed at 46–174 GB RSS.
        //
        // Protocol:
        //   1. Every caller sets `dirty = true` (publishes "there is new
        //      state worth persisting").
        //   2. Every caller try-acquires `in_flight` via CAS false→true.
        //      On failure (a task is already running), the caller returns
        //      immediately — the in-flight task will see `dirty` when it
        //      finishes its current snapshot and loop once more.
        //   3. The winning caller spawns the persist task, which loops:
        //      clear `dirty`, snapshot+save, then check `dirty` again.
        //      When `dirty` is still false after a snapshot, release
        //      `in_flight` and exit.
        self.persist_state.dirty.store(true, Ordering::Release);
        if self
            .persist_state
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            // Another task is already running and will pick up the new state
            // via the `dirty` flag we just set.
            return;
        }

        let index_id = self.index_id.clone();
        let root_path = self.root_path.clone();
        let store = self.store.clone();
        let chunks = self.chunks.clone();
        let entities = self.entities.clone();
        let persist_state = self.persist_state.clone();
        // Issue #28: when a redb `CorpusStore` is wired, the chunk corpus is
        // already persisted transactionally per-batch by `commit_corpus_to_redb`.
        // Skipping the full-rewrite `chunks.json` snapshot here eliminates the
        // memory-explosion path entirely (the ~1× corpus clone + JSON encode
        // documented in `PersistState`). Only the HNSW snapshot still needs the
        // background task. `false` (no corpus store) keeps the legacy JSON
        // behaviour for test / BM25-only indexers.
        let persist_chunks_json = self.corpus.is_none();
        tokio::spawn(async move {
            // Issue #403: route HNSW path to colocated or legacy storage.
            let is_colocated = crate::service::colocated_storage::has_colocated_storage(&root_path);
            // Re-resolve paths in the task so the persistence layer's path
            // resolution failures don't crash the commit caller. The chunks
            // JSON path is only needed in the legacy (no redb) mode.
            let chunks_path = if persist_chunks_json {
                match crate::service::persistence::chunks_path(&index_id) {
                    Ok(p) => Some(p),
                    Err(e) => {
                        tracing::debug!(
                            "incremental persist: cannot resolve chunks path for '{index_id}': {e}"
                        );
                        persist_state.in_flight.store(false, Ordering::Release);
                        return;
                    }
                }
            } else {
                None
            };
            let hnsw_path = if is_colocated {
                match crate::service::colocated_storage::colocated_hnsw_path(&root_path) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::debug!(
                            "incremental persist: cannot resolve colocated hnsw path for '{index_id}': {e}"
                        );
                        persist_state.in_flight.store(false, Ordering::Release);
                        return;
                    }
                }
            } else {
                match crate::service::persistence::hnsw_path(&index_id) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::debug!(
                            "incremental persist: cannot resolve hnsw path for '{index_id}': {e}"
                        );
                        persist_state.in_flight.store(false, Ordering::Release);
                        return;
                    }
                }
            };

            // Coalescing loop: snapshot+save while `dirty` keeps being set.
            // Bound the loop so a pathological caller can't pin us forever
            // (each iteration is bounded by I/O latency, but we also cap at
            // a small constant to ensure forward progress on the reindex
            // hot loop's behalf).
            const MAX_COALESCED_ITERATIONS: u32 = 8;
            for _ in 0..MAX_COALESCED_ITERATIONS {
                // Clear `dirty` *before* snapshotting so any commit that
                // races in after we start reading is guaranteed to set it
                // again — ensuring we don't miss it.
                persist_state.dirty.store(false, Ordering::Release);

                // Save HNSW first (large, parallel-friendly).
                if let Some(store) = &store {
                    if let Err(e) = store.save_to(&hnsw_path).await {
                        tracing::warn!(
                            "incremental persist: failed to save HNSW for '{index_id}': {e}"
                        );
                    }
                }

                // Legacy chunks.json snapshot — only when no redb corpus
                // store is wired (issue #28). Snapshot chunks + entities under
                // read locks, scoped tightly so the `Vec<RawChunk>` is dropped
                // before the next loop iteration; `serde_json::to_vec` runs
                // inside a `spawn_blocking` so the hundreds-of-MB JSON build
                // doesn't block a runtime worker thread.
                if let Some(chunks_path) = &chunks_path {
                    let chunks_vec: Vec<RawChunk> = {
                        let g = chunks.read().await;
                        g.values().cloned().collect()
                    };
                    let entities_vec: Vec<(String, Vec<RawEntity>)> = {
                        let g = entities.read().await;
                        g.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                    };
                    let snapshot = ChunkSnapshot {
                        version: 1,
                        chunks: chunks_vec,
                        entities: entities_vec,
                    };
                    if let Some(parent) = chunks_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let tmp = chunks_path.with_extension("json.tmp");
                    let chunks_path_inner = chunks_path.clone();
                    let index_id_inner = index_id.clone();
                    // Serialize + write on a blocking worker so we don't pin a
                    // runtime worker for hundreds of ms on large corpora.
                    let join = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                        let bytes = match serde_json::to_vec(&snapshot) {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::warn!(
                                    "incremental persist: serialize chunks failed for \
                                     '{index_id_inner}': {e}"
                                );
                                return Ok(()); // non-fatal
                            }
                        };
                        std::fs::write(&tmp, &bytes)?;
                        std::fs::rename(&tmp, &chunks_path_inner)?;
                        Ok(())
                    })
                    .await;
                    match join {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            tracing::warn!("incremental persist: I/O failed for '{index_id}': {e}")
                        }
                        Err(e) => tracing::warn!(
                            "incremental persist: blocking task panicked for '{index_id}': {e}"
                        ),
                    }
                }

                // If no new commits arrived during the snapshot, we're
                // done. Release in_flight under Release ordering so the
                // next caller's CAS sees the cleared state.
                if !persist_state.dirty.load(Ordering::Acquire) {
                    persist_state.in_flight.store(false, Ordering::Release);
                    return;
                }
                // Otherwise loop: another commit landed while we were
                // saving, so its state needs flushing too.
            }
            // Hit the iteration cap. Drop in_flight so future commits can
            // start a fresh persist; we logged a debug above per iteration.
            tracing::debug!(
                "incremental persist: coalesce cap reached for '{index_id}' \
                 (more commits arriving than we can flush)"
            );
            persist_state.in_flight.store(false, Ordering::Release);
        });
    }
}
