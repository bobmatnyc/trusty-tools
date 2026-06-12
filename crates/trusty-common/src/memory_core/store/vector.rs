//! Vector store trait and HNSW implementation backed by redb (issue #51).
//!
//! Why: Issue #51 — the previous backend (`usearch`) pulled a C++ FFI build
//! dependency into every consumer. This module now exposes the same
//! `UsearchStore` type name (preserved as the public contract used by
//! `PalaceHandle`, dream, retrieval, and the `TrustyBackedMemoryStore`
//! adapter), but the internals are pure-Rust: an `HnswStore` (issue #50)
//! that persists raw vectors in a redb file. The type name is kept for
//! backward compatibility while the rest of the codebase still references
//! `UsearchStore`; downstream renames can happen incrementally.
//! What: `VectorStore` async trait + `UsearchStore` wrapping `HnswStore`.
//! `upsert`/`search`/`remove` run on `tokio::task::spawn_blocking` so the
//! sync `HnswStore` API doesn't stall the async reactor. UUIDs are
//! converted to/from strings at the boundary (HnswStore keys are strings).
//! On `new()`, if a legacy `<path>` usearch index file is present (and the
//! `usearch-migrate` feature is compiled in), its contents are drained
//! into the redb-backed HNSW index and the legacy file is renamed to
//! `<path>.migrated`.
//! Test: `upsert` then `search` returns the inserted id at rank 0 with
//! score at least 0.99 for an identical query vector; `remove` then
//! `search` no longer returns the removed id; reopening the store from
//! the same path retrieves previously inserted vectors.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use anyhow::{Context, Result};
use async_trait::async_trait;
use redb::Database;
use uuid::Uuid;

use crate::memory_core::store::concurrent_open::{OpenMode, SnapshotGuard, try_open_or_snapshot};
use crate::memory_core::store::hnsw_store::HnswStore;

/// Bundle of state shared between every `UsearchStore` clone that points
/// at the same canonical path.
///
/// Why: When the live file is locked by another process (issue #59) we
/// fall back to a snapshot copy via `try_open_or_snapshot`. The
/// `SnapshotGuard` that deletes the snapshot file on drop must live for
/// as long as any handle keeps using the database — bundling guard, db,
/// and mode into one `Arc` in the cache ensures that lifetime alignment
/// across clones.
/// What: Owns the open `Database`, the open mode, and the snapshot
/// guard.
/// Test: Indirect — every `UsearchStore::new` constructs one.
#[derive(Debug)]
struct VectorDbState {
    db: Arc<Database>,
    mode: OpenMode,
    _snapshot_guard: SnapshotGuard,
}

/// Process-wide cache of `Arc<VectorDbState>` keyed by canonical path.
///
/// Why: redb takes an exclusive lock on the database file, so two
/// independent `Database::create` calls against the same path inside a
/// single process fail with a lock error. Several call paths exist
/// (e.g. `PalaceRegistry::create_palace` immediately followed by another
/// registry's `open_palace` in the same test) where the same logical
/// palace is opened twice; without a cache the second open trips the
/// lock. `KgStoreRedb` solves the same problem with the same pattern.
/// What: A `Mutex<HashMap<PathBuf, Weak<VectorDbState>>>` so dropped
/// handles fall out automatically.
/// Test: Indirectly via `trusty-memory`'s
/// `default_palace_used_when_arg_omitted` which opens the same redb
/// file twice in one process.
fn vector_db_cache() -> &'static Mutex<HashMap<PathBuf, Weak<VectorDbState>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Weak<VectorDbState>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Canonicalize a path for use as a cache key. Falls back to the raw
/// path if canonicalization fails (e.g. file does not yet exist).
fn canonical_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Return the cached `VectorDbState` for `path`, opening (and caching) a
/// fresh one if no live handle exists. The returned state carries the
/// open mode so callers can switch the HNSW store into read-only mode
/// when the live file was locked.
fn open_or_get_cached_db(path: &Path) -> Result<Arc<VectorDbState>> {
    {
        let mut cache = vector_db_cache().lock().expect("vector_db_cache poisoned");
        let key = canonical_key(path);
        if let Some(weak) = cache.get(&key)
            && let Some(state) = weak.upgrade()
        {
            return Ok(state);
        }
        cache.remove(&key);
    }

    let (db, snapshot_guard, mode) = try_open_or_snapshot(path)
        .with_context(|| format!("open vector redb at {}", path.display()))?;
    let state = Arc::new(VectorDbState {
        db,
        mode,
        _snapshot_guard: snapshot_guard,
    });
    {
        let mut cache = vector_db_cache().lock().expect("vector_db_cache poisoned");
        cache.insert(canonical_key(path), Arc::downgrade(&state));
    }
    Ok(state)
}

