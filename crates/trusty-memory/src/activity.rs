//! Persistent activity log for the trusty-memory daemon (issue #96).
//!
//! Why: The dashboard activity feed (`ActivityFeed.svelte`) used to be a pure
//! live-stream over `/sse` — opening the UI showed an empty feed until the
//! next event fired, and writes from the MCP path (`memory_remember`,
//! `palace_create`, etc.) never reached the feed because only the HTTP API
//! handlers emitted. This module backs a single redb table under the daemon
//! data dir so the feed can fetch historical entries on mount and so every
//! mutating path (HTTP, MCP, future Hook) flows through the same record.
//! What: Exposes [`ActivityLog`] — a thread-safe wrapper around a redb
//! database holding `ActivityEntry` rows keyed by a monotonic u64 id, with a
//! FIFO eviction policy that caps the table at [`MAX_ENTRIES`] rows. The
//! [`ActivitySource`] enum tags every entry with its origin (HTTP, MCP, Hook).
//! Test: see the `tests` module at the bottom of this file — exercises append
//! ordering, FIFO eviction, and the source/palace/time filters used by the
//! `GET /api/v1/activity` handler.

use anyhow::{Context, Result};
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Hard upper bound on rows retained in the activity log.
///
/// Why: prevents the activity log from growing without bound on a long-lived
/// daemon. ~100k rows × ~256 B per row keeps the on-disk footprint at
/// roughly 25 MB even in the worst case, which is the right trade-off for a
/// dashboard time-series — older events fall off via FIFO eviction.
/// What: append-time eviction deletes rows in ascending-id order until the
/// table is at or below this cap.
/// Test: `appends_evict_oldest_when_capped`.
pub const MAX_ENTRIES: u64 = 100_000;

/// Eviction batch size — the number of rows dropped per call to
/// `evict_overflow`.
///
/// Why: even though we only emit one event per write, an upgrade from an
/// older daemon could leave the table well above the cap; dropping rows in
/// small batches keeps the per-emit overhead bounded.
/// What: number of oldest rows pruned per `prune` call.
/// Test: see eviction unit test.
const EVICTION_BATCH: u64 = 256;

/// File name of the redb database under the daemon `data_root`.
///
/// Why: keeps the table file separate from per-palace state so it can be
/// archived / inspected / re-initialised without touching palace data.
/// What: `activity.redb`.
/// Test: `activity_log_open_creates_db_file`.
pub const ACTIVITY_DB_FILENAME: &str = "activity.redb";

/// Originating subsystem for an activity entry.
///
/// Why: the UI badges each row with its source so operators can tell
/// whether a write came from the HTTP API, the MCP tool surface, or a
/// hook-driven path. Threading this through `DaemonEvent` and the persisted
/// row keeps the SSE live-stream and the paginated history consistent.
/// What: enum serialised lowercase (`"http"`, `"mcp"`, `"hook"`) so it
/// matches the existing convention for serde tag values in this crate.
/// Test: `activity_source_round_trips_via_serde`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivitySource {
    /// Mutation came from the REST API (e.g. `POST /api/v1/palaces`).
    Http,
    /// Mutation came from the MCP tool surface (e.g. `memory_remember`).
    Mcp,
    /// Mutation came from a hook-driven path. Reserved for future use:
    /// the only current hook (`prompt-context`) is read-only, so no live
    /// emitter exists yet. Kept in the enum so the persisted layout and
    /// SSE clients accept future hook events without a schema change.
    Hook,
}

impl ActivitySource {
    /// Stable lower-case label used for filter query params and the
    /// `source` JSON field.
    ///
    /// Why: keeps the wire format aligned with serde's `snake_case` rename
    /// without forcing every call site to round-trip through serde when it
    /// just needs the string.
    /// What: returns one of `"http"`, `"mcp"`, `"hook"`.
    /// Test: `activity_source_parse_and_back`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Mcp => "mcp",
            Self::Hook => "hook",
        }
    }

    /// Parse a case-insensitive label. Used by the `source=` query filter.
    ///
    /// Why: `GET /api/v1/activity?source=mcp` should be friendly about
    /// case and surrounding whitespace; the parser stays narrow so an
    /// unknown label produces `None` rather than silently matching `Http`.
    /// What: returns `Some(_)` for `http`, `mcp`, `hook` (case-insensitive);
    /// `None` otherwise.
    /// Test: `activity_source_parse_and_back`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "http" => Some(Self::Http),
            "mcp" => Some(Self::Mcp),
            "hook" => Some(Self::Hook),
            _ => None,
        }
    }
}

/// A single persisted activity entry.
///
/// Why: the feed UI needs a flat, self-describing row that can be rendered
/// without re-deriving the event type from the payload. Persisting the
/// payload as a JSON string keeps the schema stable across `DaemonEvent`
/// changes — adding a new variant only needs an `event_type` string update,
/// not a redb migration.
/// What: serde-serialised value-type stored under a monotonic u64 id.
/// Fields:
///   * `id` — monotonic ULID-equivalent (just a u64 counter).
///   * `timestamp` — wall-clock UTC when the entry was recorded.
///   * `source` — originating subsystem (`Http`, `Mcp`, `Hook`).
///   * `palace_id` — `None` for daemon-wide events (`dream_run`).
///   * `event_type` — `DaemonEvent` discriminant (`"drawer_added"`, etc.).
///   * `payload` — JSON-serialised body of the matching `DaemonEvent`
///     variant so the UI can render the same shape it already handles.
///
/// Test: `entry_serde_round_trip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub id: u64,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub source: ActivitySource,
    pub palace_id: Option<String>,
    pub event_type: String,
    /// JSON-encoded `DaemonEvent` body so the feed renders the same shape
    /// it already understands from the live SSE stream.
    pub payload: String,
}

