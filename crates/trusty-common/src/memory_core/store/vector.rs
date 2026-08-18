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

use crate::memory_core::store::concurrent_open::{
    OpenIntent, OpenMode, SnapshotGuard, backoff_sleep_ms, is_incompatible_format_refusal,
    try_open_or_snapshot,
};
use crate::memory_core::store::hnsw_store::HnswStore;
use crate::memory_core::store::kg_redb::READ_ONLY_ERROR_MSG;

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
///
/// Why: mirrors `KgStoreRedb::open_with_intent` with the same TOCTOU retry
/// strategy (issue #1152 / #59) and the same Writer fail-loud contract
/// (issue #1487). See those functions for the full rationale.
/// What: Cache-check → `try_open_or_snapshot(intent)`. For `ReadOnlyClient`,
/// up to 4 retries with exponential backoff (2/10/50/100 ms) absorb in-process
/// TOCTOU races and cross-process lock conflicts fall back to a read-only
/// snapshot (writes rejected via `READ_ONLY_ERROR_MSG`). For `Writer`, the
/// bounded handoff retry lives inside `try_open_or_snapshot`, so this function
/// makes a single attempt and never double-retries; a persistent conflict
/// fails loud rather than degrading to snapshot mode.
/// Test: Indirectly via `trusty-memory`'s
/// `default_palace_used_when_arg_omitted`, plus
/// `writer_intent_open_fails_loud_on_locked_vector_file`.
fn open_or_get_cached_db(path: &Path, intent: OpenIntent) -> Result<Arc<VectorDbState>> {
    const RETRIES: u8 = 4;
    const RETRY_SLEEP_MS: [u64; 4] = [2, 10, 50, 100];
    // Writer → 0 extra attempts (handoff retry lives in `try_open_or_snapshot`).
    // ReadOnlyClient → RETRIES in-process TOCTOU retries (issue #1152). The
    // `u8::from(bool)` keeps this a single non-`if` expression (fmt-stable).
    let max_attempts = RETRIES * u8::from(intent != OpenIntent::Writer);

    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..=max_attempts {
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

        // Pass the caller's intent: `ReadOnlyClient` cross-process lock
        // conflicts fall back to a read-only snapshot (issue #59), with writes
        // rejected via `READ_ONLY_ERROR_MSG`; `Writer` conflicts fail loud
        // after the bounded handoff window (issue #1487). In-process TOCTOU
        // races (async `KgWriter` abort) are resolved by the retry loop for
        // the read-only path.
        match try_open_or_snapshot(path, intent)
            .with_context(|| format!("open vector redb at {}", path.display()))
        {
            Ok((db, snapshot_guard, mode)) => {
                let state = Arc::new(VectorDbState {
                    db,
                    mode,
                    _snapshot_guard: snapshot_guard,
                });
                {
                    let mut cache = vector_db_cache().lock().expect("vector_db_cache poisoned");
                    cache.insert(canonical_key(path), Arc::downgrade(&state));
                }
                return Ok(state);
            }
            Err(e) => {
                // #4911: an incompatible on-disk format never resolves by
                // waiting, unlike the lock races this loop exists for.
                if is_incompatible_format_refusal(&e) {
                    return Err(e);
                }
                last_err = Some(e);
                if attempt < max_attempts {
                    // Exponential backoff: let any concurrent in-process
                    // `Database::drop` (or async `KgWriter` abort) finish
                    // releasing the OS file lock before retrying.
                    //
                    // `open_or_get_cached_db` is sync but may be called from
                    // async contexts (via `UsearchStore::new` from `PalaceHandle::open`).
                    // `backoff_sleep_ms` uses `block_in_place` on multi-thread
                    // Tokio runtimes so the executor can schedule other tasks
                    // while this thread waits, preventing worker-thread starvation.
                    let sleep_ms = RETRY_SLEEP_MS[attempt as usize];
                    backoff_sleep_ms(sleep_ms);
                }
            }
        }
    }
    Err(last_err.expect("at least one attempt was made"))
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

/// One raw alias scan of a palace's `VECTOR_KEYS` table.
///
/// Why (#5005): the counts and the id list answer different questions, and only
/// the counts are trustworthy. `key_rows` and `distinct_vector_ids` come
/// straight off the table — no parse, no filter, nothing that can shrink them —
/// so their difference is the number of drawers whose vector belongs to someone
/// else. The id list can be incomplete, because a key that is not a uuid names
/// no drawer. Reading "no ids" as "no collision" is what let a real collision
/// report clean.
/// What: the two authoritative counts, the drawer ids in collision groups, and
/// the keys in those groups that could not be parsed into one.
/// Test: `a_collision_whose_keys_do_not_parse_is_never_clean`.
#[derive(Debug, Clone, Default)]
pub struct AliasScan {
    /// Rows in `VECTOR_KEYS` — one per drawer with a vector.
    pub key_rows: usize,
    /// Distinct vector ids those rows point at. Below `key_rows` exactly when
    /// drawers share an id.
    pub distinct_vector_ids: usize,
    /// Drawers caught in a collision group.
    pub aliased_drawer_ids: Vec<Uuid>,
    /// Keys in a collision group that are not valid uuids, so no drawer id can
    /// be reported for them. Non-empty means `aliased_drawer_ids` is short.
    pub unnameable_keys: Vec<String>,
}

/// What one `unalias` run freed, including anything it could not name.
///
/// Why (#5005): the freed ids ARE the operator's worklist — each one is a
/// drawer that now has no vector and needs a re-embed. A key removed from
/// `VECTOR_KEYS` but dropped from the returned list because it would not parse
/// is a drawer nobody knows to repair, reported inside a success. That is the
/// count-based all-clear this ticket exists to remove, one layer down, so the
/// unparseable keys are carried rather than discarded.
/// What: `freed` is the parseable drawer ids; `unparsed_keys` is every raw key
/// that was freed and could not be read back as a `Uuid`. A non-empty
/// `unparsed_keys` means the worklist is incomplete.
/// Test: `repair_aliases_never_reports_success_over_a_partial_repair`.
#[derive(Debug, Clone, Default)]
pub struct UnaliasOutcome {
    /// Drawer ids freed by this run — the re-embed worklist.
    pub freed: Vec<Uuid>,
    /// Keys freed that are not valid uuids, so they have no drawer id.
    pub unparsed_keys: Vec<String>,
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
        Self::new_with_intent(path, dim, OpenIntent::ReadOnlyClient)
    }

    /// Open or create an HNSW index with the caller's open intent.
    ///
    /// Why (issue #1487): the HTTP daemon (sole writer) must open the vector
    /// redb with [`OpenIntent::Writer`] so a second instance fails LOUD after
    /// the bounded handoff window instead of silently serving a read-only
    /// snapshot and rejecting every `upsert`/`remove` for its lifetime.
    /// CLI / stdio / test callers keep [`OpenIntent::ReadOnlyClient`] for the
    /// snapshot read-fallback (issue #59).
    /// What: Translates `path` to `<path>.redb`, opens (or creates) the redb
    /// file with `intent`, opens an `HnswStore` against it (read-only when the
    /// snapshot fallback fired), and runs the one-shot legacy migration for
    /// read-write opens.
    /// Test: `persist_and_reload` (ReadOnlyClient default) and
    /// `writer_intent_open_fails_loud_on_locked_vector_file` (Writer path).
    pub fn new_with_intent(path: PathBuf, dim: usize, intent: OpenIntent) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent dir for vector store: {parent:?}")
            })?;
        }

        let redb_path = redb_path_for(&path);
        let db_state = open_or_get_cached_db(&redb_path, intent)
            .with_context(|| format!("open vector redb at {}", redb_path.display()))?;
        let read_only = db_state.mode.is_read_only();
        let inner = Arc::new(
            HnswStore::open_with_mode(db_state.db.clone(), dim, read_only)
                .with_context(|| format!("open HnswStore at {}", redb_path.display()))?,
        );

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

    /// Drawer ids whose vector has been overwritten by another drawer's.
    ///
    /// Why (#5005): key presence — what `embed_health` and `palace_reembed`
    /// test — cannot see an id collision, so a palace with four unretrievable
    /// drawers reported a clean bill of health. This is the comparison that
    /// does see it.
    ///
    /// This used to build its id list with `filter_map(…ok())`, which was the
    /// same fail-open shape as the one fixed in [`UsearchStore::unalias`], one
    /// layer earlier and in the detector rather than the repair: a collision
    /// group whose keys did not parse shrank to nothing, so `is_clean()`
    /// answered true over a real collision and `repair_aliases` returned
    /// `Clean` without touching it. The keys are carried now, and
    /// [`AliasAudit::is_clean`](crate::memory_core::retrieval::AliasAudit::is_clean) consults the row-vs-distinct arithmetic rather
    /// than the id list alone.
    /// What: delegates to `HnswStore::audit_aliases` and splits each collision
    /// group's keys into drawer ids and unnameable leftovers, alongside the two
    /// counts. The counts come straight from the table and no parse can affect
    /// them, which is why they are the authoritative signal.
    /// Test: `alias_audit_surfaces_a_collision`,
    /// `a_collision_whose_keys_do_not_parse_is_never_clean`.
    pub fn alias_audit(&self) -> Result<AliasScan> {
        let audit = self
            .inner
            .audit_aliases()
            .context("alias_audit: scan vector keys")?;
        let mut scan = AliasScan {
            key_rows: audit.key_rows,
            distinct_vector_ids: audit.distinct_vector_ids,
            ..Default::default()
        };
        for key in audit.aliased.iter().flat_map(|(_, uuids)| uuids.iter()) {
            match Uuid::parse_str(key) {
                Ok(u) => scan.aliased_drawer_ids.push(u),
                Err(e) => {
                    tracing::error!(
                        key = %key,
                        "#5005: a key in a collision group is not a uuid, so the drawer it \
                         covers cannot be named: {e}"
                    );
                    scan.unnameable_keys.push(key.clone());
                }
            }
        }
        Ok(scan)
    }

    /// Unmap every drawer caught in an id collision so a re-embed repairs it.
    ///
    /// Why: see [`HnswStore::unalias`] — the reachable member of a collision
    /// group is no more trustworthy than the unreachable ones, so the repair
    /// has to free the whole group.
    /// What: delegates to `HnswStore::unalias` and splits the freed raw keys
    /// into parseable drawer ids and unnameable leftovers. The freed drawers
    /// then read as ordinary "missing" to `embed_health`. Callers should route
    /// through [`PalaceHandle::repair_aliases`](crate::memory_core::PalaceHandle::repair_aliases), which adds the dry run and the
    /// post-repair verification; this is the raw primitive.
    /// Test: `unalias_marks_the_whole_group_for_reembed`,
    /// `repair_aliases_never_reports_success_over_a_partial_repair`.
    pub fn unalias(&self) -> Result<UnaliasOutcome> {
        let raw = self.inner.unalias().context("unalias: free aliased keys")?;
        let mut out = UnaliasOutcome::default();
        for key in raw {
            match Uuid::parse_str(&key) {
                Ok(u) => out.freed.push(u),
                Err(e) => {
                    tracing::error!(
                        key = %key,
                        "#5005: unalias freed a key that is not a uuid, so no drawer id \
                         can be reported for it: {e}"
                    );
                    out.unparsed_keys.push(key);
                }
            }
        }
        Ok(out)
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
        // Surface read-only errors before spawn_blocking so the anyhow context
        // chain doesn't bury the actionable sentinel message (issue #59).
        if self.is_read_only() {
            anyhow::bail!(READ_ONLY_ERROR_MSG);
        }
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
        // Surface read-only errors before spawn_blocking so the anyhow context
        // chain doesn't bury the actionable sentinel message (issue #59).
        if self.is_read_only() {
            anyhow::bail!(READ_ONLY_ERROR_MSG);
        }
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
mod tests;
