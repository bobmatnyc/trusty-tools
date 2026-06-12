//! `VectorStore` trait implementation for `UsearchStore`.
//!
//! Why: separates the high-level VectorStore contract from the struct
//! definition and internal helpers so both files stay under the 500-line cap.
//! What: implements all `VectorStore` methods on `UsearchStore` — upsert,
//! search, remove, len, save_to, rewrite_keys_to_relative, and the optimised
//! bulk `upsert_batch`.
//! Test: see `super::tests`.

use std::path::Path;
use std::sync::atomic::Ordering;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use super::types::VectorHit;
use super::types::VectorStore;
use super::usearch_store::{hnsw_max_elements, validate_embedding, UsearchStore};

#[async_trait]
impl VectorStore for UsearchStore {
    async fn upsert(&self, id: &str, embedding: Vec<f32>) -> Result<()> {
        if embedding.len() != self.dim {
            return Err(anyhow!(
                "embedding dim mismatch: got {}, expected {}",
                embedding.len(),
                self.dim
            ));
        }

        // Promote view → mutable on first write. No-op when already mutable.
        self.ensure_mutable().await?;

        // Resolve or allocate the u64 key under a write lock.
        let key = {
            let mut id_to_key = self.id_to_key.write().await;
            if let Some(&existing) = id_to_key.get(id) {
                existing
            } else {
                let key = self.next_key.fetch_add(1, Ordering::Relaxed);
                id_to_key.insert(id.to_string(), key);
                self.key_to_id.write().await.insert(key, id.to_string());
                key
            }
        };

        let index = self.index.write().await;

        // If the key already existed, remove the old vector first so `add` doesn't
        // collide. usearch's `multi=false` index treats duplicate keys as errors.
        if index.contains(key) {
            index
                .remove(key)
                .map_err(|e| anyhow!("usearch remove (for upsert) failed: {e}"))?;
        }

        UsearchStore::ensure_capacity(&index)?;
        index
            .add(key, &embedding)
            .map_err(|e| anyhow!("usearch add failed: {e}"))?;
        Ok(())
    }

    async fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<VectorHit>> {
        if query.len() != self.dim {
            return Err(anyhow!(
                "query dim mismatch: got {}, expected {}",
                query.len(),
                self.dim
            ));
        }
        if top_k == 0 {
            return Ok(Vec::new());
        }

        let matches = {
            let index = self.index.read().await;
            index
                .search(query, top_k)
                .map_err(|e| anyhow!("usearch search failed: {e}"))?
        };

        let key_to_id = self.key_to_id.read().await;
        let mut hits = Vec::with_capacity(matches.keys.len());
        for (key, dist) in matches.keys.iter().zip(matches.distances.iter()) {
            if let Some(chunk_id) = key_to_id.get(key) {
                // Cosine distance ∈ [0, 2]; convert to similarity ∈ [-1, 1] so callers
                // can RRF/fuse with BM25 scores where "higher = better".
                let score = 1.0 - *dist;
                hits.push(VectorHit {
                    chunk_id: chunk_id.clone(),
                    score,
                });
            }
            // Silently skip orphaned keys (e.g. removed mid-search) — the alternative
            // of erroring would tear down a valid query for a benign race.
        }
        Ok(hits)
    }

    async fn remove(&self, id: &str) -> Result<()> {
        // Promote view → mutable on first write. No-op when already mutable.
        self.ensure_mutable().await?;
        let key = {
            let mut id_to_key = self.id_to_key.write().await;
            match id_to_key.remove(id) {
                Some(k) => k,
                None => return Ok(()), // idempotent: removing an unknown id is a no-op
            }
        };
        self.key_to_id.write().await.remove(&key);

        let index = self.index.write().await;
        if index.contains(key) {
            index
                .remove(key)
                .map_err(|e| anyhow!("usearch remove failed: {e}"))?;
        }
        Ok(())
    }