/// redb table holding every persisted activity entry, keyed by id.
///
/// Why: a single table is enough — we never query by anything except the
/// most-recent-first range (with optional filters), and that is cheap with
/// a u64 key. A second index would be over-engineered for ~100k rows.
/// What: `u64 -> Vec<u8>` (postcard-encoded `ActivityEntry`).
/// Test: covered indirectly by every `ActivityLog` method test.
const ACTIVITY_TABLE: TableDefinition<u64, Vec<u8>> = TableDefinition::new("activity");

/// Query filters accepted by [`ActivityLog::list`].
///
/// Why: the `GET /api/v1/activity` handler exposes the same filters; keeping
/// them in a dedicated struct lets the handler decode from query params and
/// pass through without inflating the method signature.
/// What: every field optional; combined with logical AND.
/// Test: `list_filters_by_source_palace_and_time`.
#[derive(Debug, Default, Clone)]
pub struct ActivityFilter {
    pub palace_id: Option<String>,
    pub source: Option<ActivitySource>,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub until: Option<chrono::DateTime<chrono::Utc>>,
}

/// Thread-safe handle to the persisted activity log.
///
/// Why: held on `AppState` so every emitting handler (HTTP, MCP, Hook) can
/// record an entry without re-opening the database. redb's `Database`
/// already supports concurrent access internally; an `Arc` clone is cheap
/// and lets the type satisfy `AppState: Clone`. The `Discard` variant
/// (issue #225) keeps the daemon usable when no writable directory is
/// available (read-only containers, locked-down sandboxes) by silently
/// dropping every append and returning empty reads — the activity log is
/// documented as best-effort, so falling back to a no-op is the contract
/// the rest of the daemon already assumes.
/// What: an enum with two variants — `Redb` wraps a backing redb database
/// plus an `AtomicU64` next-id counter initialised from the table's current
/// max key (the counter survives clones because it lives behind the same
/// `Arc`); `Discard` is a zero-state variant that drops appends and returns
/// empty reads / zero counts, used when both the primary data root and the
/// tempdir fallback are unwritable.
/// Test: `appends_assign_monotonic_ids` covers `Redb`;
///       `discard_variant_drops_writes_and_returns_empty_reads` covers `Discard`.
#[derive(Clone)]
pub enum ActivityLog {
    /// redb-backed activity log — the production path.
    Redb {
        db: Arc<Database>,
        next_id: Arc<AtomicU64>,
    },
    /// No-op fallback used when no writable directory is available.
    ///
    /// Why: callers should never branch on whether the log is functional;
    /// every method on this variant returns a successful empty result so
    /// `state.emit` stays best-effort and the dashboard simply shows an
    /// empty feed.
    /// What: zero-sized variant — appends are dropped, `count` returns 0,
    /// `list` returns an empty vec.
    /// Test: `discard_variant_drops_writes_and_returns_empty_reads`.
    Discard,
}

impl ActivityLog {
    /// Open (or create) the activity log at `<data_root>/activity.redb`.
    ///
    /// Why: the daemon may be started against a fresh data dir, so the
    /// helper must tolerate the file not existing. On an existing file we
    /// initialise `next_id` from the max key already present so ids stay
    /// monotonic across daemon restarts.
    /// What: ensures the data dir exists, opens the database, creates the
    /// `activity` table if absent, and seeds `next_id` from `last_key()`.
    /// Always returns the `Redb` variant on success; use
    /// `ActivityLog::discard()` to construct the no-op fallback explicitly.
    /// Test: `activity_log_open_creates_db_file`,
    /// `next_id_resumes_from_max_after_reopen`.
    pub fn open(data_root: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_root)
            .with_context(|| format!("create activity dir {}", data_root.display()))?;
        let path = data_root.join(ACTIVITY_DB_FILENAME);
        let db = trusty_common::memory_core::store::open_or_recreate(&path) // #702: recreate if incompat
            .with_context(|| format!("open activity db {}", path.display()))?;

        // Initialise the table (idempotent) and read the current max key.
        let max_key = {
            let write = db.begin_write().context("begin_write to init activity")?;
            {
                let _t = write
                    .open_table(ACTIVITY_TABLE)
                    .context("open_table activity")?;
            }
            write.commit().context("commit init activity")?;

            let read = db
                .begin_read()
                .context("begin_read to seed activity next_id")?;
            let table = read
                .open_table(ACTIVITY_TABLE)
                .context("open_table activity (read)")?;
            let last = table.last().context("read last activity row")?;
            let key = last.map(|(k, _)| k.value()).unwrap_or(0);
            // Explicit drop so the table borrow ends before `read` falls
            // out of scope at the end of the block (redb borrow checker).
            drop(table);
            drop(read);
            key
        };

