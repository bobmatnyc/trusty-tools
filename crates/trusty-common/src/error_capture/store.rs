//! Local durable store for captured error records.
//!
//! Why: Error records must survive process restarts so Phase 2 can surface
//! errors that occurred in a prior daemon run. We use JSON-Lines (JSONL) over
//! SQLite to keep the dep surface minimal — `rusqlite` is already optional in
//! `trusty-common` but adding it to `bug-capture` would pull heavy deps into
//! every consumer. JSONL + a ring buffer provides the same queryable contract
//! with zero new transitive deps. Store path: OS data dir / app_name /
//! errors.jsonl (macOS Application Support, Linux ~/.local/share). Override
//! the base dir with `TRUSTY_DATA_DIR_OVERRIDE` for tests. Phase 2 may
//! migrate to SQLite; the public API is designed so that is localised here.
//!
//! What: [`ErrorStore`] wraps a bounded `VecDeque` (ring buffer, same eviction
//! pattern as `LogBuffer`) plus a path to the JSONL append file. Every
//! `append` call pushes to the ring and, best-effort, appends one JSON line to
//! disk. The file is size-capped and rotated by [`super::rotation`] (#8028).
//! IO errors print to stderr and are swallowed — they must never panic or
//! propagate into the tracing hot path. `recent_errors` and
//! `errors_by_fingerprint` read from the ring; `load_from_disk` re-populates
//! it on daemon restart.
//!
//! Test: `store_ring_bounded`, `store_round_trip_write_read`,
//! `store_handles_missing_file_gracefully`, `store_corrupt_line_skipped`.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::error_capture::rotation::{self, RotationPolicy};
use crate::error_capture::types::CapturedError;

/// Default ring-buffer capacity (records). Mirrors `DEFAULT_LOG_CAPACITY` in
/// `log_buffer` — enough for a few minutes of busy daemon activity at <1 MB.
pub const DEFAULT_CAPTURE_CAPACITY: usize = 500;

/// File name within the app's data directory for the JSONL error log.
const ERRORS_FILENAME: &str = "errors.jsonl";

/// Thread-safe, bounded ring buffer of [`CapturedError`] records plus a
/// backing JSONL append file for persistence across restarts.
///
/// Why: the ring keeps hot-path queries O(1) in memory while the JSONL file
///      ensures records survive a daemon restart. The combination mirrors the
///      `LogBuffer` pattern already in `trusty-common`.
/// What: `Arc<Mutex<Inner>>` where `Inner` holds a `VecDeque` (ring) and an
///      optional `std::fs::File` in append mode. `append` takes the lock,
///      pushes to the deque, and writes one JSON line to disk. `recent_errors`
///      clones out the last `n` entries. All IO failures degrade silently to
///      stderr.
/// Test: `store_ring_bounded`, `store_round_trip_write_read`.
#[derive(Clone)]
pub struct ErrorStore {
    inner: Arc<Mutex<Inner>>,
    capacity: usize,
}

struct Inner {
    ring: VecDeque<CapturedError>,
    file_path: Option<PathBuf>,
    // #8028: size cap and retention for `file_path`.
    policy: RotationPolicy,
    // #8028: disk writes refused because a due rotation failed.
    refused_disk_writes: u64,
    // #8028: malformed lines skipped when the ring was loaded from disk.
    corrupt_lines_skipped: u64,
}

impl ErrorStore {
    /// Open (or create) the error store for the named app.
    ///
    /// Why: called once at daemon startup; the store is then shared via `Arc`
    ///      clone between the tracing layer and the query API.
    /// What: resolves the app data dir, opens `errors.jsonl` in append mode,
    ///      and loads existing records into the ring buffer via
    ///      `ErrorStore::load_from_disk`. If the data dir or file cannot be
    ///      opened, the store operates in memory-only mode (ring buffer only).
    ///      Returns an operational store regardless.
    /// Test: `store_round_trip_write_read`.
    #[must_use]
    pub fn open(app_name: &str, capacity: usize) -> Self {
        let file_path = match crate::resolve_data_dir(app_name) {
            Ok(dir) => Some(dir.join(ERRORS_FILENAME)),
            Err(e) => {
                eprintln!("[bug-capture] cannot resolve data dir for {app_name}: {e}");
                None
            }
        };
        Self::with_path(file_path, capacity)
    }

