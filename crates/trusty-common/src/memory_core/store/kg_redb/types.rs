//! Shared types, state, and small pure helpers for the redb KG store.
//!
//! Why: Concentrates all of the primitive vocabulary (constants, codec
//! helpers, the db-state cache, and the public batch-op enums) in one
//! place so the larger submodules (store, read_ops, write_ops, import)
//! can stay focused on logic.
//! What: Exports `BatchWriteOp`, `BatchOpResult`, `READ_ONLY_ERROR_MSG`,
//! and the private types `KgDbState`, `Tbl`, plus free helpers used by
//! multiple submodules.
//! Test: Indirect — every submodule test exercises these types.

use crate::memory_core::palace::Drawer;
use crate::memory_core::store::kg_store::DrawerRecord;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use redb::Database;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use uuid::Uuid;

use super::super::kg::Triple;
use crate::memory_core::store::concurrent_open::{OpenMode, SnapshotGuard};
use crate::memory_core::store::kg_store::TripleValue;

/// Sentinel returned by every write method when the store is in snapshot
/// (read-only) mode.
///
/// Why: Issue #59 — a stdio MCP client that falls back to a snapshot must
/// reject writes with a clear message so the caller sees "writes go
/// through the HTTP daemon" instead of a silent divergence where the
/// write succeeds locally but never reaches the live file.
/// What: A `&'static str` so call sites can wrap it in `anyhow::anyhow!`
/// without allocating.
/// Test: `write_on_snapshot_returns_read_only_error`.
pub const READ_ONLY_ERROR_MSG: &str = "palace is read-only: HTTP daemon holds the write lock — \
     route writes through the daemon's HTTP API or stop the daemon \
     before retrying via stdio";

/// Pre-#61 on-disk shape of a drawer row (without `drawer_type` /
/// `expires_at_ms`).
///
/// Why: postcard is positional — it refuses to decode legacy rows as the
/// new `DrawerRecord` because the trailing optional fields don't exist in
/// the bytes. We try the current shape first and fall back to this struct
/// to migrate the data forward on read.
/// What: Mirrors the historical struct field-for-field; `From` lifts it
/// into the modern `DrawerRecord` with the new fields defaulted.
/// Test: `drawer_type_round_trips_through_redb` plus
/// `drawer_record_legacy_decode_without_new_fields` in `kg_store`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct LegacyDrawerRecord {
    pub room_id: String,
    pub content: String,
    pub importance: f32,
    pub tags: Vec<String>,
    pub source_file: Option<String>,
    pub created_at_ms: i64,
}

impl From<LegacyDrawerRecord> for DrawerRecord {
    fn from(l: LegacyDrawerRecord) -> Self {
        DrawerRecord {
            room_id: l.room_id,
            content: l.content,
            importance: l.importance,
            tags: l.tags,
            source_file: l.source_file,
            created_at_ms: l.created_at_ms,
            drawer_type: None,
            expires_at_ms: None,
            completed_at_ms: None,
            fact_key: None,
        }
    }
}

/// #61-era on-disk shape of a drawer row (with `drawer_type` /
/// `expires_at_ms` but without the spec-001 `completed_at_ms`).
///
/// Why: postcard is positional, so adding `completed_at_ms` to `DrawerRecord`
/// means rows written in the #61 era no longer decode as the current shape —
/// the reader would otherwise wrongly fall all the way back to
/// `LegacyDrawerRecord` and silently drop each row's `drawer_type` /
/// `expires_at`. This intermediate shape preserves those fields and only
/// defaults the new `completed_at_ms`.
/// What: mirrors the pre-spec-001 `DrawerRecord` field-for-field; `From` lifts
/// it into the modern record with `completed_at_ms = None`.
/// Test: `pre_task_drawer_row_migrates_completed_at_to_none` (decodes a
/// legacy-shaped row through this fallback) plus the round-trip coverage in
/// `drawer_completed_at_round_trips_through_redb` and
/// `drawer_type_round_trips_through_redb`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct PreTaskDrawerRecord {
    pub room_id: String,
    pub content: String,
    pub importance: f32,
    pub tags: Vec<String>,
    pub source_file: Option<String>,
    pub created_at_ms: i64,
    #[serde(default)]
    pub drawer_type: Option<String>,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
}

