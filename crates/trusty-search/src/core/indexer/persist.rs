//! Persistence + snapshot helpers for [`CodeIndexer`].
//!
//! Why: snapshotting the chunk corpus and HNSW graph to disk is a self-contained
//! concern (issue #85). Lifting it out of the god `impl CodeIndexer` block keeps
//! the search/ingest paths focused on their own state mutation logic.
//! What: holds `save_chunks_to_disk`, `load_chunks_from_disk`, `load_chunks_from_redb`,
//! `load_or_rebuild_symbol_graph`, `refresh_live_indices_from_corpus`,
//! `migrate_corpus_to_redb`, and `flush_corpus_to_disk`.
//! The HNSW/vector-store persistence helpers (`save_vector_store`,
//! `rewrite_vector_store_keys`, `set_store`, `bm25_doc_text`,
//! `force_incremental_persist`, `spawn_incremental_persist`) live in
//! `persist_hnsw.rs`.
//! Test: covered by `test_save_chunks_roundtrip`, `test_load_chunks_missing_file_returns_zero`,
//! and `test_persist_coalesces_concurrent_calls` in `indexer::tests`.

use anyhow::{Context, Result};

use crate::core::chunker::RawChunk;
use crate::core::entity::RawEntity;

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

    /// Rebuild the in-memory BM25 index and `chunks` HashMap from scratch using
    /// the durable redb corpus as the authoritative source.
    ///
    /// Why (issue #402 / M002): after M002 rewrites chunk IDs and file paths in
    /// redb from absolute to root-relative, the live BM25 index and in-memory
    /// `chunks` map still hold the stale absolute-path keys. Any search that
    /// runs after M002 completes calls `fetch_chunks_for_ids` with the old
    /// absolute IDs, which are no longer present in redb — producing 0 results.
    /// Calling this method immediately after M002's redb write resolves the
    /// mismatch by atomically replacing both structures with the relative-path
    /// corpus.
    ///
    /// What: clears BM25 and the in-memory `chunks` map under their write locks,
    /// then replays every row from the durable corpus (same four-phase sequence
    /// as `load_chunks_from_redb`). On an empty or absent corpus the method is a
    /// safe no-op.
    ///
    /// Test: covered by `test_m002_refresh_live_indices_after_path_rewrite` in
    /// `indexer::tests`.
    pub async fn refresh_live_indices_from_corpus(&self) -> Result<usize> {
        let Some(corpus) = self.corpus.clone() else {
            return Ok(0);
        };
        // Load the updated corpus on a blocking worker.
        let (chunks, entities) = tokio::task::spawn_blocking(move || -> Result<RestoredCorpus> {
            let chunks = corpus.load_all_chunks()?;
            let entities = corpus.load_all_entities()?;
            Ok((chunks, entities))
        })
        .await
        .context("refresh_live_indices: load task panicked")??;

        let total = chunks.len();
        if total == 0 {
            return Ok(0);
        }

        // Phase 1: atomically replace BM25. Clear first so stale absolute-path
        // postings cannot linger alongside the new relative-path ones.
        {
            let mut bm25 = self.bm25.write().await;
            *bm25 = crate::core::bm25::Bm25Index::new();
            for chunk in &chunks {
                let text = Self::bm25_doc_text(chunk);
                bm25.upsert_document(&chunk.id, &text);
            }
        }
        // Phase 2: atomically replace the in-memory chunks map. Draining and
        // re-inserting under a single write lock keeps concurrent readers from
        // observing a half-replaced map.
        {
            let mut corpus_map = self.chunks.write().await;
            corpus_map.clear();
            for chunk in chunks {
                corpus_map.insert(chunk.id.clone(), chunk);
            }
        }
        // Phase 3: replace entities.
        {
            let mut emap = self.entities.write().await;
            emap.clear();
            for (file, ents) in entities {
                emap.insert(file, ents);
            }
        }
        // Phase 4: the chunk map was just repopulated (not evicted), so clear
        // the eviction flag so ensure_chunks_loaded doesn't overwrite our work
        // with a stale redb read on the next query.
        self.chunks_evicted
            .store(false, std::sync::atomic::Ordering::Release);
        tracing::info!(
            "index '{}': refreshed live BM25 + chunks from corpus ({total} chunks, \
             post-M002 relative-path sync)",
            self.index_id
        );
        Ok(total)
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
}