    /// Create an in-memory-only store backed by a specific file path.
    ///
    /// Why: tests need to control the backing file path without going through
    ///      the OS data dir resolution (which calls `NSFileManager` on macOS).
    /// What: builds an `ErrorStore` with the given path as the JSONL file.
    ///      If `path` is `None`, operates in ring-only mode.
    /// Test: all store tests use this constructor.
    #[must_use]
    pub fn with_path(file_path: Option<PathBuf>, capacity: usize) -> Self {
        Self::with_path_and_rotation(file_path, capacity, RotationPolicy::default())
    }

    /// Like [`ErrorStore::with_path`], with an explicit size cap and retention.
    ///
    /// Why: tests need a cap small enough to cross in a few records (#8028).
    /// What: loads the ring from the live file and its rotated siblings, then
    ///      rotates under `policy` on every later append.
    /// Test: `store_tests::writing_past_the_cap_rotates_and_bounds_disk_use`.
    #[must_use]
    pub fn with_path_and_rotation(
        file_path: Option<PathBuf>,
        capacity: usize,
        policy: RotationPolicy,
    ) -> Self {
        let capacity = capacity.max(1);
        let (ring, corrupt_lines_skipped) = match file_path {
            Some(ref path) => load_ring_from_disk(path, capacity, policy),
            None => (VecDeque::with_capacity(capacity), 0),
        };
        let inner = Inner {
            ring,
            file_path,
            policy,
            refused_disk_writes: 0,
            corrupt_lines_skipped,
        };
        Self {
            inner: Arc::new(Mutex::new(inner)),
            capacity,
        }
    }

