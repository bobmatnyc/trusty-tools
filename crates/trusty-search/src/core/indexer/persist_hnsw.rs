//! HNSW/vector-store persistence helpers for [`CodeIndexer`].
//!
//! Why: extracted from `persist.rs` (issue #607) to keep both files under the
//! 500-SLOC hard cap. Groups the vector-store snapshot, key-rewrite, and
//! incremental background persist logic.
//! What: `save_vector_store`, `rewrite_vector_store_keys`, `set_store`,
//! `bm25_doc_text`, `force_incremental_persist`, and `spawn_incremental_persist`.
//! Test: covered by `test_save_chunks_roundtrip`, `test_persist_coalesces_concurrent_calls`,
//! and `test_incremental_persist_throttles_to_interval` in `indexer::tests`.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;

use crate::core::chunker::RawChunk;
use crate::core::entity::RawEntity;
use crate::core::store::VectorStore;

use super::{ChunkSnapshot, CodeIndexer};

impl CodeIndexer {
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

    /// Rewrite the HNSW key map from absolute to root-relative chunk IDs, and
    /// flush the updated sidecar to disk atomically.
    ///
    /// Why (M003 — issue #402 phase 2): M002 rewrites redb chunk IDs to
    /// relative paths but leaves the HNSW `hnsw.keys.json` sidecar with
    /// absolute keys. At query time vector search returns absolute chunk IDs
    /// that `fetch_chunks_for_ids` looks up in redb — which is now relative —
    /// producing 0 vector results. This method fixes both the in-memory maps
    /// and the on-disk sidecar atomically so subsequent restarts stay correct.
    /// What: calls `VectorStore::rewrite_keys_to_relative` to update the
    /// in-memory maps, then calls `save_to(hnsw_path)` to flush the updated
    /// sidecar alongside the (unchanged) `.usearch` binary. Returns the number
    /// of entries rewritten; returns 0 when no store is wired (BM25-only) or
    /// when all keys are already relative (idempotent no-op).
    /// Test: `tests::test_rewrite_vector_store_keys_no_store_is_noop` and the
    /// M003 migration tests in `core::migration::m003::tests`.
    pub async fn rewrite_vector_store_keys(
        &self,
        hnsw_path: &std::path::Path,
        root_path: &std::path::Path,
    ) -> Result<usize> {
        let Some(store) = &self.store else {
            return Ok(0);
        };
        let count = store.rewrite_keys_to_relative(root_path).await?;
        if count > 0 {
            // Flush the updated sidecar. The `.usearch` binary is unchanged
            // (vectors are keyed by u64 labels that don't encode file paths);
            // only the JSON sidecar maps string IDs to those labels.
            store.save_to(hnsw_path).await?;
        }
        Ok(count)
    }

    /// Install a pre-loaded `VectorStore` (typically a restored `UsearchStore`)
    /// onto this indexer. Used by the warm-boot path so the persisted HNSW
    /// graph is wired in before `load_chunks_from_disk` runs.
    pub fn set_store(&mut self, store: Arc<dyn VectorStore>) {
        self.store = Some(store);
    }

    /// Compose the BM25 document text for a chunk: body + virtual_terms,
    /// matching the layout the per-query rebuild used to construct.
    pub(crate) fn bm25_doc_text(chunk: &RawChunk) -> String {
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
