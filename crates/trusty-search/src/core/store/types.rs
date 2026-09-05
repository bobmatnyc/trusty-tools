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

/// How a staged→live HNSW snapshot swap resolved (issue #6299).
///
/// Why: during a reindex every periodic checkpoint is written to a staging
/// file, and `UsearchStore::save` records the file it wrote as the store's
/// snapshot source. Resolving the swap makes that file vanish under the
/// store — renamed onto the live path, or deleted — so the store must be told
/// which of the two happened before it next tries to open the path it
/// recorded. The two outcomes need different repairs: after a commit the
/// bytes still exist under the live name, after an abort they are gone and
/// the live snapshot is the pre-reindex one.
/// What: the discriminator passed to [`VectorStore::resolve_staged_snapshot`].
/// Test: `service::reindex::hnsw_swap_tests::write_after_commit_swap_succeeds_when_store_was_demoted_to_staging_view`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagedSwapOutcome {
    /// The staging file was renamed over the live path; the bytes the store
    /// may still be mapping are now the live snapshot.
    Committed,
    /// The staging file was deleted; the live snapshot is whatever it was
    /// before the reindex started.
    Aborted,
}

/// What one scalar-precision backfill run did, or would do (issue #6822).
///
/// Why: the backfill is a durable, one-way re-encode of an index's whole vector
/// arena, so an operator has to be able to see what it will touch BEFORE it
/// runs — which index precision is current, how many vectors, how many bytes.
/// The same record serves the dry run and the applied run, so the confirmation
/// an operator reads is literally the report of the work.
/// What: a plain serialisable record. `current` is `None` only for a scalar
/// kind [`crate::core::store_config::VectorQuant`] cannot express. `applied` is
/// `false` for a dry run AND for a no-op (already at the target precision), so
/// callers distinguish the two by `dry_run`.
/// Test: `tests/vector_quant_default_6822.rs::backfill_dry_run_reports_without_writing`.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct RequantizeReport {
    /// Precision the index holds right now, e.g. `"f32 (none)"`.
    pub current: Option<&'static str>,
    /// Precision requested, e.g. `"f16"`.
    pub target: &'static str,
    /// Vectors the live index holds.
    pub vectors: usize,
    /// Keys the key map named that the graph could not return; skipped.
    pub missing: usize,
    /// On-disk snapshot bytes before the run; `None` when no snapshot exists.
    pub bytes_before: Option<u64>,
    /// On-disk snapshot bytes after the run; equals `bytes_before` when nothing
    /// was written.
    pub bytes_after: Option<u64>,
    /// `true` only when the index was actually converted and published.
    pub applied: bool,
    /// `true` when this run was a report-only dry run.
    pub dry_run: bool,
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
    /// is all either side actually needs (see `path_match::matches_chunk_id`).
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
    ///
    /// `shapes` (#6581) is the caller index's chunk-id policy — see
    /// [`crate::core::chunk_id::ChunkIdShapes`]. It reaches the predicate so a
    /// pre-M005 index still matches its own legacy named ids and a migrated one
    /// does not.
    async fn search_filtered(
        &self,
        query: &[f32],
        top_k: usize,
        path_prefix: Option<&str>,
        repos: &[String],
        shapes: crate::core::chunk_id::ChunkIdShapes,
    ) -> Result<Vec<VectorHit>> {
        let overfetch = top_k.saturating_mul(50).max(top_k).min(100_000);
        let hits = self.search(query, overfetch).await?;
        let mut out = Vec::with_capacity(top_k.min(hits.len()));
        for hit in hits {
            if super::path_match::matches_chunk_id(
                hit.chunk_id.as_str(),
                path_prefix,
                repos,
                shapes,
            ) {
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
    ///
    /// Why this is a thin wrapper (#6581): M005 rewrites the same two maps with
    /// a different mapping, and a second lock/swap implementation is exactly the
    /// kind of duplication that lets two of them drift. Both routes go through
    /// [`Self::rewrite_keys`]; only the mapping differs.
    async fn rewrite_keys_to_relative(&self, root_path: &Path) -> Result<usize> {
        self.rewrite_keys(&|id| relative_key(id, root_path)).await
    }

    /// Rewrite the in-memory chunk-ID to u64 key maps under one lock pair,
    /// returning the number of entries rewritten.
    ///
    /// Why (#6581): the `.usearch` binary is keyed by `u64` and never encodes a
    /// chunk id, so any id change — M002/M003's relativization, M005's
    /// re-chunk — is a rewrite of the JSON sidecar alone. One implementation of
    /// that lock/swap serves every such migration, and it is what lets M005 hand
    /// an existing vector to a re-chunked chunk whose text is unchanged instead
    /// of paying to embed it again.
    /// What: `remap` is called once per stored id; `Some(new_id)` replaces the
    /// entry in both maps, `None` leaves it alone (so a mapping that covers
    /// nothing is a no-op and a second pass over already-rewritten keys returns
    /// `0`). Callers persist the result with [`Self::save_to`]. Default = no-op
    /// for mock / BM25-only stores.
    /// Test: `test_rewrite_keys_applies_an_arbitrary_mapping` in `store::tests`.
    async fn rewrite_keys(
        &self,
        _remap: &(dyn for<'a> Fn(&'a str) -> Option<String> + Sync),
    ) -> Result<usize> {
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

    /// Re-point a store that recorded `staged` as its snapshot source at
    /// `live`, once a staged→live swap has resolved (issue #6299).
    ///
    /// Why: `UsearchStore::save` records the path it wrote, so every reindex
    /// checkpoint retargets the store at the staging file; an idle or
    /// memory-pressure demotion then re-views the store FROM that staging
    /// file. `service::reindex::hnsw_swap` renames or deletes it moments
    /// later, leaving the store holding a path that no longer exists — and
    /// the next write's `promote_view_to_mutable` failed `No such file or
    /// directory` on every attempt, permanently, because a failed promote
    /// never clears `is_view`. Three production indexes wedged this way.
    /// Telling the store how the swap resolved is what keeps `hnsw_path` and
    /// `is_view` truthful across it.
    /// What: default = no-op (mock / BM25-only stores record no path).
    /// `UsearchStore` overrides: a store that recorded some OTHER path is
    /// left alone; one that recorded `staged` is re-pointed at `live` after a
    /// commit, and restored from the live snapshot after an abort.
    /// Test: `UsearchStore` — `store::tests::test_resolve_staged_snapshot_*`.
    async fn resolve_staged_snapshot(
        &self,
        _staged: &Path,
        _live: &Path,
        _outcome: StagedSwapOutcome,
    ) -> Result<()> {
        Ok(())
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

    /// Scalar-precision label the LIVE store actually holds (issue #6822).
    ///
    /// Why: `GET /indexes/:id/status` must report the precision of the index
    /// that answers queries, not the `TRUSTY_VECTOR_QUANT` value the daemon
    /// happens to be running with — those differ for every index built before
    /// the #6822 default flip, which is the whole point of reporting it.
    /// What: default `None` (mock / BM25-only stores have no scalar precision);
    /// `UsearchStore` overrides by reading `Index::scalar_kind()`.
    /// Test: `tests/vector_quant_default_6822.rs::opening_an_existing_f32_snapshot_keeps_it_f32`.
    async fn vector_quant_label(&self) -> Option<&'static str> {
        None
    }

    /// Re-encode this store's vectors at `target` precision, in place
    /// (issue #6822).
    ///
    /// Why: the #6822 default flip applies at index CREATION only, so it saves
    /// nothing on an index that already exists — and a forced reindex does not
    /// help, because it upserts into the store object built at warm-boot. This
    /// is the explicit, operator-run backfill that closes that gap.
    /// What: default `Ok(None)` — "this backend has no scalar precision to
    /// convert", which a caller reports rather than treating as success.
    /// `UsearchStore` overrides via [`super::UsearchStore::requantize`]. With
    /// `dry_run` the report describes what WOULD happen and nothing is written.
    /// Test: `tests/vector_quant_default_6822.rs::backfill_converts_an_f32_index_to_f16_and_keeps_recall`.
    async fn requantize(
        &self,
        _target: crate::core::store_config::VectorQuant,
        _dry_run: bool,
    ) -> Result<Option<RequantizeReport>> {
        Ok(None)
    }
}

/// Map one absolute chunk id onto its root-relative form, or `None` to leave it.
///
/// Why (#6581): this was inlined in `UsearchStore::rewrite_keys_to_relative`
/// alongside the lock/swap it now shares with M005. Lifting it out leaves one
/// lock/swap ([`VectorStore::rewrite_keys`]) and one mapping per migration.
/// What: an already-relative id returns `None` (idempotency); an absolute id
/// under `root_path` loses that prefix as a raw string swap, preserving the
/// exact suffix bytes; an absolute id outside `root_path` returns `None` and is
/// logged at warn.
/// Test: `test_rewrite_keys_to_relative` and
/// `test_m003_skips_out_of_root_absolute_ids`.
pub fn relative_key(id: &str, root_path: &Path) -> Option<String> {
    if !Path::new(id).is_absolute() {
        return None;
    }
    // Chunk IDs are `{file_path}{suffix}`. On POSIX `:` is a valid path
    // character, so `Path::strip_prefix` treats the whole id as one path and
    // strips the root correctly; the raw string swap below then preserves the
    // suffix bytes without re-encoding them through `to_string_lossy`.
    if Path::new(id).strip_prefix(root_path).is_err() {
        tracing::warn!(
            %id,
            root = %root_path.display(),
            "HNSW key is absolute but not under root_path; skipping"
        );
        return None;
    }
    let stripped = id.strip_prefix(root_path.to_string_lossy().as_ref())?;
    Some(stripped.trim_start_matches('/').to_string())
}