/// A single nearest-neighbour result.
///
/// Why: Callers ranking across L1/L2/L3 need a uniform shape that pairs a
/// drawer UUID with a normalised similarity score (1.0 = identical, 0.0 =
/// orthogonal).
/// What: Plain data — drawer id + cosine similarity score.
/// Test: See `upsert_then_search_returns_same_vector_at_rank_0`.
#[derive(Debug, Clone)]
pub struct VectorHit {
    pub drawer_id: Uuid,
    pub score: f32,
}

/// Result summary returned by `UsearchStore::compact_orphans`.
///
/// Why: CLI / MCP callers need a structured report (not just a count) so they
/// can render progress like "checked 644 vectors, removed 541 orphans (84%)"
/// without re-deriving totals from the store.
/// What: Plain data: total tracked vector ids inspected, count removed as
/// orphans, and the index size before/after compaction.
/// Test: `compact_orphans_removes_only_missing_ids` exercises the values.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompactionResult {
    pub total_checked: usize,
    pub orphans_removed: usize,
    pub index_size_before: usize,
    pub index_size_after: usize,
}

#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn upsert(&self, id: Uuid, embedding: Vec<f32>) -> Result<()>;
    async fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<VectorHit>>;
    async fn remove(&self, id: Uuid) -> Result<()>;
}

/// Suffix appended to the legacy `.usearch` index path when migration
/// completes — guarantees the migration runs exactly once per palace.
/// Retained as a constant (unused since #989 removed `usearch-migrate`) so
/// any on-disk `.migrated` marker files are still recognisable by operators.
const MIGRATED_SUFFIX: &str = ".migrated";

/// Translate the legacy `.usearch` index path into the redb file that holds
/// the new HNSW vectors. Keeping the redb file co-located (just with a
/// `.redb` extension) makes upgrade-in-place obvious to operators and keeps
/// per-palace cleanup simple ("delete the palace data dir").
fn redb_path_for(usearch_path: &Path) -> PathBuf {
    let mut s = usearch_path.as_os_str().to_owned();
    s.push(".redb");
    PathBuf::from(s)
}

/// HNSW-backed vector store with the `UsearchStore` public API.
///
/// Why: `PalaceHandle` and several test helpers reference `UsearchStore`
/// directly. Renaming everywhere would balloon the diff for issue #51;
/// keeping the name with new internals isolates the swap to a single file
/// while still satisfying the goal — no more C++ FFI dependency on the
/// hot vector-search path.
/// What: Owns an `Arc<Database>` (redb), an `Arc<HnswStore>` (the in-memory
/// HNSW graph + redb-persisted vectors), the on-disk path of the
/// _logical_ index (the legacy `.usearch` path, retained for diagnostics
/// and migration), and the embedding dimension.
/// Test: See module tests covering insert+search, remove, reload, and
/// orphan compaction.
pub struct UsearchStore {
    /// Path to the legacy `.usearch` file. We never create this file
    /// ourselves any more — it only exists when migrating an old palace.
    /// Kept on the struct for diagnostics and so `compact_orphans` /
    /// `reset` can report a meaningful location.
    path: PathBuf,
    dim: usize,
    inner: Arc<HnswStore>,
    /// Held to keep the redb handle (and its snapshot guard, if any) alive
    /// for the lifetime of the store. `HnswStore` already holds its own
    /// `Arc<Database>` clone, so this slot is effectively a second handle
    /// to the same state — its job is to extend the snapshot guard's
    /// lifetime across the full store lifetime.
    #[allow(dead_code)]
    db_state: Arc<VectorDbState>,
}