    async fn len(&self) -> Result<usize> {
        Ok(self.index.read().await.size())
    }

    async fn save_to(&self, path: &Path) -> Result<()> {
        self.save(path).await
    }

    /// Rewrite in-memory `id_to_key` / `key_to_id` maps from absolute to
    /// root-relative chunk IDs. Returns the count of entries rewritten.
    ///
    /// Why: M003 needs to fix the HNSW key maps after M002 relativized redb.
    /// See [`VectorStore::rewrite_keys_to_relative`] for the full rationale.
    /// What: under a single write-lock pair on `id_to_key` and `key_to_id`,
    /// iterates every entry whose string key is absolute and shares `root_path`
    /// as a prefix, strips the prefix (mirrors M002's `strip_prefix` logic),
    /// replaces the entry in both maps. Idempotent: already-relative IDs are
    /// skipped. Outside-root absolute IDs are left unchanged and logged at warn.
    /// Test: `tests::test_rewrite_keys_to_relative`.
    async fn rewrite_keys_to_relative(&self, root_path: &Path) -> Result<usize> {
        let mut id_map = self.id_to_key.write().await;
        let mut key_map = self.key_to_id.write().await;

        // Collect rewrites first to avoid mutating the map while iterating.
        // Each entry: (old_absolute_id, new_relative_id, u64_key).
        let mut rewrites: Vec<(String, String, u64)> = Vec::new();
        // Compute root prefix string once, outside the loop.
        let root_prefix = root_path.to_string_lossy();

        for (id, &key) in id_map.iter() {
            if !std::path::Path::new(id).is_absolute() {
                // Already relative — idempotency: leave unchanged.
                continue;
            }
            // Try to strip root_path from the absolute chunk ID. Chunk IDs
            // have the format "{file_path}:{start}:{end}". On POSIX, ':'
            // is a valid path character so `Path::strip_prefix` treats the
            // entire ID string as a single path and strips the prefix
            // correctly. We then do a raw string prefix-swap (instead of
            // trusting the Path result's `to_string_lossy`) to preserve the
            // exact ":{start}:{end}" suffix bytes without re-encoding.
            match std::path::Path::new(id.as_str()).strip_prefix(root_path) {
                Ok(_) => {
                    // ID is under root_path. Compute the relative ID by
                    // stripping the root prefix as a raw string and trimming
                    // the leading separator.
                    let new_id = id
                        .strip_prefix(root_prefix.as_ref())
                        .map(|s| s.trim_start_matches('/').to_string())
                        .unwrap_or_else(|| id.clone());
                    rewrites.push((id.clone(), new_id, key));
                }
                Err(_) => {
                    tracing::warn!(
                        id = %id,
                        root = %root_path.display(),
                        "M003: HNSW key is absolute but not under root_path; skipping"
                    );
                }
            }
        }

        let count = rewrites.len();
        for (old_id, new_id, key) in rewrites {
            id_map.remove(&old_id);
            id_map.insert(new_id.clone(), key);
            key_map.insert(key, new_id);
        }

        // The in-memory maps now differ from the on-disk sidecar: clear the
        // view flag so `save()` does not treat the snapshot as clean and skip
        // the flush. We use `Release` ordering to pair with the `Acquire` load
        // in `save()` — the updated map state must be visible before save reads it.
        if count > 0 {
            self.is_view.store(false, Ordering::Release);
        }

        Ok(count)
    }