impl From<PreTaskDrawerRecord> for DrawerRecord {
    fn from(p: PreTaskDrawerRecord) -> Self {
        DrawerRecord {
            room_id: p.room_id,
            content: p.content,
            importance: p.importance,
            tags: p.tags,
            source_file: p.source_file,
            created_at_ms: p.created_at_ms,
            drawer_type: p.drawer_type,
            expires_at_ms: p.expires_at_ms,
            completed_at_ms: None,
            fact_key: None,
        }
    }
}

/// spec-001-era on-disk shape of a drawer row (with `completed_at_ms` but
/// without the #4884 `fact_key`).
///
/// Why: #4884 — the same positional-postcard problem `PreTaskDrawerRecord`
/// exists for. Adding `fact_key` to `DrawerRecord` means every row written
/// between spec-001 and #4884 stops decoding as the current shape; without
/// this link the reader would skip straight to `PreTaskDrawerRecord` and
/// silently drop each row's `completed_at`, marking open tasks done-unknown.
/// What: mirrors the pre-#4884 `DrawerRecord` field-for-field; `From` lifts it
/// into the modern record with `fact_key = None`.
/// Test: `pre_fact_key_drawer_row_migrates_fact_key_to_none`, plus the
/// round-trip coverage in `drawer_fact_key_round_trips_through_redb`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct PreFactKeyDrawerRecord {
    pub room_id: String,
    pub content: String,
    pub importance: f32,
    pub tags: Vec<String>,
    pub source_file: Option<String>,
    pub created_at_ms: i64,
    #[serde(default)]
    pub drawer_type: Option<String>,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    #[serde(default)]
    pub completed_at_ms: Option<i64>,
}

impl From<PreFactKeyDrawerRecord> for DrawerRecord {
    fn from(p: PreFactKeyDrawerRecord) -> Self {
        DrawerRecord {
            room_id: p.room_id,
            content: p.content,
            importance: p.importance,
            tags: p.tags,
            source_file: p.source_file,
            created_at_ms: p.created_at_ms,
            drawer_type: p.drawer_type,
            expires_at_ms: p.expires_at_ms,
            completed_at_ms: p.completed_at_ms,
            fact_key: None,
        }
    }
}

/// Decode a stored `DRAWERS` value, walking the migration chain newest→oldest.
///
/// Why: #4884 added a fourth on-disk shape, and the chain had been open-coded
/// inside `load_drawers`. The `DRAWERS_BY_FACT_KEY` index maintenance in
/// `write_ops` has to read a row's prior `fact_key` before overwriting it, so a
/// second copy of the chain would otherwise appear there — and two copies is
/// exactly how a shape gets added to one and forgotten in the other, which
/// presents as rows silently losing whichever field the stale copy predates.
/// What: tries `DrawerRecord`, then `PreFactKeyDrawerRecord` (pre-#4884), then
/// `PreTaskDrawerRecord` (#61-era), then `LegacyDrawerRecord` (pre-#61),
/// lifting each older shape forward with its missing fields defaulted. Order is
/// load-bearing: postcard rejects trailing bytes, so a newer row cannot decode
/// as an older shape, but an older row would decode as a still-older one and
/// drop fields. Returns the LAST error when every shape fails.
/// Test: `pre_fact_key_drawer_row_migrates_fact_key_to_none`,
/// `pre_task_drawer_row_migrates_completed_at_to_none`,
/// `drawer_type_round_trips_through_redb`.
pub(super) fn decode_drawer_record(bytes: &[u8]) -> Result<DrawerRecord, postcard::Error> {
    use crate::memory_core::store::kg_store::decode_value;
    if let Ok(r) = decode_value::<DrawerRecord>(bytes) {
        return Ok(r);
    }
    if let Ok(r) = decode_value::<PreFactKeyDrawerRecord>(bytes) {
        return Ok(r.into());
    }
    if let Ok(r) = decode_value::<PreTaskDrawerRecord>(bytes) {
        return Ok(r.into());
    }
    decode_value::<LegacyDrawerRecord>(bytes).map(Into::into)
}