impl UsearchStore {
    /// Open or create an HNSW index for `dim`-dimensional f32 vectors.
    ///
    /// Why: Production palaces previously called
    /// `UsearchStore::new(<data_dir>/index.usearch, 384)`. We preserve
    /// that signature so call sites need not change. The legacy `.usearch`
    /// file (if present) is drained into the redb HNSW index on first
    /// open and renamed to `<path>.migrated` so the migration is exactly
    /// once.
    /// What: Translates `path` to `<path>.redb`, opens (or creates) the
    /// redb file, opens an `HnswStore` against it, and runs the one-shot
    /// migration if the legacy file is present and the `usearch-migrate`
    /// feature is compiled in.
    /// Test: `persist_and_reload` exercises the open path twice on the
    /// same logical path.
    pub fn new(path: PathBuf, dim: usize) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent dir for vector store: {parent:?}")
            })?;
        }

        let redb_path = redb_path_for(&path);
        let db_state = open_or_get_cached_db(&redb_path)
            .with_context(|| format!("open vector redb at {}", redb_path.display()))?;
        let read_only = db_state.mode.is_read_only();
        let inner = HnswStore::open_with_mode(db_state.db.clone(), dim, read_only)
            .with_context(|| format!("open HnswStore at {}", redb_path.display()))?;
        let inner = Arc::new(inner);

        // One-shot migration from the legacy `.usearch` file, if any. The
        // closure is split out so the feature gate stays isolated. Skip
        // migration entirely when the store is read-only — writes against
        // a snapshot would not reach the live file and would corrupt the
        // stdio session's notion of progress.
        if !read_only {
            migrate_legacy_usearch_if_present(&path, &inner, dim)
                .with_context(|| format!("migrate legacy usearch index at {}", path.display()))?;
        }

        Ok(Self {
            path,
            dim,
            inner,
            db_state,
        })
    }

    /// Whether this store rejects writes because the underlying redb file
    /// was locked by another process at open time.
    ///
    /// Why: Issue #59 — `PalaceHandle::is_read_only` builds on this so
    /// every higher-level write surface (MCP tools, dream cycle) can
    /// short-circuit with a clear error.
    /// What: Delegates to `HnswStore::is_read_only`.
    /// Test: `vector_writes_rejected_on_snapshot`.
    pub fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    /// Number of live vectors currently in the index.
    ///
    /// Why: Cold-start diagnostics compare the HNSW size to the drawer table
    /// size to surface orphaned vectors.
    /// What: Delegates to `HnswStore::len`; falls back to `0` on the
    /// (theoretically impossible) redb error so callers don't have to
    /// thread a `Result` through purely informational call sites.
    /// Test: Indirectly via `PalaceHandle::open` warnings.
    pub fn index_size(&self) -> usize {
        self.inner.len().unwrap_or(0)
    }

    /// Reset the HNSW index to an empty state and discard all vectors.
    ///
    /// Why: When the index has accumulated orphans we cannot address by
    /// drawer id alone, the cheapest remediation is to rebuild from the
    /// authoritative drawer table. This method clears the index so the
    /// caller can re-upsert from drawers.
    /// What: Replaces the inner `HnswStore` with a fresh one against the
    /// same redb path after truncating the redb file (`File::create` over
    /// the existing path). The previous redb file's tables (vectors,
    /// vector_keys, deleted_vectors) are wiped; the in-memory HNSW graph
    /// is rebuilt empty.
    /// Test: Indirectly via `dream_cycle_compacts_orphaned_vectors`.
    pub fn reset(&self) -> Result<()> {
        // Truncate the redb file by re-creating it. We have to drop our
        // own handle first so the OS releases it on platforms that lock
        // the file. The trick: build a parking lot around `inner`. To
        // keep this simple and crash-safe, we instead just iterate every
        // mapped id and delete it via the HnswStore API, then run a
        // compaction. This is slower than recreating the file but doesn't
        // need to fight redb's locking semantics.
        let ids: Vec<String> = self.inner.all_keys().context("snapshot keys for reset")?;
        for uuid_str in ids {
            // `delete` is idempotent — already-deleted ids return Ok(false).
            let _ = self
                .inner
                .delete(&uuid_str)
                .with_context(|| format!("reset: delete vector {uuid_str}"))?;
        }
        // Reclaim the rows physically so the next open hydrates an empty
        // graph (otherwise the tombstoned rows stay until compaction).
        let _ = self
            .inner
            .compact_orphans()
            .context("reset: compact orphans after wipe")?;
        Ok(())
    }

    /// Snapshot of every drawer id currently tracked by this store.
    ///
    /// Why: The dream compaction pass needs to enumerate vector entries so
    /// it can detect orphans (vectors with no surviving drawer row) and
    /// remove them. Unlike `UsearchStore`'s usearch-era implementation —
    /// which depended on a session-only `key_map` populated by upserts —
    /// the redb-backed `HnswStore` can enumerate persisted keys directly.
    /// What: Reads the `VECTOR_KEYS` table via `HnswStore::all_keys` and
    /// parses each string back into a `Uuid`. Skips (and logs) any row
    /// that fails to parse so a single corrupt entry doesn't make the
    /// whole list unreadable.
    /// Test: `dream_cycle_compacts_orphaned_vectors` exercises this path.
    pub fn all_ids(&self) -> Vec<Uuid> {
        match self.inner.all_keys() {
            Ok(keys) => keys
                .into_iter()
                .filter_map(|s| match Uuid::parse_str(&s) {
                    Ok(u) => Some(u),
                    Err(e) => {
                        tracing::warn!(key = %s, "all_ids: skipping unparseable uuid: {e}");
                        None
                    }
                })
                .collect(),
            Err(e) => {
                tracing::warn!("all_ids: redb scan failed: {e}");
                Vec::new()
            }
        }
    }

    /// Remove vector entries whose drawer IDs are not in `valid_ids`.
    ///
    /// Why: Issue #49 — over a palace's lifetime, vectors get orphaned by
    /// partial writes, schema migrations, or older bugs that dropped drawer
    /// rows without removing the corresponding HNSW entry.
    /// What: Snapshots the persisted vector keys, marks any UUID not in
    /// `valid_ids` as deleted, then physically compacts the redb store.
    /// Returns a `CompactionResult` with the inspected count, the orphan
    /// count, and the index size before/after.
    /// Test: `compact_orphans_removes_only_missing_ids`.
    pub fn compact_orphans(&self, valid_ids: &HashSet<Uuid>) -> Result<CompactionResult> {
        let index_size_before = self.inner.len().unwrap_or(0);

        let keys = self
            .inner
            .all_keys()
            .context("compact_orphans: read vector keys")?;
        let total_checked = keys.len();
        let mut orphans_removed = 0usize;
        for key in keys {
            let drawer_id = match Uuid::parse_str(&key) {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(key = %key, "compact_orphans: unparseable uuid: {e}");
                    continue;
                }
            };
            if valid_ids.contains(&drawer_id) {
                continue;
            }
            match self.inner.delete(&key) {
                Ok(true) => orphans_removed += 1,
                Ok(false) => {} // already absent — race with another writer
                Err(e) => {
                    tracing::warn!(?drawer_id, "compact_orphans: delete failed: {e}");
                }
            }
        }
        // Physically reclaim the rows so `len()` and the next open reflect
        // the removal (otherwise tombstoned rows linger).
        let _ = self
            .inner
            .compact_orphans()
            .context("compact_orphans: physical reclaim")?;

        let index_size_after = self.inner.len().unwrap_or(0);
        Ok(CompactionResult {
            total_checked,
            orphans_removed,
            index_size_before,
            index_size_after,
        })
    }

    /// Path of the logical index (the legacy `.usearch` path); useful for
    /// diagnostics that want to print where the store lives.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Embedding dimension.
    pub fn dim(&self) -> usize {
        self.dim
    }
}

