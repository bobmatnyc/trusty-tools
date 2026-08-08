//! Machine-wide, content-addressed embedding cache (issue #5024).
//!
//! Why: the per-`IndexId` content-hash cache in `service::reindex::hash` lets a
//! *re*index skip unchanged files, but it cannot help a **new** index. A fresh
//! git worktree derives a distinct `index_id` from its canonical root
//! (`trusty_common::project_index_id`) and therefore cold-starts a full index —
//! re-embedding content its base clone embedded hours earlier. Embedding is
//! ~96.5% of a cold index's wall time, so that is nearly the whole cost, paid
//! again for bytes the machine has already seen. This cache is keyed on content
//! rather than on index identity, so the second index of identical content
//! reads vectors instead of computing them.
//!
//! What: a bounded, disk-backed store (see [`store`]) consulted inside
//! `CodeIndexer::embed_chunks_in_batches`. Every key folds in the embedder's
//! own [`Embedder::cache_identity`] alongside the content, so a model or
//! precision change misses rather than silently serving a foreign vector.
//!
//! **Every failure is non-fatal.** A cache that cannot open, read, or write
//! degrades to exactly the pre-#5024 behaviour: embed everything. Nothing in
//! this module can advance state that makes a miss unrecoverable — a lost write
//! costs one re-embed on the next reindex and nothing else.
//!
//! Test: `tests` submodule; end-to-end reuse in
//! `tests/embed_cache_reuse.rs`.
//!
//! [`Embedder::cache_identity`]: crate::core::embed::Embedder::cache_identity

mod config;
mod key;
mod store;

#[cfg(test)]
mod tests;

use std::sync::{Arc, OnceLock};

pub(crate) use key::CacheKey;
use store::EmbedCacheStore;

/// Outcome of a batch lookup: what was found, and what the caller owes back.
pub(crate) struct Lookup {
    /// One slot per input chunk — `Some` for a hit.
    pub(crate) hits: Vec<Option<Vec<f32>>>,
    /// Derived key per input chunk, so the caller can store misses without
    /// re-hashing.
    pub(crate) keys: Vec<CacheKey>,
    /// `(key, current_seq)` for every hit, to refresh its LRU position.
    pub(crate) touches: Vec<(CacheKey, u64)>,
}

impl Lookup {
    /// Count of filled slots.
    pub(crate) fn hit_count(&self) -> usize {
        self.hits.iter().filter(|h| h.is_some()).count()
    }
}

/// Handle on the process's embedding cache.
///
/// Why: wraps the blocking redb store so callers on the async reindex path get
/// an `.await`able surface that never blocks a runtime worker, and so every
/// error is absorbed here rather than propagating into a reindex that would
/// otherwise have succeeded.
/// What: `Arc<EmbedCacheStore>` plus `spawn_blocking` shims. All methods are
/// infallible from the caller's perspective — failures log and degrade.
/// Test: see `tests`.
pub(crate) struct EmbedCache {
    store: Arc<EmbedCacheStore>,
}