/// Build a `DrawerRecord` from a live `Drawer`.
///
/// Why: Three call sites (single upsert, bulk import, batch upsert) all
/// build the same record; centralising the construction keeps the
/// drawer_type / expires_at_ms fields (issue #61) in sync across them.
/// What: Copies the persisted fields and converts the optional
/// `DrawerType` and `expires_at` into their on-disk representations.
/// Test: Indirect via `upsert_drawer_then_load_drawers_round_trips` and
/// the new `drawer_type_round_trips_through_redb`.
pub(super) fn drawer_to_record(drawer: &Drawer) -> DrawerRecord {
    DrawerRecord {
        room_id: drawer.room_id.to_string(),
        content: drawer.content().to_string(),
        importance: drawer.importance,
        tags: drawer.tags.clone(),
        source_file: drawer
            .source_file
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        created_at_ms: drawer.created_at.timestamp_millis(),
        drawer_type: Some(drawer.drawer_type.as_str().to_string()),
        expires_at_ms: drawer.expires_at.map(|d| d.timestamp_millis()),
        completed_at_ms: drawer.completed_at.map(|d| d.timestamp_millis()),
        // #4884: carried verbatim — the slot name is the writer's, never
        // normalised here, so the index key and the record always agree.
        fact_key: drawer.fact_key.clone(),
    }
}

/// Parse a `DrawerType` tag back from its on-disk string representation.
///
/// Why: We persist the variant name as a string so the schema stays stable
/// when new variants are added; readers tolerate unknown / absent tags by
/// returning `DrawerType::Unknown` (the migration default).
/// What: delegates to [`DrawerType::from_tag`], the inverse of
/// `DrawerType::as_str`. #5902 moved the match onto the enum so the JSONL share
/// format reads the same projection this store writes; the wrapper stays because
/// every call site in this module names it.
/// Test: Indirect via `drawer_type_round_trips_through_redb`;
/// `palace::tests::drawer_type_tag_round_trips_every_variant` pins the projection.
pub(super) fn parse_drawer_type(tag: Option<&str>) -> crate::memory_core::palace::DrawerType {
    crate::memory_core::palace::DrawerType::from_tag(tag)
}