    /// Append a captured error to the ring buffer and persist it to disk.
    ///
    /// Why: called by `BugCaptureLayer::on_event` on every ERROR event; must
    ///      be non-blocking (no async, short lock hold) and must never panic.
    ///      The file is shared by several processes and must stay bounded
    ///      (#8028).
    /// What: acquires the mutex, rotates the file if it has reached the cap,
    ///      appends the record as one full-line `O_APPEND` write, then pushes
    ///      to the ring (evicting oldest when at capacity). When a due rotation
    ///      fails, the disk write is refused and counted
    ///      ([`ErrorStore::refused_disk_writes`]) so the file cannot grow past
    ///      the cap; the record still enters the ring and the next append
    ///      retries the rotation. IO errors go to stderr, never to the caller.
    /// Test: `store_tests::concurrent_writers_produce_only_well_formed_lines`,
    ///      `store_tests::a_rotation_failure_refuses_the_disk_write_and_counts_it`.
    pub fn append(&self, record: CapturedError) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };

        // Append JSON line to disk before touching the ring so a crash after
        // write but before ring update at worst leaves the file one record
        // ahead of the ring — acceptable for our best-effort guarantees.
        if let Some(path) = guard.file_path.clone() {
            // #8028: fail closed on disk growth, never on logging — a failed
            // rotation refuses this write instead of appending past the cap.
            if let Err(e) = rotation::rotate_if_due(&path, guard.policy) {
                guard.refused_disk_writes += 1;
                eprintln!(
                    "[bug-capture] rotating {} failed ({e}); record kept in memory only, {} disk writes refused",
                    path.display(),
                    guard.refused_disk_writes
                );
            } else if let Err(e) = serialise_and_append(&path, &record) {
                eprintln!("[bug-capture] write to {}: {e}", path.display());
            }
        }

        guard.ring.push_back(record);
        while guard.ring.len() > self.capacity {
            guard.ring.pop_front();
        }
    }

    /// Return the `n` most recent captured errors (oldest-first within the
    /// result slice).
    ///
    /// Why: Phase 2 `list_recent_errors` MCP tool calls this; returning an
    ///      owned `Vec` keeps the lock held for the minimum duration.
    /// What: clones the last `n` elements of the ring into a `Vec`. If `n`
    ///      exceeds the ring length, all records are returned.
    /// Test: `store_round_trip_write_read`.
    #[must_use]
    pub fn recent_errors(&self, n: usize) -> Vec<CapturedError> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let skip = guard.ring.len().saturating_sub(n);
        guard.ring.iter().skip(skip).cloned().collect()
    }

    /// Aggregate errors by fingerprint, returning each unique fingerprint
    /// with its occurrence count and the most recent record.
    ///
    /// Why: Phase 2 can present a deduplicated list — "this error happened N
    ///      times" — rather than a raw chronological log, which is far more
    ///      actionable for bug filing.
    /// What: iterates the ring in order (oldest → newest); the most-recent
    ///      record per fingerprint wins. Returns `Vec<(CapturedError, usize)>`
    ///      sorted by count descending.
    /// Test: `store_errors_by_fingerprint`.
    #[must_use]
    pub fn errors_by_fingerprint(&self) -> Vec<(CapturedError, usize)> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        // latest_record keeps the most-recent CapturedError per fingerprint.
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut latest: HashMap<String, CapturedError> = HashMap::new();
        for rec in &guard.ring {
            *counts.entry(rec.fingerprint.clone()).or_insert(0) += 1;
            latest.insert(rec.fingerprint.clone(), rec.clone());
        }
        let mut result: Vec<(CapturedError, usize)> = latest
            .into_iter()
            .map(|(fp, rec)| (rec, *counts.get(&fp).unwrap_or(&1)))
            .collect();
        result.sort_by_key(|item| std::cmp::Reverse(item.1));
        result
    }

    /// Total number of records currently in the ring.
    ///
    /// Why: callers (tests, status endpoints) need to know how many records
    ///      are buffered without cloning them all out.
    /// What: returns the deque length under the mutex.
    /// Test: `store_ring_bounded`.
    #[must_use]
    pub fn len(&self) -> usize {
        match self.inner.lock() {
            Ok(g) => g.ring.len(),
            Err(p) => p.into_inner().ring.len(),
        }
    }

    /// Whether the ring buffer is empty.
    ///
    /// Why: clippy requires `is_empty` alongside `len`.
    /// What: `len() == 0`.
    /// Test: `store_ring_bounded`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Disk writes refused because a due rotation failed (#8028).
    ///
    /// Test: `store_tests::a_rotation_failure_refuses_the_disk_write_and_counts_it`.
    #[must_use]
    pub fn refused_disk_writes(&self) -> u64 {
        match self.inner.lock() {
            Ok(g) => g.refused_disk_writes,
            Err(p) => p.into_inner().refused_disk_writes,
        }
    }

    /// Malformed lines skipped while loading the ring from disk (#8028).
    ///
    /// Test: `store_corrupt_line_skipped`.
    #[must_use]
    pub fn corrupt_lines_skipped(&self) -> u64 {
        match self.inner.lock() {
            Ok(g) => g.corrupt_lines_skipped,
            Err(p) => p.into_inner().corrupt_lines_skipped,
        }
    }

    /// Read records from an explicit JSONL file path without a live store handle.
    ///
    /// Why: Phase 2's multi-store reader needs to load records from several
    ///      daemon JSONL files (trusty-search, trusty-memory, trusty-mpm, …)
    ///      and merge them in-process without opening those files in append mode
    ///      or holding locks across daemon boundaries — a snapshot read.
    /// What: reads the JSONL at `path` and its rotated siblings under the
    ///      default [`RotationPolicy`], keeping the newest `limit` records
    ///      oldest-first, and skips corrupt lines. Returns an empty `Vec` when
    ///      no file exists — never an error.
    /// Test: `read_records_loads_file`, `read_records_missing_file_is_empty`,
    ///      `store_tests::malformed_line_does_not_break_reading`.
    #[must_use]
    pub fn read_records(path: &Path, limit: usize) -> Vec<CapturedError> {
        load_ring_from_disk(path, limit, RotationPolicy::default())
            .0
            .into_iter()
            .collect()
    }
}

// ── Disk helpers ──────────────────────────────────────────────────────────────

