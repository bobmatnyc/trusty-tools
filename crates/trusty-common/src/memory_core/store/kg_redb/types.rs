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
use std::sync::{Arc, Mutex, OnceLock, Weak};
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
        }
    }
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
        content: drawer.content.clone(),
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
    }
}

/// Parse a `DrawerType` tag back from its on-disk string representation.
///
/// Why: We persist the variant name as a string so the schema stays stable
/// when new variants are added; readers tolerate unknown / absent tags by
/// returning `DrawerType::Unknown` (the migration default).
/// What: Match against the known variant names; anything else falls back
/// to `DrawerType::Unknown`.
/// Test: Indirect via `drawer_type_round_trips_through_redb`.
pub(super) fn parse_drawer_type(tag: Option<&str>) -> crate::memory_core::palace::DrawerType {
    use crate::memory_core::palace::DrawerType;
    match tag {
        Some("UserFact") => DrawerType::UserFact,
        Some("SessionEvent") => DrawerType::SessionEvent,
        Some("AgentNote") => DrawerType::AgentNote,
        Some("Commit") => DrawerType::Commit,
        Some("Task") => DrawerType::Task,
        _ => DrawerType::Unknown,
    }
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
    pub db: Arc<Database>,
    pub mode: OpenMode,
    pub _snapshot_guard: SnapshotGuard,
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
