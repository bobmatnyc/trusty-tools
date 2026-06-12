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
}