    /// Bulk-upsert override that minimises the time the HNSW write lock is held.
    ///
    /// Why: per-vector `upsert` acquires three write locks (`id_to_key`,
    /// `key_to_id`, `index`) for each call, and the original batch path held
    /// the HNSW write lock while calling `index.contains()` on every key —
    /// blocking concurrent searches for the entire batch. For a 640-vector
    /// batch on a hot daemon that was ~640 sequential C FFI calls (plus the
    /// pre-existence probe) under exclusive lock, which serialised all
    /// concurrent queries behind the indexer. usearch 2.25's Rust API exposes
    /// no multi-vector batch insert, so the wins come from (a) doing the
    /// `contains` probe under a read lock so the write lock only does work,
    /// and (b) decoupling the id-map locks from the HNSW write lock so the
    /// hot loop never touches `id_to_key`.
    /// What: four phases —
    /// 1. Validate dims (no locks). 2. Allocate keys for any new IDs under a
    /// single id-map write-lock pair, then drop those locks. 3. Snapshot
    /// `(key, &embedding)` pairs and pre-compute the existing-key set under a
    /// **read** lock on the HNSW index. 4. Acquire the HNSW write lock once,
    /// reserve capacity, remove pre-existing keys, then add every vector.
    /// Search results are identical to the previous implementation because
    /// the same `(key, vector)` pairs are inserted in the same order; only
    /// lock-hold duration changes.
    ///
    /// Per-item error isolation (issue #128): a single bad embedding — most
    /// commonly a NaN or all-zero vector emitted by the CoreML execution
    /// provider — used to make `index.add` fail and abort the whole call,
    /// silently dropping every other vector in a 128-file batch. Phase 4 now
    /// isolates failures: each `add` is attempted independently, the bad
    /// chunk id is logged at `warn`, and the offending item's key map entry
    /// is rolled back so it isn't left orphaned. The remaining vectors are
    /// committed normally. The call only returns `Err` when **every** add
    /// failed, which indicates a systemic problem (corrupt index, dim drift)
    /// rather than one stray vector.
    /// Test: `tests::test_upsert_and_search`, `test_upsert_replaces_existing`,
    /// `test_concurrent_reads`, and `test_upsert_batch_isolates_bad_vector`
    /// cover ordering, idempotent overwrite, reader parallelism, and the
    /// per-item isolation path.
    async fn upsert_batch(&self, items: &[(String, Vec<f32>)]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        // Promote view → mutable on first write. No-op when already mutable.
        // Done before any other work so we never half-commit against a view.
        self.ensure_mutable().await?;
        // Phase 1: validate dims up front so we don't half-commit on a bad batch.
        for (_, v) in items {
            if v.len() != self.dim {
                return Err(anyhow!(
                    "embedding dim mismatch: got {}, expected {}",
                    v.len(),
                    self.dim
                ));
            }
        }

        // Phase 2: assign keys for any new IDs under a single id-map write-lock
        // pair, then drop the locks before touching the HNSW index. Snapshot the
        // resolved keys so phases 3 and 4 don't re-acquire `id_to_key`.
        let resolved_keys: Vec<u64> = {
            let mut id_map = self.id_to_key.write().await;
            let mut key_map = self.key_to_id.write().await;
            let mut out = Vec::with_capacity(items.len());
            for (id, _) in items {
                let key = if let Some(&k) = id_map.get(id.as_str()) {
                    k
                } else {
                    let k = self.next_key.fetch_add(1, Ordering::Relaxed);
                    id_map.insert(id.clone(), k);
                    key_map.insert(k, id.clone());
                    k
                };
                out.push(key);
            }
            out
        };

        // Phase 3: under a READ lock, determine which keys already exist in the
        // HNSW so the write lock only has to do the actual mutation. Concurrent
        // searches can still run during this probe.
        let existing: std::collections::HashSet<u64> = {
            let index = self.index.read().await;
            resolved_keys
                .iter()
                .copied()
                .filter(|k| index.contains(*k))
                .collect()
        };

        // Phase 4: acquire the HNSW write lock once. Reserve capacity, remove
        // pre-existing keys, then add every vector. The write lock now only
        // does the work that actually requires exclusive access — no
        // `contains` probes inside the hot loop.
        let index = self.index.write().await;
        let max_elem = hnsw_max_elements();
        if index.size() >= max_elem {
            return Err(anyhow!(
                "usearch index at TRUSTY_MAX_CHUNKS cap ({} elements) — refusing batch upsert",
                max_elem
            ));
        }
        // Reserve once for the worst case (every item is new). `existing.len()`
        // items will be removed first, but reserving for the full batch size
        // is a safe upper bound and avoids re-entering reserve mid-loop.
        let want = index.size().saturating_add(items.len());
        if want > index.capacity() {
            let mut new_cap = index.capacity().max(1);
            while new_cap < want {
                new_cap = new_cap.saturating_mul(2);
            }
            if new_cap > max_elem {
                new_cap = max_elem;
            }
            index
                .reserve(new_cap)
                .map_err(|e| anyhow!("usearch reserve grow failed: {e}"))?;
        }
        for &key in &existing {
            index
                .remove(key)
                .map_err(|e| anyhow!("usearch remove (for upsert) failed: {e}"))?;
        }

        // Per-item error isolation (issue #128). Each vector is screened and
        // added independently: a single bad embedding (NaN / zero vector from
        // CoreML) must not abort the whole batch. We collect the chunk ids
        // of any failures, then roll their key-map entries back (below)
        // after releasing the HNSW write lock, so a failed chunk leaves no
        // orphaned `id_to_key` / `key_to_id` entry that a later search would
        // try (and fail) to resolve.
        let mut failed: Vec<(String, String)> = Vec::new();
        for (key, (id, embedding)) in resolved_keys.iter().zip(items.iter()) {
            // Screen the vector first: a NaN/zero vector is not reliably
            // rejected by usearch's `add`, but it poisons cosine search if it
            // lands in the graph. Catching it here keeps the index clean.
            if let Err(reason) = validate_embedding(embedding) {
                failed.push((id.clone(), format!("embedding {reason}")));
                continue;
            }
            if let Err(e) = index.add(*key, embedding) {
                failed.push((id.clone(), e.to_string()));
            }
        }
        // Drop the HNSW write lock before touching the id maps so we don't
        // hold two write locks at once.
        drop(index);

        if failed.is_empty() {
            return Ok(());
        }

        // Roll back the key-map entries for the failed items so they don't
        // dangle. We only remove an `id_to_key` entry when it still points at
        // the key we allocated in phase 2 — an entry that already existed
        // (and whose old vector was removed above) is left as-is rather than
        // silently deleted, since its previous state was already lost and
        // re-removing the mapping wouldn't help.
        {
            let failed_ids: std::collections::HashSet<&str> =
                failed.iter().map(|(id, _)| id.as_str()).collect();
            let mut id_map = self.id_to_key.write().await;
            let mut key_map = self.key_to_id.write().await;
            for (id, key) in resolved_keys
                .iter()
                .zip(items.iter())
                .filter(|(_, (id, _))| failed_ids.contains(id.as_str()))
                .map(|(key, (id, _))| (id, key))
            {
                if !existing.contains(key) && id_map.get(id.as_str()) == Some(key) {
                    id_map.remove(id.as_str());
                    key_map.remove(key);
                }
            }
        }

        let succeeded = items.len() - failed.len();
        for (id, err) in &failed {
            tracing::warn!(
                "usearch upsert_batch: skipped chunk '{id}' — add failed ({err}); \
                 likely a NaN or zero embedding vector. The rest of the batch was indexed."
            );
        }

        if succeeded == 0 {
            // Every add failed — this is a systemic problem (corrupt index,
            // dimension drift), not one stray vector. Surface it so the
            // reindex orchestrator can abort rather than silently produce an
            // empty index.
            return Err(anyhow!(
                "usearch upsert_batch: all {} vectors failed to add — \
                 systemic failure, not isolated bad input (first error: {})",
                items.len(),
                failed.first().map(|(_, e)| e.as_str()).unwrap_or("<none>")
            ));
        }

        tracing::warn!(
            "usearch upsert_batch: {succeeded}/{} vectors indexed; {} skipped due to \
             add failures (see warnings above)",
            items.len(),
            failed.len()
        );
        Ok(())
    }
}
