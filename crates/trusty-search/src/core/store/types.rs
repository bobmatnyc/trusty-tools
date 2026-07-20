//! Core types and the abstract `VectorStore` trait.
//!
//! Why: decouples the indexer from any specific ANN backend so we can swap
//! implementations (mocks for tests, remote services for sharding) without
//! touching call sites.
//! What: defines `VectorHit`, the `VectorStore` async trait, and the private
//! `StoreKeyMap` sidecar type used for HNSW persistence.
//! Test: see `super::tests` — all `VectorStore` behaviour is exercised
//! through `UsearchStore`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Sidecar JSON written alongside the usearch binary snapshot, capturing the
/// `chunk_id → u64 key` mapping (and the `next_key` counter) so a restored
/// index can translate HNSW matches back into chunk ids.
///
/// Why: usearch persists vectors + graph + keys, but only as `u64`s. We
/// allocate string→u64 mappings ourselves in `UsearchStore::id_to_key`, so
/// without this sidecar the loaded index would have orphaned keys.
/// What: `id_to_key` is the authoritative mapping; `next_key` is the
/// monotonic counter so post-restore inserts never collide with restored
/// keys.
/// Test: `tests::test_save_load_roundtrip` exercises this.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoreKeyMap {
    pub(super) id_to_key: HashMap<String, u64>,
    pub(super) next_key: u64,
    pub(super) dim: usize,
}

#[derive(Debug, Clone)]
pub struct VectorHit {
    pub chunk_id: String,
    pub score: f32,
}

/// Abstract vector store interface. Concrete impls (in-process HNSW today,
/// possibly remote tomorrow) plug in here so the rest of the indexer never
/// imports `usearch` directly.
///
/// Why: Decouples the indexer from any specific ANN backend so we can swap
/// implementations (mocks for tests, remote services for sharding) without
/// touching call sites.
/// What: Async upsert/search/remove/len over `(String chunk_id, Vec<f32>)`.
/// Test: See `UsearchStore` tests below — exercise upsert, search ordering,
/// remove, and len through this trait.
#[async_trait]
#[allow(clippy::len_without_is_empty)]
pub trait VectorStore: Send + Sync {
    async fn upsert(&self, id: &str, embedding: Vec<f32>) -> Result<()>;
    async fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<VectorHit>>;
    async fn remove(&self, id: &str) -> Result<()>;
    async fn len(&self) -> Result<usize>;