#[async_trait]
impl VectorStore for UsearchStore {
    async fn upsert(&self, id: Uuid, embedding: Vec<f32>) -> Result<()> {
        let inner = self.inner.clone();
        let key = id.to_string();
        tokio::task::spawn_blocking(move || -> Result<()> {
            inner
                .upsert(&key, &embedding)
                .with_context(|| format!("upsert vector {key}"))?;
            Ok(())
        })
        .await
        .context("upsert task panicked")??;
        Ok(())
    }

    async fn search(&self, query: &[f32], top_k: usize) -> Result<Vec<VectorHit>> {
        let inner = self.inner.clone();
        let query = query.to_vec();
        let hits = tokio::task::spawn_blocking(move || -> Result<Vec<VectorHit>> {
            let raw = inner.search(&query, top_k).context("hnsw search")?;
            let mut hits = Vec::with_capacity(raw.len());
            for (uuid_str, distance) in raw {
                let drawer_id = match Uuid::parse_str(&uuid_str) {
                    Ok(u) => u,
                    Err(e) => {
                        tracing::warn!(key = %uuid_str, "search: unparseable uuid: {e}");
                        continue;
                    }
                };
                // `hnsw_rs` returns squared cosine distance in [0, 2]. Convert
                // to a similarity score in [0, 1] using `1 - distance` and
                // clamp so callers comparing to thresholds (e.g. 0.99) get
                // clean boundaries.
                let score = (1.0_f32 - distance).clamp(0.0, 1.0);
                hits.push(VectorHit { drawer_id, score });
            }
            hits.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            Ok(hits)
        })
        .await
        .context("search task panicked")??;
        Ok(hits)
    }