/// Shared per-path state: the open `Database` plus its open mode and
/// optional snapshot guard. Bundled into one `Arc` so every cache hit
/// inherits the same snapshot lifetime (the guard's `Drop` removes the
/// snapshot file on disk).
///
/// Why: Issue #59 — when the live redb file is locked by another process
/// (typically the HTTP daemon), `try_open_or_snapshot` copies it to a
/// process-local snapshot. The snapshot's `SnapshotGuard` must live as
/// long as any handle to the resulting `Database` to keep the temp file
/// alive for reads. Bundling them in one `Arc` ties the two lifetimes
/// together.
/// What: Carries the open `Database`, the `OpenMode`, and the snapshot
/// guard. `SnapshotGuard::noop()` is used for the read/write path so the
/// shape is uniform.
/// Test: Indirect via every `KgStoreRedb::open` call.
#[derive(Debug)]
pub(super) struct KgDbState {
    /// The live `Database`, behind a lock so #6652's copy-then-swap compaction
    /// can install a replacement without dropping the shared state.
    ///
    /// Why (#6652): every `KgStoreRedb` clone that opened the same canonical
    /// path shares ONE `Arc<KgDbState>`, and redb never notices that the file
    /// under its `Database` was replaced by a rename. Before this field was a
    /// lock, a compaction could only invalidate [`db_cache`] and hope the next
    /// `open` picked up the new file — the daemon's own long-lived handle kept
    /// reading the unlinked pre-compaction inode forever, silently. Making the
    /// handle swappable is what lets the swap FAIL CLOSED: the rename only
    /// happens when this lock can be taken and the replacement installed.
    /// What: `RwLock<Arc<Database>>`. Readers take the read lock, clone the
    /// `Arc`, and drop the lock immediately — a transaction opened from that
    /// clone keeps the old `Database` (and thus the old inode) alive for its
    /// whole lifetime, which is exactly redb's MVCC contract. The write lock is
    /// held only for the pointer store inside [`super::copy_swap`].
    /// Test: `compaction_swaps_the_live_handle_in_place`.
    pub db: RwLock<Arc<Database>>,
    /// Excludes every kg.redb write transaction for the duration of the
    /// compaction swap (#6652, code-critic BLOCK on effb8c343).
    ///
    /// Why: the palace write mutex is the wrong primitive and was never
    /// sufficient. It is per-`PalaceHandle`, while a `KgStoreRedb` is shared
    /// per canonical path across every handle in the process — and, decisively,
    /// `KgWriter`'s actor commits on a `spawn_blocking` thread holding only
    /// `Arc<KgStoreRedb>`, so `KnowledgeGraph::assert` / `retract` (behind
    /// `kg_assert`, `kg_retract_triple`, `share::supersede`, and every direct
    /// KG mutator) never touch that mutex at all. A commit landing between the
    /// swap's fingerprint re-check and its `rename` therefore wrote to the old
    /// inode, the rename unlinked it, and the caller was told it succeeded.
    /// What: `RwLock<()>` living beside the handle it protects, so its scope is
    /// exactly the file being swapped. Writers take `read()` — they do not
    /// contend with each other, because redb already serialises write
    /// transactions — and hold it for the whole transaction. The swap takes
    /// `write()` across the re-check, the rename, and the install together, so
    /// there is no window between them for a write to slip into. Lock order is
    /// always `PalaceHandle::write_mutex` then this, never the reverse.
    /// Test: `a_kg_writer_commit_inside_the_swap_window_is_never_dropped`.
    pub swap_lock: RwLock<()>,
    pub mode: OpenMode,
    pub _snapshot_guard: SnapshotGuard,
}

impl KgDbState {
    /// The current `Database`, as an owned handle.
    ///
    /// Why: the lock must not be held across a redb transaction — a
    /// long-running read would block the compaction swap, and the swap would
    /// block every reader. Cloning the `Arc` costs one atomic increment and
    /// releases the lock immediately.
    /// What: read-locks, clones, unlocks. Panics only if the lock is poisoned,
    /// which means a previous holder panicked while swapping — an
    /// unrecoverable invariant break, not a runtime condition to handle.
    /// Test: exercised by every read/write path in this module.
    pub(super) fn db(&self) -> Arc<Database> {
        Arc::clone(&self.db.read().expect("kg db handle lock poisoned"))
    }
}