/// Load the newest `capacity` records from a store's live and rotated files.
///
/// Why: on daemon restart the ring must be pre-populated from the persistent
///      store so `recent_errors` reflects prior runs, not just the current
///      session — including records a rotation just moved to `.1` (#8028).
/// What: walks the files newest-first, reading at most
///      [`RotationPolicy::read_limit`] bytes of each, and fills the ring from
///      the front until it holds `capacity` records (oldest first, newest
///      last). Lines are split on raw `\n` bytes and parsed one at a time, so
///      a malformed line — bad JSON or invalid UTF-8 — is skipped and counted
///      and never fails the rest of the file. Returns the ring and the count.
/// Test: `store_round_trip_write_read`, `store_corrupt_line_skipped`,
///      `store_tests::malformed_line_does_not_break_reading`.
fn load_ring_from_disk(
    path: &Path,
    capacity: usize,
    policy: RotationPolicy,
) -> (VecDeque<CapturedError>, u64) {
    let mut ring: VecDeque<CapturedError> = VecDeque::with_capacity(capacity);
    let mut skipped_total = 0u64;
    for file in rotation::files_newest_first(path, policy) {
        if ring.len() >= capacity {
            break;
        }
        let bytes = match rotation::read_tail(&file, policy.read_limit()) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                eprintln!("[bug-capture] cannot read {}: {e}", file.display());
                continue;
            }
        };
        let mut skipped = 0u64;
        let records: Vec<CapturedError> = bytes
            .split(|b| *b == b'\n')
            .filter(|line| !line.trim_ascii().is_empty())
            .filter_map(|line| {
                // #8028: one bad line costs that line, never the whole file.
                let parsed = serde_json::from_slice(line.trim_ascii()).ok();
                skipped += u64::from(parsed.is_none());
                parsed
            })
            .collect();
        for rec in records.into_iter().rev() {
            if ring.len() >= capacity {
                break;
            }
            ring.push_front(rec);
        }
        if skipped > 0 {
            eprintln!(
                "[bug-capture] skipped {skipped} corrupt record(s) in {}",
                file.display()
            );
        }
        skipped_total += skipped;
    }
    (ring, skipped_total)
}