    async fn remove(&self, id: Uuid) -> Result<()> {
        let inner = self.inner.clone();
        let key = id.to_string();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let _ = inner
                .delete(&key)
                .with_context(|| format!("delete vector {key}"))?;
            Ok(())
        })
        .await
        .context("remove task panicked")??;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// One-shot migration from the legacy usearch index
// ---------------------------------------------------------------------------

/// Migrate every (uuid, vector) pair from the legacy `.usearch` file into
/// the redb-backed HNSW index, then rename the legacy file so the
/// migration runs exactly once.
///
/// Why: Issue #51 — operators upgrading from a usearch-backed palace must
/// not lose their vector index. The `.usearch` file alone does not carry
/// the original UUIDs (the `usearch` C++ index keys are `u64` hashes of
/// the UUID's first 8 bytes), so we rely on the `.keymap.json` sidecar
/// that the previous `UsearchStore` wrote on every upsert. Without the
/// sidecar we cannot recover full UUIDs and we skip the migration with a
/// warning rather than corrupt the new index with zero-padded UUIDs.
/// What: When the legacy file exists and the `.migrated` marker does
/// No-op: the `usearch-migrate` feature has been removed (issue #989).
///
/// Why: All production palaces were confirmed migrated from the legacy
/// `.usearch` C++ FFI backend to the pure-Rust `hnsw_rs` + redb backend
/// before this feature was deleted. Keeping this unconditional no-op
/// preserves the call site in `new()` without requiring a cfg-gate.
/// What: Returns `Ok(())` immediately. Suppresses the unused-constant
/// warning for `MIGRATED_SUFFIX` so operators can still recognise on-disk
/// `*.migrated` marker files.
/// Test: Exercised by every test in this module (all use the default build).
fn migrate_legacy_usearch_if_present(
    _legacy_path: &Path,
    _inner: &Arc<HnswStore>,
    _dim: usize,
) -> Result<()> {
    let _ = MIGRATED_SUFFIX;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn unit_vec(dim: usize, seed: u32) -> Vec<f32> {
        let raw: Vec<f32> = (0..dim).map(|i| ((i as u32 + seed) as f32) + 1.0).collect();
        let norm: f32 = raw.iter().map(|x| x * x).sum::<f32>().sqrt();
        raw.into_iter().map(|x| x / norm).collect()
    }

    #[tokio::test]
    async fn upsert_then_search_returns_same_vector_at_rank_0() {
        let dir = tempdir().unwrap();
        let store = UsearchStore::new(dir.path().join("test.usearch"), 384).unwrap();
        let id = Uuid::new_v4();
        let v = unit_vec(384, 0);

        store.upsert(id, v.clone()).await.unwrap();
        let hits = store.search(&v, 1).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].drawer_id, id);
        assert!(hits[0].score >= 0.99, "score was {}", hits[0].score);
    }

    #[tokio::test]
    async fn remove_clears_vector() {
        let dir = tempdir().unwrap();
        let store = UsearchStore::new(dir.path().join("test.usearch"), 384).unwrap();
        let id = Uuid::new_v4();
        let v = unit_vec(384, 7);
        store.upsert(id, v.clone()).await.unwrap();
        store.remove(id).await.unwrap();

        let hits = store.search(&v, 5).await.unwrap();
        assert!(
            !hits.iter().any(|h| h.drawer_id == id),
            "removed id still present in results"
        );
    }

    #[tokio::test]
    async fn persist_and_reload() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.usearch");
        let id = Uuid::new_v4();
        let v = unit_vec(384, 13);
        {
            let store = UsearchStore::new(path.clone(), 384).unwrap();
            store.upsert(id, v.clone()).await.unwrap();
        }
        let store2 = UsearchStore::new(path, 384).unwrap();
        let hits = store2.search(&v, 1).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].drawer_id, id);
        assert!(hits[0].score >= 0.99, "score was {}", hits[0].score);
    }

    /// Why: Issue #51 — `compact_orphans` must remove only the vectors
    /// whose drawer UUIDs are absent from the supplied valid set, and must
    /// persist the change so a subsequent reload doesn't resurrect the
    /// orphans.
    /// What: Insert three vectors, mark one as valid, run compaction,
    /// then assert (a) total_checked counts all three, (b) two were
    /// removed, and (c) reopening the store from disk shows only the
    /// kept vector.
    /// Test: This test itself is the verification.
    #[tokio::test]
    async fn compact_orphans_removes_only_missing_ids() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.usearch");
        let store = UsearchStore::new(path.clone(), 384).unwrap();

        let keep = Uuid::new_v4();
        let drop_a = Uuid::new_v4();
        let drop_b = Uuid::new_v4();
        store.upsert(keep, unit_vec(384, 1)).await.unwrap();
        store.upsert(drop_a, unit_vec(384, 2)).await.unwrap();
        store.upsert(drop_b, unit_vec(384, 3)).await.unwrap();

        let mut valid = HashSet::new();
        valid.insert(keep);
        let res = store.compact_orphans(&valid).unwrap();
        assert_eq!(res.total_checked, 3);
        assert_eq!(res.orphans_removed, 2);
        assert_eq!(res.index_size_before, 3);
        assert_eq!(res.index_size_after, 1);

        // Reopen from disk — the compacted state must survive.
        drop(store);
        let reopened = UsearchStore::new(path, 384).unwrap();
        let ids = reopened.all_ids();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], keep);
    }

    /// Why: Search results must round-trip the full UUID (not a truncated
    /// or zero-padded form), so dedup across L1/L2 doesn't silently fail.
    /// What: Upsert a vector under a fresh `Uuid::new_v4`, search for it,
    /// and assert the returned `drawer_id` matches the input bit-for-bit.
    /// Test: This test itself is the verification.
    #[tokio::test]
    async fn upsert_then_l1_l2_no_duplicate() {
        let dir = tempdir().unwrap();
        let store = UsearchStore::new(dir.path().join("test.usearch"), 384).unwrap();
        let id = Uuid::new_v4();
        let v = unit_vec(384, 42);

        store.upsert(id, v.clone()).await.unwrap();
        let hits = store.search(&v, 1).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].drawer_id, id,
            "search must return the full original UUID"
        );
    }

    /// Why: `reset` must wipe the index so the next search returns
    /// nothing — the dream cycle relies on this to safely rebuild from
    /// drawers.
    /// What: Insert two vectors, reset, then search; expect an empty
    /// result.
    /// Test: This test itself is the verification.
    #[tokio::test]
    async fn reset_clears_index() {
        let dir = tempdir().unwrap();
        let store = UsearchStore::new(dir.path().join("test.usearch"), 384).unwrap();
        store
            .upsert(Uuid::new_v4(), unit_vec(384, 1))
            .await
            .unwrap();
        store
            .upsert(Uuid::new_v4(), unit_vec(384, 2))
            .await
            .unwrap();
        assert!(store.index_size() >= 2);

        store.reset().unwrap();
        assert_eq!(store.index_size(), 0);

        let hits = store.search(&unit_vec(384, 1), 5).await.unwrap();
        assert!(hits.is_empty(), "search after reset should be empty");
    }

    // -- Issue #59: read-only snapshot fallback ----------------------------

    /// Why: When the redb file backing the vector index is locked by
    /// another process (the HTTP daemon), `UsearchStore::new` must fall
    /// back to a snapshot copy and report `is_read_only()`.
    /// What: Seeds the vector file with one row, holds the redb file lock
    /// via a raw `redb::Database`, then opens a second `UsearchStore`
    /// against the same logical path. Asserts read-only + that the
    /// snapshot can still serve a search hit.
    /// Test: this test.
    #[tokio::test]
    async fn vector_writes_rejected_on_snapshot() {
        let dir = tempdir().unwrap();
        let logical = dir.path().join("test.usearch");
        let id = Uuid::new_v4();
        let v = unit_vec(384, 91);

        // Populate the live file via the normal store API, then drop the
        // store so the in-process cache entry expires before we try to
        // re-acquire the redb file lock with a raw handle.
        {
            let primary = UsearchStore::new(logical.clone(), 384).unwrap();
            primary.upsert(id, v.clone()).await.unwrap();
        }

        // Hold the redb file lock with a raw `Database::create` (bypasses
        // the in-process cache). This must succeed because the previous
        // store was dropped above.
        let redb_path = redb_path_for(&logical);
        let _live = redb::Database::create(&redb_path).expect("lock vector redb");

        let snapshot =
            UsearchStore::new(logical.clone(), 384).expect("snapshot fallback must succeed");
        assert!(snapshot.is_read_only(), "snapshot must report read-only");

        // Reads still work against the snapshot.
        let hits = snapshot.search(&v, 1).await.expect("search on snapshot");
        assert!(
            !hits.is_empty(),
            "snapshot must surface vectors seeded into the live file"
        );

        // Writes against the snapshot must fail with a read-only error.
        let write = snapshot.upsert(Uuid::new_v4(), unit_vec(384, 2)).await;
        let err = write.expect_err("upsert must fail in snapshot mode");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("read-only") || msg.contains("read_only"),
            "expected read-only error, got: {msg}"
        );
    }

    /// Why: Writes that race with another process must surface a clear
    /// error without panicking — `remove` is on the same write surface as
    /// `upsert` so it must reject under the same conditions.
    /// What: Holds the redb file lock with a raw handle, opens an
    /// `UsearchStore` against the same logical path, and asserts `remove`
    /// fails.
    /// Test: this test.
    #[tokio::test]
    async fn vector_remove_rejected_on_snapshot() {
        let dir = tempdir().unwrap();
        let logical = dir.path().join("test.usearch");
        // Seed and drop so the cache entry expires.
        {
            let primary = UsearchStore::new(logical.clone(), 384).unwrap();
            primary
                .upsert(Uuid::new_v4(), unit_vec(384, 5))
                .await
                .unwrap();
        }
        let redb_path = redb_path_for(&logical);
        let _live = redb::Database::create(&redb_path).expect("lock vector redb");

        let snapshot = UsearchStore::new(logical, 384).expect("snapshot fallback");
        assert!(snapshot.is_read_only());

        let res = snapshot.remove(Uuid::new_v4()).await;
        assert!(res.is_err(), "remove must fail in snapshot mode");
    }
}