impl EmbedCache {
    /// The process-global cache, or `None` when one could not be built.
    ///
    /// Why: a machine-wide cache is process-global by nature — one file, opened
    /// once, shared by every index. This mirrors the existing
    /// `service::reindex::hash::file_hashes()` precedent rather than threading
    /// a handle through every `CodeIndexer`. Resolution happens once: if the
    /// database cannot be opened at startup, the answer stays `None` for the
    /// process rather than retrying (and re-warning) on every batch.
    /// What: `OnceLock<Option<Arc<EmbedCache>>>`, initialised from
    /// [`config::CacheConfig::resolve`].
    /// Test: `global_is_none_when_disabled`.
    pub(crate) fn global() -> Option<Arc<EmbedCache>> {
        static GLOBAL: OnceLock<Option<Arc<EmbedCache>>> = OnceLock::new();
        GLOBAL
            .get_or_init(|| {
                let cfg = config::CacheConfig::resolve()?;
                match EmbedCacheStore::open(&cfg.path, cfg.max_bytes, cfg.redb_cache_bytes) {
                    Ok(store) => {
                        tracing::info!(
                            path = %cfg.path.display(),
                            max_mb = cfg.max_bytes / (1024 * 1024),
                            "embed cache: enabled"
                        );
                        Some(Arc::new(EmbedCache {
                            store: Arc::new(store),
                        }))
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = %cfg.path.display(),
                            "embed cache: could not open ({e}) — embedding uncached"
                        );
                        None
                    }
                }
            })
            .clone()
    }

    /// Build a cache over an explicit path. Tests and benchmarks only.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn open_at(
        path: &std::path::Path,
        max_bytes: u64,
    ) -> Option<Arc<EmbedCache>> {
        EmbedCacheStore::open(path, max_bytes, 8 * 1024 * 1024)
            .ok()
            .map(|store| {
                Arc::new(EmbedCache {
                    store: Arc::new(store),
                })
            })
    }

    /// Look up every chunk in `contents` under `identity`.
    ///
    /// Why: batched so one redb read transaction serves a whole parse batch.
    /// What: derives a key per chunk and reports a hit only when the stored
    /// vector's length equals `dim`. A store error is logged once and reported
    /// as an all-miss `Lookup`, which is behaviourally identical to having no
    /// cache — the caller then embeds everything.
    /// Test: `lookup_reports_all_miss_on_empty_cache`,
    /// `lookup_hits_after_store`.
    pub(crate) async fn lookup(&self, identity: &str, contents: &[&str], dim: usize) -> Lookup {
        let keys: Vec<CacheKey> = contents
            .iter()
            .map(|c| CacheKey::derive(identity, c))
            .collect();
        if keys.is_empty() {
            return Lookup {
                hits: Vec::new(),
                keys,
                touches: Vec::new(),
            };
        }
        let store = Arc::clone(&self.store);
        let lookup_keys = keys.clone();
        let found = tokio::task::spawn_blocking(move || store.get_batch(&lookup_keys, dim)).await;

        let rows = match found {
            Ok(Ok(rows)) => rows,
            Ok(Err(e)) => {
                tracing::warn!("embed cache: read failed ({e}) — treating batch as uncached");
                Vec::new()
            }
            Err(e) => {
                tracing::warn!("embed cache: read task panicked ({e}) — treating batch as uncached");
                Vec::new()
            }
        };

        let mut hits = vec![None; keys.len()];
        let mut touches = Vec::new();
        for (i, row) in rows.into_iter().enumerate() {
            if let Some(hit) = row {
                touches.push((keys[i], hit.seq));
                hits[i] = Some(hit.vector);
            }
        }
        Lookup {
            hits,
            keys,
            touches,
        }
    }

    /// Persist freshly computed vectors and refresh the hits' LRU positions.
    ///
    /// Why: called *after* the caller already holds the embeddings it needs, so
    /// a failure here can never cost correctness — the vectors are returned and
    /// committed to the index either way, and the only consequence is that the
    /// next index of the same content re-embeds it.
    /// What: one write transaction covering both the inserts and the touches,
    /// then eviction down to the configured ceiling. Errors log at `warn`.
    /// Test: `store_batch_then_lookup_hits`, `store_batch_error_is_swallowed`.
    pub(crate) async fn store_batch(
        &self,
        inserts: Vec<(CacheKey, Vec<f32>)>,
        touches: Vec<(CacheKey, u64)>,
    ) {
        if inserts.is_empty() && touches.is_empty() {
            return;
        }
        let store = Arc::clone(&self.store);
        let result =
            tokio::task::spawn_blocking(move || store.write_batch(&inserts, &touches)).await;
        match result {
            Ok(Ok(evicted)) if evicted > 0 => {
                tracing::debug!("embed cache: evicted {evicted} entries to stay under ceiling");
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                tracing::warn!("embed cache: write failed ({e}) — entries not cached");
            }
            Err(e) => {
                tracing::warn!("embed cache: write task panicked ({e}) — entries not cached");
            }
        }
    }
}