/// Why: redb forbids more than one in-process `Database` handle to the same
/// file ("Database already open. Cannot acquire lock."). The trusty stack
/// regularly opens the same palace from multiple registries within a single
/// process (e.g. test setup + `AppState`, or background dreamer + foreground
/// handle); SQLite previously allowed this so we must preserve it. The fix
/// is a process-global cache of `Weak<KgDbState>` keyed by canonical path —
/// when any handle is alive we hand it back; once all handles drop the entry
/// expires and the next `open` creates a fresh `Database`.
/// What: Lazily-initialised global mutex over a `HashMap<canonical_path,
/// Weak<KgDbState>>`.
/// Test: `multiple_handles_to_same_path_share_database`.
pub(super) fn db_cache() -> &'static Mutex<HashMap<PathBuf, Weak<KgDbState>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Weak<KgDbState>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Why: The cache key must be path-canonical so `/var/tmp/x` and
/// `/private/var/tmp/x` (the same file via symlink) collapse to one entry.
/// What: Tries `canonicalize`; on failure falls back to the original path so
/// brand-new files (not yet on disk) still work.
/// Test: Indirect — exercised by every `open` call.
pub(super) fn canonical_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Return the current wall-clock time as milliseconds since the Unix epoch.
///
/// Why: Multiple methods stamp records with now; a single function makes
/// mocking easier and avoids scattered `Utc::now()` calls.
/// What: Calls `Utc::now().timestamp_millis()`.
/// Test: Indirect via assert / retract timestamps.
pub(super) fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Convert a millisecond timestamp to a `DateTime<Utc>`.
///
/// Why: Every place that decodes a stored timestamp needs the same error
/// message; centralising avoids drift.
/// What: Delegates to `DateTime::from_timestamp_millis`, returning an
/// `anyhow` error on failure.
/// Test: Indirect via `triple_from_parts` callers.
pub(super) fn ms_to_dt(ms: i64) -> Result<DateTime<Utc>> {
    DateTime::from_timestamp_millis(ms).context("invalid millisecond timestamp")
}

/// Reconstruct a `Triple` from its stored parts.
///
/// Why: Three read paths (query_active, list_active, dump_all_triples) all
/// need to decode a `TripleValue` plus its key fields into a `Triple`.
/// What: Converts timestamps via `ms_to_dt`, assembles the struct.
/// Test: Indirect via every read method that returns `Triple`.
pub(super) fn triple_from_parts(
    subject: String,
    predicate: String,
    v: TripleValue,
) -> Result<Triple> {
    let valid_from = ms_to_dt(v.valid_from_ms)?;
    let valid_to = match v.valid_to_ms {
        Some(ms) => Some(ms_to_dt(ms)?),
        None => None,
    };
    Ok(Triple {
        subject,
        predicate,
        object: v.object,
        valid_from,
        valid_to,
        confidence: v.confidence,
        provenance: v.provenance,
    })
}

/// A kg.redb write transaction and the swap-exclusion it must not outlive.
///
/// Why (#6652): the guard has to live as long as the transaction, not just as
/// long as the `begin_write` call — a swap that started after `begin_write`
/// returned would still race the commit. Bundling the two makes that
/// impossible to get wrong at a call site: you cannot hold the transaction
/// without holding the guard.
/// What: owns the read guard, an `Arc<Database>` clone (so the handle a
/// blocked writer picks up after a swap stays alive for its whole
/// transaction), and the transaction itself. [`Self::commit`] consumes all
/// three in the right order.
/// Test: `a_kg_writer_commit_inside_the_swap_window_is_never_dropped`.
pub(super) struct GuardedWrite<'a> {
    pub(super) _swap: std::sync::RwLockReadGuard<'a, ()>,
    pub(super) _db: Arc<Database>,
    pub(super) txn: redb::WriteTransaction,
}

impl GuardedWrite<'_> {
    /// Commit the transaction, then release the swap exclusion.
    ///
    /// Test: every write path in this module.
    pub(super) fn commit(self) -> Result<()> {
        self.txn.commit().context("commit kg.redb write txn")
    }
}

/// A single write op that can be queued through `apply_batch`.
///
/// Why: The write coalescer in `kg_writer.rs` accepts ops from concurrent
/// callers, then replays them inside a single redb transaction. Modelling
/// the op shape explicitly keeps the writer task backend-agnostic and
/// makes `apply_batch` directly unit-testable.
/// What: Mirrors the four mutating entry points on `KgStoreRedb` —
/// `assert`, `retract`, `upsert_drawer`, `delete_drawer`. All variants
/// own their data so an op can cross an `mpsc` channel.
/// Test: `apply_batch_groups_asserts_into_single_commit` exercises the
/// `Assert` variant; the writer tests cover the others.
#[derive(Debug, Clone)]
pub enum BatchWriteOp {
    /// Assert a triple; closes any prior active interval.
    Assert(Triple),
    /// Close the active triple for `(subject, predicate)` without
    /// inserting a replacement.
    Retract { subject: String, predicate: String },
    /// Persist a drawer row.
    UpsertDrawer(Drawer),
    /// Remove a drawer row by UUID.
    DeleteDrawer(Uuid),
}