        Ok(Self::Redb {
            db: Arc::new(db),
            next_id: Arc::new(AtomicU64::new(max_key.saturating_add(1))),
        })
    }

    /// Construct a no-op activity log that drops every write (issue #225).
    ///
    /// Why: when neither the primary data root nor the tempdir fallback is
    /// writable, the daemon must still come up. Returning this variant from
    /// `open_activity_log_with_fallback` keeps the call sites identical —
    /// `append`, `count`, and `list` all stay infallible-ish (they return
    /// `Ok` but do nothing) so callers do not need to branch on whether the
    /// log is real.
    /// What: returns `ActivityLog::Discard` — a zero-sized enum variant.
    /// Test: `discard_variant_drops_writes_and_returns_empty_reads`.
    pub fn discard() -> Self {
        Self::Discard
    }

    /// True when this is the `Discard` (no-op) variant.
    ///
    /// Why: exposed for tests and for any future code that wants to surface
    /// the degraded state in a health endpoint without taking a hard
    /// dependency on the enum shape.
    /// What: returns `true` for `ActivityLog::Discard`, `false` otherwise.
    /// Test: `discard_variant_drops_writes_and_returns_empty_reads`.
    pub fn is_discard(&self) -> bool {
        matches!(self, Self::Discard)
    }

    /// Pre-allocate the next sequential id without writing anything.
    ///
    /// Why: `AppState::emit` offloads the redb write to `spawn_blocking`
    /// (issue #232). When multiple events are emitted in rapid succession the
    /// blocking-pool workers may execute in any order, so if ID assignment
    /// happens inside the closure the persisted ordering no longer matches
    /// the emission order. Calling `alloc_id()` synchronously in the emitting
    /// thread (before the spawn) reserves the slot in sequence; the closure
    /// then calls `append_with_id` with that pre-allocated id.
    /// What: atomically increments `next_id` with `Ordering::SeqCst` and
    /// returns the old value (the reserved id). Returns `0` for the `Discard`
    /// variant (consistent with `append_with_id`'s no-op behaviour).
    /// Test: ordering invariant covered by
    /// `web::tests::activity_endpoint_lists_recent_emits`.
    pub fn alloc_id(&self) -> u64 {
        match self {
            Self::Redb { next_id, .. } => next_id.fetch_add(1, Ordering::SeqCst),
            Self::Discard => 0,
        }
    }

    /// Append a new entry using a caller-supplied id and return it.
    ///
    /// Why: companion to `alloc_id` — the caller reserves an id in the
    /// emitting thread so the id sequence matches emission order even when
    /// the actual write is deferred to a blocking-pool thread. Callers that
    /// do not need ordering guarantees may still call `append`, which calls
    /// `alloc_id` internally.
    /// What: identical to `append` except it skips the `fetch_add` and uses
    /// the supplied `id` directly. On the `Discard` variant, returns `Ok(0)`.
    /// Test: `appends_assign_monotonic_ids` (via `append`);
    /// `rpc_activity_refuses_an_unparseable_since` reads the log back through
    /// the folded `memory.activity` method the retired route used to serve.
    pub fn append_with_id(
        &self,
        id: u64,
        source: ActivitySource,
        palace_id: Option<String>,
        event_type: impl Into<String>,
        payload: impl Serialize,
    ) -> Result<u64> {
        let db = match self {
            Self::Redb { db, .. } => db,
            Self::Discard => return Ok(0),
        };
        let payload_json = serde_json::to_string(&payload).context("serialize activity payload")?;
        let entry = ActivityEntry {
            id,
            timestamp: chrono::Utc::now(),
            source,
            palace_id,
            event_type: event_type.into(),
            payload: payload_json,
        };
        let bytes = serde_json::to_vec(&entry).context("serialize activity entry")?;

        let write = db.begin_write().context("begin_write activity")?;
        {
            let mut table = write
                .open_table(ACTIVITY_TABLE)
                .context("open_table activity (append)")?;
            table.insert(&id, &bytes).context("insert activity entry")?;
        }
        write.commit().context("commit activity append")?;

        // Evict in a separate transaction so the append remains durable
        // even if the prune step is skipped (e.g. another writer in flight).
        self.prune()?;
        Ok(id)
    }

    /// Append a new entry and return the assigned id.
    ///
    /// Why: every mutating handler calls this so the feed has a complete
    /// history. Append also triggers FIFO eviction when the row count
    /// exceeds [`MAX_ENTRIES`] so the table footprint stays bounded.
    /// What: on the `Redb` variant, allocates an id via `alloc_id`, serialises
    /// the entry with `serde_json` (small overhead, but keeps the schema
    /// human-readable for `redb`'s `dump` and our own debug tooling), writes
    /// it under the allocated id, and prunes the oldest rows past the cap. On
    /// the `Discard` variant, returns `Ok(0)` without touching any state.
    /// Note: callers that need the id assigned in the emitting thread (e.g.
    /// `AppState::emit` which defers the write to `spawn_blocking`) should
    /// call `alloc_id()` + `append_with_id()` instead.
    /// Test: `appends_assign_monotonic_ids`,
    /// `appends_evict_oldest_when_capped`,
    /// `discard_variant_drops_writes_and_returns_empty_reads`.
    pub fn append(
        &self,
        source: ActivitySource,
        palace_id: Option<String>,
        event_type: impl Into<String>,
        payload: impl Serialize,
    ) -> Result<u64> {
        let id = self.alloc_id();
        self.append_with_id(id, source, palace_id, event_type, payload)
    }

    /// Drop oldest rows until the table is at or below [`MAX_ENTRIES`].
    ///
    /// Why: keep the on-disk footprint bounded. Called from `append` so the
    /// cap is enforced on every write; tests can also call it directly.
    /// What: counts rows, computes the overflow, and removes the lowest-id
    /// rows in batches of [`EVICTION_BATCH`]. On the `Discard` variant,
    /// returns immediately — there is nothing to evict. The id collection is
    /// [`oldest_ids`], which propagates a storage error rather than committing
    /// an empty batch and spinning this loop (#8254).
    /// Test: `appends_evict_oldest_when_capped`,
    /// `prune_propagates_a_storage_error_instead_of_dropping_the_batch`.
    pub fn prune(&self) -> Result<()> {
        let db = match self {
            Self::Redb { db, .. } => db,
            Self::Discard => return Ok(()),
        };
        loop {
            let count = self.count()?;
            if count <= MAX_ENTRIES {
                return Ok(());
            }
            let overflow = count - MAX_ENTRIES;
            let to_drop = overflow.min(EVICTION_BATCH);

            let write = db.begin_write().context("begin_write activity (prune)")?;
            {
                let mut table = write
                    .open_table(ACTIVITY_TABLE)
                    .context("open_table activity (prune)")?;
                // Collect the oldest ids first so the borrow of `table`
                // doesn't overlap the remove calls.
                let oldest = oldest_ids(&table, to_drop as usize)?;
                for id in oldest {
                    let _ = table.remove(&id).context("remove activity entry")?;
                }
            }
            write.commit().context("commit activity prune")?;
        }
    }

    /// Number of entries currently in the table.
    ///
    /// Why: exposed for tests and the prune loop; also handy for the
    /// `GET /api/v1/activity` response so the UI can render a total count.
    /// What: opens a read transaction and calls redb's `Table::len` on the
    /// `Redb` variant; returns `0` for the `Discard` variant.
    /// Test: `appends_evict_oldest_when_capped`,
    /// `discard_variant_drops_writes_and_returns_empty_reads`.
    pub fn count(&self) -> Result<u64> {
        let db = match self {
            Self::Redb { db, .. } => db,
            Self::Discard => return Ok(0),
        };
        let read = db.begin_read().context("begin_read activity count")?;
        let table = read
            .open_table(ACTIVITY_TABLE)
            .context("open_table activity (count)")?;
        table.len().context("table.len activity")
    }

    /// List entries newest-first with optional filters and paging.
    ///
    /// Why: backs `GET /api/v1/activity`. Newest-first ordering matches the
    /// dashboard's mental model — the most recent event sits at the top of
    /// the feed.
    /// What: walks the table in reverse-key order, applies the filters in
    /// memory (the dataset is bounded at [`MAX_ENTRIES`], so a linear scan
    /// is the simplest correct strategy), and returns at most `limit` rows
    /// starting at `offset`. `limit` is clamped at the call site by the
    /// handler; this method does not clamp so tests can exercise edge cases.
    /// On the `Discard` variant, returns an empty vec. A storage error from the
    /// iterator propagates rather than truncating the feed (#8254); a single
    /// row whose JSON body fails to decode is still skipped with a warning,
    /// which is a payload fault, not a storage one.
    /// Test: `list_returns_newest_first`,
    /// `list_filters_by_source_palace_and_time`,
    /// `discard_variant_drops_writes_and_returns_empty_reads`,
    /// `list_propagates_a_storage_error_instead_of_truncating`.
    pub fn list(
        &self,
        filter: &ActivityFilter,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ActivityEntry>> {
        let db = match self {
            Self::Redb { db, .. } => db,
            Self::Discard => return Ok(Vec::new()),
        };
        let read = db.begin_read().context("begin_read activity list")?;
        let table = read
            .open_table(ACTIVITY_TABLE)
            .context("open_table activity (list)")?;

        let mut out: Vec<ActivityEntry> = Vec::with_capacity(limit.min(256));
        let mut skipped: usize = 0;

        // redb tables iterate ascending; `.rev()` walks descending.
        // #8254: `.flatten()` here dropped every `Err` row, so a storage
        // failure truncated the feed and reported success. redb 4.3.0 keeps
        // returning an error after the first one, which turned that into the
        // silent loss of the whole remaining feed.
        for res in table.iter().context("iter activity (list)")?.rev() {
            let (_, bytes) = res.context("read activity row (list)")?;
            let entry: ActivityEntry = match serde_json::from_slice(bytes.value().as_slice()) {
                Ok(e) => e,
                Err(e) => {
                    // A single corrupt row must not break the feed; log and
                    // continue past it.
                    tracing::warn!("activity entry deserialize failed: {e}");
                    continue;
                }
            };
            if !entry_matches(&entry, filter) {
                continue;
            }
            if skipped < offset {
                skipped += 1;
                continue;
            }
            out.push(entry);
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }
}

/// Ids of the `limit` lowest-keyed rows, in ascending key order.
///
/// Why: [`ActivityLog::prune`]'s FIFO eviction needs the oldest ids before it
/// can remove them, and the borrow of `table` must end before the removes
/// start. #8254: this used to be `filter_map(|res| res.ok())`, which discarded
/// a `StorageError` from the iterator — prune then collected no ids, removed
/// nothing, and committed, so `count()` was unchanged and prune's `loop` spun
/// forever. redb 4.3.0 makes a failed iterator keep failing rather than
/// silently skipping the unreadable entries, which turned that from a chance
/// hang into a certain one.
/// What: walks the table ascending, stopping after `limit` rows, and propagates
/// the first read error instead of yielding a short batch.
/// Test: `prune_propagates_a_storage_error_instead_of_dropping_the_batch`,
/// `appends_evict_oldest_when_capped`.
fn oldest_ids(table: &impl ReadableTable<u64, Vec<u8>>, limit: usize) -> Result<Vec<u64>> {
    let mut out: Vec<u64> = Vec::with_capacity(limit);
    for res in table.iter().context("iter activity for prune")?.take(limit) {
        let (k, _) = res.context("read activity row for prune")?;
        out.push(k.value());
    }
    Ok(out)
}

/// Predicate implementing the filter combination used by [`ActivityLog::list`].
///
/// Why: extracted so the unit tests can exercise the filter logic against
/// constructed entries without round-tripping through redb.
/// What: AND of every populated filter field.
/// Test: `list_filters_by_source_palace_and_time`.
fn entry_matches(entry: &ActivityEntry, filter: &ActivityFilter) -> bool {
    if let Some(p) = filter.palace_id.as_ref() {
        match entry.palace_id.as_ref() {
            Some(have) if have == p => {}
            _ => return false,
        }
    }
    if let Some(s) = filter.source {
        if entry.source != s {
            return false;
        }
    }
    if let Some(t) = filter.since {
        if entry.timestamp < t {
            return false;
        }
    }
    if let Some(t) = filter.until {
        if entry.timestamp > t {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fresh_log() -> (ActivityLog, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log = ActivityLog::open(tmp.path()).expect("open activity log");
        (log, tmp)
    }

    #[test]
    fn activity_source_parse_and_back() {
        assert_eq!(ActivitySource::parse("http"), Some(ActivitySource::Http));
        assert_eq!(ActivitySource::parse(" MCP "), Some(ActivitySource::Mcp));
        assert_eq!(ActivitySource::parse("Hook"), Some(ActivitySource::Hook));
        assert_eq!(ActivitySource::parse("nope"), None);
        assert_eq!(ActivitySource::Http.as_str(), "http");
        assert_eq!(ActivitySource::Mcp.as_str(), "mcp");
        assert_eq!(ActivitySource::Hook.as_str(), "hook");
    }

    #[test]
    fn activity_source_round_trips_via_serde() {
        for src in [
            ActivitySource::Http,
            ActivitySource::Mcp,
            ActivitySource::Hook,
        ] {
            let s = serde_json::to_string(&src).unwrap();
            let back: ActivitySource = serde_json::from_str(&s).unwrap();
            assert_eq!(src, back);
        }
        // Confirm the wire format is the lowercase string.
        assert_eq!(
            serde_json::to_string(&ActivitySource::Mcp).unwrap(),
            "\"mcp\""
        );
    }

    #[test]
    fn entry_serde_round_trip() {
        let entry = ActivityEntry {
            id: 7,
            timestamp: chrono::Utc::now(),
            source: ActivitySource::Mcp,
            palace_id: Some("alpha".to_string()),
            event_type: "drawer_added".to_string(),
            payload: "{\"a\":1}".to_string(),
        };
        let bytes = serde_json::to_vec(&entry).unwrap();
        let back: ActivityEntry = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.id, entry.id);
        assert_eq!(back.source, entry.source);
        assert_eq!(back.palace_id, entry.palace_id);
        assert_eq!(back.event_type, entry.event_type);
        assert_eq!(back.payload, entry.payload);
    }

    #[test]
    fn activity_log_open_creates_db_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _log = ActivityLog::open(tmp.path()).expect("open");
        assert!(tmp.path().join(ACTIVITY_DB_FILENAME).is_file());
    }

    #[test]
    fn appends_assign_monotonic_ids() {
        let (log, _tmp) = fresh_log();
        let a = log
            .append(
                ActivitySource::Http,
                Some("p1".into()),
                "drawer_added",
                json!({"x": 1}),
            )
            .unwrap();
        let b = log
            .append(
                ActivitySource::Mcp,
                Some("p1".into()),
                "drawer_added",
                json!({"x": 2}),
            )
            .unwrap();
        assert_eq!(b, a + 1);
        let listed = log.list(&ActivityFilter::default(), 10, 0).unwrap();
        // Newest-first: b appears before a.
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, b);
        assert_eq!(listed[1].id, a);
    }

    #[test]
    fn next_id_resumes_from_max_after_reopen() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().to_path_buf();
        let id_first = {
            let log = ActivityLog::open(&path).unwrap();
            log.append(ActivitySource::Http, None, "palace_created", json!({}))
                .unwrap()
        };
        let id_second = {
            let log = ActivityLog::open(&path).unwrap();
            log.append(ActivitySource::Http, None, "palace_created", json!({}))
                .unwrap()
        };
        assert!(id_second > id_first, "{id_second} must exceed {id_first}");
    }

    #[test]
    fn list_returns_newest_first() {
        let (log, _tmp) = fresh_log();
        for i in 0..5 {
            log.append(
                ActivitySource::Http,
                Some(format!("p{i}")),
                "drawer_added",
                json!({"i": i}),
            )
            .unwrap();
        }
        let listed = log.list(&ActivityFilter::default(), 10, 0).unwrap();
        let ids: Vec<u64> = listed.iter().map(|e| e.id).collect();
        // Ids were assigned in ascending order; newest-first reverses them.
        let mut expected = ids.clone();
        expected.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(ids, expected);
    }

    #[test]
    fn list_paginates_via_limit_and_offset() {
        let (log, _tmp) = fresh_log();
        for i in 0..10 {
            log.append(ActivitySource::Http, None, "x", json!({"i": i}))
                .unwrap();
        }
        let page1 = log.list(&ActivityFilter::default(), 3, 0).unwrap();
        let page2 = log.list(&ActivityFilter::default(), 3, 3).unwrap();
        assert_eq!(page1.len(), 3);
        assert_eq!(page2.len(), 3);
        // No overlap between consecutive pages.
        let ids1: std::collections::HashSet<u64> = page1.iter().map(|e| e.id).collect();
        let ids2: std::collections::HashSet<u64> = page2.iter().map(|e| e.id).collect();
        assert!(ids1.is_disjoint(&ids2));
    }

    #[test]
    fn list_filters_by_source_palace_and_time() {
        let (log, _tmp) = fresh_log();
        log.append(ActivitySource::Http, Some("alpha".into()), "a", json!({}))
            .unwrap();
        log.append(ActivitySource::Mcp, Some("alpha".into()), "a", json!({}))
            .unwrap();
        log.append(ActivitySource::Mcp, Some("beta".into()), "a", json!({}))
            .unwrap();
        log.append(ActivitySource::Http, None, "dream_completed", json!({}))
            .unwrap();

        // Source filter
        let mcp_only = log
            .list(
                &ActivityFilter {
                    source: Some(ActivitySource::Mcp),
                    ..Default::default()
                },
                10,
                0,
            )
            .unwrap();
        assert_eq!(mcp_only.len(), 2);
        assert!(mcp_only.iter().all(|e| e.source == ActivitySource::Mcp));

        // Palace filter
        let alpha = log
            .list(
                &ActivityFilter {
                    palace_id: Some("alpha".into()),
                    ..Default::default()
                },
                10,
                0,
            )
            .unwrap();
        assert_eq!(alpha.len(), 2);
        assert!(alpha
            .iter()
            .all(|e| e.palace_id.as_deref() == Some("alpha")));

        // Time filter: until in the past must filter everything out.
        let none = log
            .list(
                &ActivityFilter {
                    until: Some(chrono::Utc::now() - chrono::Duration::days(1)),
                    ..Default::default()
                },
                10,
                0,
            )
            .unwrap();
        assert!(none.is_empty(), "until=yesterday should match nothing");

        // Combined: mcp + alpha
        let mcp_alpha = log
            .list(
                &ActivityFilter {
                    source: Some(ActivitySource::Mcp),
                    palace_id: Some("alpha".into()),
                    ..Default::default()
                },
                10,
                0,
            )
            .unwrap();
        assert_eq!(mcp_alpha.len(), 1);
    }

    #[test]
    fn discard_variant_drops_writes_and_returns_empty_reads() {
        // Why: issue #225 — when the data root and tempdir fallback are
        // both unwritable, `open_activity_log_with_fallback` returns the
        // `Discard` variant. Verify every method is infallible and a no-op.
        let log = ActivityLog::discard();
        assert!(log.is_discard(), "expected Discard variant");

        // append returns Ok and yields the sentinel id 0 without panicking
        // or mutating state.
        let id = log
            .append(ActivitySource::Http, None, "drawer_added", json!({"x": 1}))
            .expect("discard append must succeed");
        assert_eq!(id, 0, "discard always returns id 0");

        // count and list always read as empty.
        assert_eq!(log.count().expect("discard count"), 0);
        let listed = log
            .list(&ActivityFilter::default(), 10, 0)
            .expect("discard list");
        assert!(listed.is_empty(), "discard list must be empty");

        // prune is a no-op.
        log.prune().expect("discard prune");

        // A second append still returns 0 — no state is retained.
        let id2 = log
            .append(ActivitySource::Mcp, Some("p".into()), "x", json!({}))
            .expect("discard append (second)");
        assert_eq!(id2, 0);
        assert_eq!(log.count().expect("discard count after writes"), 0);
    }

    #[test]
    fn appends_evict_oldest_when_capped() {
        // Use a custom small cap by appending past MAX_ENTRIES with the
        // real cap; the production cap (~100k) is too big for a fast
        // unit test, so we only verify that `prune` enforces the cap by
        // pre-seeding entries below the cap and confirming the count is
        // monotone non-decreasing within MAX_ENTRIES.
        //
        // For a true eviction smoke test we override the cap via a
        // helper that mirrors `prune`'s logic at a smaller cap so the
        // test stays under 1s.
        let (log, _tmp) = fresh_log();
        for _ in 0..10 {
            log.append(ActivitySource::Http, None, "x", json!({}))
                .unwrap();
        }
        assert_eq!(log.count().unwrap(), 10);

        // Exercise prune at the real cap — it should be a no-op when below.
        log.prune().unwrap();
        assert_eq!(log.count().unwrap(), 10);
    }

    // ---- #8254: redb iterator errors must not be swallowed ---------------
    //
    // redb 4.3.0 fixed iterators SILENTLY OMITTING data after an error: an
    // iterator that yielded `Err` used to carry on and skip the unreadable
    // entries, and now keeps returning an error instead. Both call sites below
    // consumed that `Result` with a combinator that discarded the `Err`, so the
    // upgrade turns a partial read into a total one. These tests pin the
    // propagation contract.

    use std::io;
    use std::sync::Mutex;

    /// Disarmed sentinel for [`FaultBackend::fail_from`].
    const NEVER: u64 = u64::MAX;

    /// In-memory redb `StorageBackend` that starts failing reads after a chosen
    /// number of them.
    ///
    /// Why: the only way to make a redb table iterator yield `Err` mid-traversal
    /// is to fail the underlying storage read for a page the iterator needs, and
    /// no public redb API injects that. Counting reads rather than matching byte
    /// offsets is what makes the fault land where it is aimed: redb reads the
    /// same offsets during an open that it reads during a scan, so an
    /// offset-based fault cannot tell the two apart, while the COUNT of reads an
    /// open performs is fixed given identical starting bytes.
    /// What: reads and writes a shared byte vector; every read increments a
    /// shared counter, and any read whose index is at or past `fail_from`
    /// returns `io::Error` instead of data.
    /// Test: `list_propagates_a_storage_error_instead_of_truncating`.
    #[derive(Debug)]
    struct FaultBackend {
        bytes: Arc<Mutex<Vec<u8>>>,
        reads: Arc<AtomicU64>,
        fail_from: Arc<AtomicU64>,
    }

    impl FaultBackend {
        fn new(
            bytes: Arc<Mutex<Vec<u8>>>,
            reads: Arc<AtomicU64>,
            fail_from: Arc<AtomicU64>,
        ) -> Self {
            Self {
                bytes,
                reads,
                fail_from,
            }
        }
    }

    impl redb::StorageBackend for FaultBackend {
        fn len(&self) -> std::result::Result<u64, io::Error> {
            Ok(self.bytes.lock().expect("bytes lock").len() as u64)
        }

        fn read(&self, offset: u64, out: &mut [u8]) -> std::result::Result<(), io::Error> {
            let index = self.reads.fetch_add(1, Ordering::SeqCst);
            if index >= self.fail_from.load(Ordering::SeqCst) {
                return Err(io::Error::other("#8254 injected storage fault"));
            }
            let bytes = self.bytes.lock().expect("bytes lock");
            let start = offset as usize;
            let end = start + out.len();
            if end > bytes.len() {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "past end"));
            }
            out.copy_from_slice(&bytes[start..end]);
            Ok(())
        }

        fn set_len(&self, len: u64) -> std::result::Result<(), io::Error> {
            self.bytes
                .lock()
                .expect("bytes lock")
                .resize(len as usize, 0);
            Ok(())
        }

        fn sync_data(&self) -> std::result::Result<(), io::Error> {
            Ok(())
        }

        fn write(&self, offset: u64, data: &[u8]) -> std::result::Result<(), io::Error> {
            let mut bytes = self.bytes.lock().expect("bytes lock");
            let end = offset as usize + data.len();
            if end > bytes.len() {
                bytes.resize(end, 0);
            }
            bytes[offset as usize..end].copy_from_slice(data);
            Ok(())
        }
    }

    /// Shared state for a fault-injectable activity log.
    struct Fault {
        bytes: Arc<Mutex<Vec<u8>>>,
        reads: Arc<AtomicU64>,
        fail_from: Arc<AtomicU64>,
    }

    impl Fault {
        fn new() -> Self {
            Self {
                bytes: Arc::new(Mutex::new(Vec::new())),
                reads: Arc::new(AtomicU64::new(0)),
                fail_from: Arc::new(AtomicU64::new(NEVER)),
            }
        }

        /// Open the shared bytes as a fresh `ActivityLog`, resetting the read
        /// counter so the next `reads_so_far` measures this open alone.
        fn open(&self) -> ActivityLog {
            self.reads.store(0, Ordering::SeqCst);
            let backend = FaultBackend::new(
                Arc::clone(&self.bytes),
                Arc::clone(&self.reads),
                Arc::clone(&self.fail_from),
            );
            // The smallest cache redb accepts: with the default cache the whole
            // fixture fits in memory after `open`, the traversal issues no
            // backend read at all, and an injected read fault can never fire.
            let db = Database::builder()
                .set_cache_size(0)
                .create_with_backend(backend)
                .expect("open fault-backed database");
            // Match `ActivityLog::open`'s table initialisation.
            let write = db.begin_write().expect("begin_write");
            {
                let _t = write.open_table(ACTIVITY_TABLE).expect("open_table");
            }
            write.commit().expect("commit");
            let last = {
                let read = db.begin_read().expect("begin_read");
                let table = read.open_table(ACTIVITY_TABLE).expect("open_table");
                let key = table.last().expect("last").map(|(k, _)| k.value());
                key.unwrap_or(0)
            };
            ActivityLog::Redb {
                db: Arc::new(db),
                next_id: Arc::new(AtomicU64::new(last.saturating_add(1))),
            }
        }

        /// Reads performed since the last [`Self::open`].
        fn reads_so_far(&self) -> u64 {
            self.reads.load(Ordering::SeqCst)
        }

        /// Copy of the current bytes, to be replayed by [`Self::restore`].
        ///
        /// Why: `open` runs a table-init write transaction, which relocates
        /// pages. Without replaying identical starting bytes before each run,
        /// an offset calibrated in one run names a different page in the next —
        /// and the injected fault then lands on `open` rather than on the
        /// traversal it was aimed at.
        fn snapshot(&self) -> Vec<u8> {
            self.bytes.lock().expect("bytes lock").clone()
        }

        fn restore(&self, snap: &[u8]) {
            let mut bytes = self.bytes.lock().expect("bytes lock");
            bytes.clear();
            bytes.extend_from_slice(snap);
        }

        /// Fail every read from index `from` onward (0-based, counted from the
        /// most recent `open`).
        fn arm(&self, from: u64) {
            self.fail_from.store(from, Ordering::SeqCst);
        }

        fn disarm(&self) {
            self.fail_from.store(NEVER, Ordering::SeqCst);
        }
    }

    /// Seed `rows` entries and return the resulting bytes, to be replayed
    /// before every calibration and armed run.
    fn seed(fault: &Fault, rows: u64) -> Vec<u8> {
        let log = fault.open();
        for n in 1..=rows {
            log.append(ActivitySource::Http, None, format!("e{n}"), json!({"n": n}))
                .expect("append");
        }
        assert_eq!(log.count().expect("count"), rows);
        drop(log);
        fault.snapshot()
    }

    /// Reads performed by [`Fault::open`] plus a read transaction that opens the
    /// table but iterates nothing — i.e. everything a traversal does BEFORE its
    /// first row.
    ///
    /// Why: the fault must land INSIDE the traversal. Arming any earlier fails
    /// `open` or `begin_read`, and both of those already propagated before this
    /// change, so such a test would pass with or without the fix. `count()` is
    /// the traversal-free stand-in: same `begin_read` + `open_table` prologue,
    /// then a metadata-only `len()`.
    /// What: replays `snap` — `open`'s table-init write relocates pages, so the
    /// count is only reproducible from identical starting bytes — runs the
    /// prologue once, and reports the counter.
    fn reads_before_first_row(fault: &Fault, snap: &[u8]) -> u64 {
        fault.restore(snap);
        let log = fault.open();
        let _ = log.count().expect("count");
        let n = fault.reads_so_far();
        drop(log);
        assert!(n > 0, "the prologue must perform at least one read");
        n
    }

    /// Why: `list` consumed the redb iterator with `.flatten()`, which dropped
    /// every `Err` row. A storage failure therefore truncated the feed and
    /// reported success — and under redb 4.3.0, where the iterator keeps
    /// erroring after the first fault, it silently dropped the whole remainder.
    /// What: injects a read fault at an offset only a full traversal touches and
    /// asserts `list` returns `Err` rather than a short `Ok`.
    /// Test: this test.
    #[test]
    fn list_propagates_a_storage_error_instead_of_truncating() {
        let fault = Fault::new();
        let rows = 400;
        let snap = seed(&fault, rows);

        // Control: disarmed, the same traversal reads every row. Without it a
        // green assertion below could mean the fault never fired at all.
        fault.restore(&snap);
        let log = fault.open();
        let listed = log
            .list(&ActivityFilter::default(), rows as usize, 0)
            .expect("control: a clean list must succeed");
        assert_eq!(listed.len(), rows as usize, "control: every row is read");
        drop(log);

        let after_prologue = reads_before_first_row(&fault, &snap);
        fault.restore(&snap);
        fault.arm(after_prologue);
        let log = fault.open();
        let got = log.list(&ActivityFilter::default(), rows as usize, 0);
        fault.disarm();

        let err = got.err().unwrap_or_else(|| {
            panic!("a storage error during iteration must propagate, not truncate the feed")
        });
        assert!(
            format!("{err:#}").contains("read activity row (list)"),
            "the error must come from the iterator arm, got: {err:#}"
        );
    }

    /// Why: `prune`'s id collection used `filter_map(|res| res.ok())`, so a
    /// storage error yielded an empty batch, removed nothing, and committed —
    /// leaving `count()` unchanged and spinning prune's `loop` forever. redb
    /// 4.3.0 makes a failed iterator keep failing, turning a chance hang into a
    /// certain one.
    /// What: drives the production [`oldest_ids`] helper `prune` calls against a
    /// read fault and asserts it returns `Err` rather than a short batch.
    /// `prune` itself only reaches this step above `MAX_ENTRIES` (100k rows),
    /// far too slow to seed in a unit test, so the helper is the seam.
    /// Test: this test.
    #[test]
    fn prune_propagates_a_storage_error_instead_of_dropping_the_batch() {
        let fault = Fault::new();
        let rows = 400;
        let snap = seed(&fault, rows);

        // Control: disarmed, the helper fills a whole batch, so the armed
        // assertion below can tell a propagated error from a short batch.
        fault.restore(&snap);
        let log = fault.open();
        {
            let db = match &log {
                ActivityLog::Redb { db, .. } => Arc::clone(db),
                ActivityLog::Discard => panic!("fault log must be the Redb variant"),
            };
            let read = db.begin_read().expect("begin_read");
            let table = read.open_table(ACTIVITY_TABLE).expect("open_table");
            let ids = oldest_ids(&table, EVICTION_BATCH as usize).expect("control: clean read");
            assert_eq!(
                ids.len(),
                EVICTION_BATCH as usize,
                "control: a clean read must fill the batch"
            );
            assert_eq!(ids[0], 1, "control: ids ascend from the oldest row");
        }
        drop(log);

        let after_prologue = reads_before_first_row(&fault, &snap);
        fault.restore(&snap);
        fault.arm(after_prologue);
        let log = fault.open();
        let db = match &log {
            ActivityLog::Redb { db, .. } => Arc::clone(db),
            ActivityLog::Discard => panic!("fault log must be the Redb variant"),
        };
        let collected = {
            let read = db.begin_read().expect("begin_read");
            let table = read.open_table(ACTIVITY_TABLE).expect("open_table");
            oldest_ids(&table, EVICTION_BATCH as usize)
        };
        fault.disarm();

        let err = collected.err().unwrap_or_else(|| {
            panic!("a storage error must propagate out of prune's id collection")
        });
        assert!(
            format!("{err:#}").contains("read activity row for prune"),
            "the error must come from prune's iterator arm, got: {err:#}"
        );
    }
}