    /// Predicate-scoped nearest-neighbour search (issue #3401).
    ///
    /// Why: a caller-supplied path/repo filter must not lose recall to
    /// `top_k` truncation. For an approximate index that means the predicate
    /// has to be evaluated DURING traversal, not after — a chunk id ranked
    /// far outside a raw-similarity `top_k` window by cosine distance alone
    /// can still be the closest (or only) match once scoped to a repo, and no
    /// over-fetch factor can guarantee catching it in general.
    ///
    /// Takes the filter as plain data (`path_prefix` / `repos`) rather than a
    /// `dyn Fn` closure: `UsearchStore`'s override needs to call
    /// `usearch::Index::filtered_search`'s synchronous `Fn(Key) -> bool`
    /// callback from inside an `#[async_trait]` method, and a `&dyn Fn(&str)
    /// -> bool` parameter threaded through `async_trait`'s generated
    /// lifetime elision ties the callback's `&str` argument lifetime to the
    /// parameter's own — plain borrowed data sidesteps that entirely, and it
    /// is all either side actually needs (see [`path_match::matches`]).
    /// What: default implementation is a best-effort fallback for backends
    /// that cannot push a predicate into traversal — it over-fetches
    /// (`top_k * 50`, capped) via plain [`Self::search`] and filters
    /// client-side. This CAN still miss a match ranked beyond the over-fetch
    /// window; it exists only so the trait stays total for mock/test stores
    /// that never carry a real path filter. [`super::usearch_impl`]'s
    /// `UsearchStore` overrides this with genuine predicate-pushed traversal
    /// via `usearch::Index::filtered_search`, which is the only
    /// implementation this crate actually relies on for correctness.
    /// Test: `UsearchStore` — `test_filtered_search_finds_match_ranked_below_top_k`.
    async fn search_filtered(
        &self,
        query: &[f32],
        top_k: usize,
        path_prefix: Option<&str>,
        repos: &[String],
    ) -> Result<Vec<VectorHit>> {
        let overfetch = top_k.saturating_mul(50).max(top_k).min(100_000);
        let hits = self.search(query, overfetch).await?;
        let mut out = Vec::with_capacity(top_k.min(hits.len()));
        for hit in hits {
            if super::path_match::matches(hit.chunk_id.as_str(), path_prefix, repos) {
                out.push(hit);
                if out.len() >= top_k {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Bulk-upsert many `(chunk_id, embedding)` pairs.
    ///
    /// Why: per-chunk `upsert` acquires three write locks (`id_to_key`,
    /// `key_to_id`, `index`) for each call. On a 115k-chunk index that's
    /// ~345k lock round-trips and serializes the entire embed pipeline behind
    /// the HNSW write lock. Concrete impls should override to do all key
    /// allocation and all HNSW writes under a single lock acquisition each.
    /// What: default implementation loops over `upsert` so non-Usearch backends
    /// keep working; `UsearchStore` overrides for the fast path.
    /// Test: see `test_upsert_batch_inserts_all` in this module.
    async fn upsert_batch(&self, items: &[(String, Vec<f32>)]) -> Result<()> {
        for (id, vec) in items {
            self.upsert(id, vec.clone()).await?;
        }
        Ok(())
    }

    /// Persist this store to disk. Default = no-op (in-memory backends).
    ///
    /// Why: lets `CodeIndexer::save_to_disk` call through a `dyn VectorStore`
    /// without downcasting. `UsearchStore` overrides; mock test stores keep
    /// the no-op so they round-trip without filesystem access.
    /// What: persist whatever state is needed to restore via `load_from`.
    /// Test: covered by `UsearchStore::test_save_load_roundtrip`.
    async fn save_to(&self, _path: &Path) -> Result<()> {
        Ok(())
    }

    /// Rewrite the in-memory chunk-ID → u64 key mapping from absolute to
    /// root-relative paths, returning the number of keys rewritten.
    ///
    /// Why (M003 — issue #402 phase 2): M002 rewrites the redb corpus to
    /// relative paths but leaves `hnsw.keys.json` untouched. At query time
    /// vector search returns absolute HNSW chunk IDs, which are no longer
    /// present in redb (now relative), producing 0 vector results on every
    /// migrated legacy index. This method rewrites the in-memory `id_to_key`
    /// and `key_to_id` maps so subsequent searches emit relative IDs that
    /// match the redb corpus. Callers are responsible for persisting the
    /// updated sidecar via `save_to`. Default = no-op (mock / BM25-only stores).
    /// What: for each absolute ID that shares `root_path` as a prefix, strips
    /// the prefix to produce a relative ID, swaps the maps, and returns the
    /// count of rewritten entries. Already-relative IDs are left unchanged
    /// (idempotency). IDs that are absolute but outside `root_path` are left
    /// unchanged and logged at warn.
    /// Test: `test_rewrite_keys_to_relative` in `store::tests`.
    async fn rewrite_keys_to_relative(&self, _root_path: &Path) -> Result<usize> {
        Ok(0)
    }

    /// Demote a promoted-but-idle store back to mmap-view mode, reclaiming
    /// its heap-resident copy. Default = no-op (in-memory / mock backends
    /// have no view-vs-mutable distinction to demote between).
    ///
    /// Why (issue #2164): `UsearchStore` promotes its HNSW index to a heap
    /// copy on the first write (#709's `ensure_mutable`), and until this
    /// method existed there was no path back — every index ever written even
    /// once stayed heap-resident for the rest of the process lifetime. This
    /// is the counterpart demotion path, called from the same idle sweep
    /// that already evicts chunks/BM25/entities
    /// (`server::tickers::spawn_idle_chunk_eviction_ticker`).
    /// What: `UsearchStore` overrides to re-open its HNSW via `Index::view`
    /// when idle and clean (no unpersisted writes); mock/BM25-only stores
    /// keep this no-op. Returns `Ok(true)` when an actual demotion happened.
    /// Test: `UsearchStore` tests — `test_demote_to_view_full_cycle` et al.
    /// in `store::tests`.
    async fn demote_to_view(&self) -> Result<bool> {
        Ok(false)
    }

    /// Returns `true` when `id` already has a stored vector.
    ///
    /// Why (issue #2984 Phase 1 HIGH finding 3): the runtime vector-component
    /// re-enable catch-up must be genuinely incremental — the locked design
    /// decision is "incremental catch-up, never a forced full rebuild" — so
    /// `CodeIndexer::embed_deferred_chunks` needs a cheap "is this chunk
    /// already embedded?" membership test to skip content that was embedded
    /// before the vector component was disabled.
    /// What: default `false` (safe but non-incremental fallback for stores
    /// that can't answer cheaply); `UsearchStore` overrides via `id_to_key`.
    /// Prefer [`Self::contains_many`] for bulk membership checks — the
    /// default here is a single-id convenience wrapper.
    /// Test: `UsearchStore` tests — `test_contains_reports_membership`.
    async fn contains(&self, _id: &str) -> bool {
        false
    }

    /// Bulk membership check: returns which of `ids` already have a stored
    /// vector, same length/order as `ids` (issue #2984 Phase 1 HIGH finding 3).
    ///
    /// Why: a per-id `contains` call on a 100k-chunk corpus is 100k lock
    /// acquisitions; a single bulk pass avoids that.
    /// What: default implementation loops over [`Self::contains`] (correct
    /// but O(n) lock acquisitions); `UsearchStore` overrides with a single
    /// read-lock pass over `id_to_key`.
    /// Test: `UsearchStore` tests — `test_contains_many_reports_membership`.
    async fn contains_many(&self, ids: &[String]) -> Vec<bool> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.push(self.contains(id).await);
        }
        out
    }
}