/// Per-op outcome returned from `apply_batch`.
///
/// Why: Callers awaiting a queued op need typed results — in particular
/// `Retract` returns 0/1 for "rows closed" which the writer task forwards
/// back through a `oneshot::Sender<Result<usize>>`.
/// What: Enum carrying the same return shape each single-op method
/// already exposes (`assert` → unit, `retract` → usize, drawer ops →
/// unit).
/// Test: Indirect via `apply_batch_*` tests and the writer tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchOpResult {
    Asserted,
    Retracted(usize),
    DrawerUpserted,
    DrawerDeleted,
}

/// Convenience alias for an already-opened redb table with byte-slice keys
/// and values; used by the in-transaction batch helpers.
///
/// Why: The batch helpers take mutable references to several tables; naming
/// the concrete type once avoids repetitive angle-bracket noise.
/// What: Type alias for `redb::Table<'txn, &'static [u8], &'static [u8]>`.
/// Test: Indirect via `apply_batch` and batch helper callers.
pub(super) type Tbl<'txn> = redb::Table<'txn, &'static [u8], &'static [u8]>;

/// Rebuild a `Drawer` from one `DRAWERS` row's stored value (#6438).
///
/// Why: two readers now hydrate a drawer row — the whole-table
/// `load_drawers_with_skipped` and the single-row `load_drawer` that the Tier C
/// retirement path consults before deciding a slot is free. Sharing one decoder
/// keeps them from drifting: a row the table scan can hydrate is exactly a row
/// the point-read can hydrate, which is what lets `persist_with_retirement`
/// read "the point-read returned a drawer" as "this incumbent can be retired".
/// What: walks the postcard migration chain via [`decode_drawer_record`],
/// parses `room_id` and `created_at_ms`, and rebuilds through `Drawer::new` so
/// the content digest is derived rather than stored (#5902), then overwrites
/// the identity fields the constructor generates. Every failure returns `Err`
/// naming the offending field; the caller decides whether to skip the row
/// (table scan) or fail the read (point-read).
/// Test: `an_on_disk_incumbent_absent_from_the_mirror_is_still_retired`,
/// `an_undecodable_incumbent_row_fails_the_write_instead_of_admitting_a_second_claimant`,
/// `drawer_load_degraded_true_on_partial_row_corruption`.
pub(super) fn drawer_from_row(id: Uuid, value: &[u8]) -> Result<Drawer> {
    let record: DrawerRecord =
        decode_drawer_record(value).map_err(|e| anyhow::anyhow!("malformed drawer value: {e}"))?;
    let room_id = Uuid::parse_str(&record.room_id).context("invalid room_id")?;
    let created_at =
        DateTime::from_timestamp_millis(record.created_at_ms).context("invalid created_at_ms")?;
    let mut drawer = Drawer::new(room_id, record.content);
    drawer.id = id;
    drawer.importance = record.importance;
    drawer.source_file = record.source_file.map(PathBuf::from);
    drawer.created_at = created_at;
    drawer.tags = record.tags;
    drawer.drawer_type = parse_drawer_type(record.drawer_type.as_deref());
    drawer.expires_at = record
        .expires_at_ms
        .and_then(DateTime::from_timestamp_millis);
    drawer.completed_at = record
        .completed_at_ms
        .and_then(DateTime::from_timestamp_millis);
    drawer.fact_key = record.fact_key;
    Ok(drawer)
}