/// Serialise one record as a JSON line and append it to the given path.
///
/// Why: several processes append to one file with no shared mutex. Writing
///      the body and the `\n` as two `write(2)` calls let another process's
///      body land between them and fuse two records into one corrupt line
///      (#8028). We open the file fresh each write to avoid holding an
///      `std::fs::File` across the mutex boundary.
/// What: builds the JSON bytes plus `\n` in one buffer and writes it with a
///      single `O_APPEND` `write_all`, so each record lands whole at the end of
///      the file. Returns `Err` on any IO failure; the caller logs to stderr.
/// Test: `store_tests::concurrent_writers_produce_only_well_formed_lines`.
fn serialise_and_append(path: &Path, record: &CapturedError) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push(b'\n');
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    // #8028: one write per record — a split write interleaves across processes.
    file.write_all(&line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error_capture::types::CapturedError;

    fn make_record(msg: &str, fp: &str) -> CapturedError {
        CapturedError {
            timestamp_secs: 1_000_000,
            crate_target: "test_crate".to_string(),
            crate_version: "0.1.0".to_string(),
            message: msg.to_string(),
            fields: String::new(),
            file: Some("src/lib.rs".to_string()),
            line: Some(10),
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            fingerprint: fp.to_string(),
        }
    }

    #[test]
    fn store_ring_bounded() {
        let store = ErrorStore::with_path(None, 3);
        assert!(store.is_empty());
        for i in 0..5u32 {
            store.append(make_record(&format!("err {i}"), &format!("fp{i}")));
        }
        // Ring cap = 3 → only last 3 survive.
        assert_eq!(store.len(), 3);
        let recent = store.recent_errors(10);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].message, "err 2");
        assert_eq!(recent[2].message, "err 4");
    }

    #[test]
    fn store_round_trip_write_read() {
        let tmp_dir = {
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            std::env::temp_dir().join(format!("bugcap-test-{pid}-{nanos}"))
        };
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let file_path = tmp_dir.join(ERRORS_FILENAME);

        // Write two records via the store.
        {
            let store = ErrorStore::with_path(Some(file_path.clone()), 10);
            store.append(make_record("first error", "fp1"));
            store.append(make_record("second error", "fp2"));
            assert_eq!(store.len(), 2);
        }

        // Re-open the store — it must reload from JSONL.
        let store2 = ErrorStore::with_path(Some(file_path), 10);
        let records = store2.recent_errors(10);
        assert_eq!(records.len(), 2, "expected 2 records after reload");
        assert_eq!(records[0].message, "first error");
        assert_eq!(records[1].message, "second error");
    }

    #[test]
    fn store_handles_missing_file_gracefully() {
        // Use a unique path per test run so previous runs don't leave
        // residual files that make the "empty store" assertion fail.
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let nonexistent = std::env::temp_dir().join(format!("bugcap-missing-{pid}-{nanos}.jsonl"));
        let store = ErrorStore::with_path(Some(nonexistent), 10);
        // Missing file → empty store, no panic.
        assert!(store.is_empty());
        // Appending should still work (file will be created).
        store.append(make_record("hello", "fp1"));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn store_corrupt_line_skipped() {
        let tmp_dir = {
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            std::env::temp_dir().join(format!("bugcap-corrupt-{pid}-{nanos}"))
        };
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let file_path = tmp_dir.join(ERRORS_FILENAME);

        // Write a valid record, then corrupt bytes, then another valid record.
        {
            let valid = serde_json::to_string(&make_record("valid first", "fp1")).unwrap();
            let valid2 = serde_json::to_string(&make_record("valid second", "fp2")).unwrap();
            let content = format!("{valid}\nnot-json-at-all\n{valid2}\n");
            std::fs::write(&file_path, content).unwrap();
        }

        let store = ErrorStore::with_path(Some(file_path), 10);
        // Only 2 valid records should be loaded; corrupt line skipped.
        assert_eq!(store.len(), 2, "corrupt line should be skipped");
        assert_eq!(
            store.corrupt_lines_skipped(),
            1,
            "#8028: the skip is counted"
        );
        let records = store.recent_errors(10);
        assert_eq!(records[0].message, "valid first");
        assert_eq!(records[1].message, "valid second");
    }

    #[test]
    fn store_errors_by_fingerprint() {
        let store = ErrorStore::with_path(None, 20);
        // Three records: two share fingerprint "fp1", one is "fp2".
        store.append(make_record("err a", "fp1"));
        store.append(make_record("err b", "fp2"));
        store.append(make_record("err c", "fp1")); // same fingerprint, later record

        let by_fp = store.errors_by_fingerprint();
        assert_eq!(by_fp.len(), 2, "expected 2 unique fingerprints");
        // fp1 has count 2 — should be first (sorted by count desc).
        assert_eq!(by_fp[0].1, 2);
        assert_eq!(by_fp[0].0.fingerprint, "fp1");
        // The most-recent record for fp1 should be "err c".
        assert_eq!(by_fp[0].0.message, "err c");
        // fp2 has count 1.
        assert_eq!(by_fp[1].1, 1);
    }

    #[test]
    fn read_records_loads_file() {
        // Write a two-record JSONL file, then read it back via the static helper.
        let tmp_dir = {
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            std::env::temp_dir().join(format!("bugcap-readrec-{pid}-{nanos}"))
        };
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let file_path = tmp_dir.join(ERRORS_FILENAME);

        // Seed the file with two records via the store's append path.
        let store = ErrorStore::with_path(Some(file_path.clone()), 10);
        store.append(make_record("alpha error", "fp-a"));
        store.append(make_record("beta error", "fp-b"));

        // Static read-back must return both records.
        let records = ErrorStore::read_records(&file_path, 10);
        assert_eq!(records.len(), 2, "expected 2 records");
        assert_eq!(records[0].message, "alpha error");
        assert_eq!(records[1].message, "beta error");
    }

    #[test]
    fn read_records_missing_file_is_empty() {
        // A missing file must return an empty vec — not an error.
        let nonexistent = std::env::temp_dir().join("bugcap-no-such-file-x99.jsonl");
        let records = ErrorStore::read_records(&nonexistent, 50);
        assert!(records.is_empty(), "missing file must yield empty vec");
    }
}
