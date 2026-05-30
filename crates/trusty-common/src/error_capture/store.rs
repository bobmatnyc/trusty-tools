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
//! disk. IO errors print to stderr and are swallowed — they must never panic or
//! propagate into the tracing hot path. `recent_errors` and
//! `errors_by_fingerprint` read from the ring; `load_from_disk` re-populates
//! it on daemon restart.
//!
//! Test: `store_ring_bounded`, `store_round_trip_write_read`,
//! `store_handles_missing_file_gracefully`, `store_corrupt_line_skipped`.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
}

impl ErrorStore {
    /// Open (or create) the error store for the named app.
    ///
    /// Why: called once at daemon startup; the store is then shared via `Arc`
    ///      clone between the tracing layer and the query API.
    /// What: resolves the app data dir, opens `errors.jsonl` in append mode,
    ///      and loads existing records into the ring buffer via
    ///      [`ErrorStore::load_from_disk`]. If the data dir or file cannot be
    ///      opened, the store operates in memory-only mode (ring buffer only).
    ///      Returns an operational store regardless.
    /// Test: `store_round_trip_write_read`.
    #[must_use]
    pub fn open(app_name: &str, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let file_path = match crate::resolve_data_dir(app_name) {
            Ok(dir) => Some(dir.join(ERRORS_FILENAME)),
            Err(e) => {
                eprintln!("[bug-capture] cannot resolve data dir for {app_name}: {e}");
                None
            }
        };

        let ring = if let Some(ref path) = file_path {
            load_ring_from_disk(path, capacity)
        } else {
            VecDeque::with_capacity(capacity)
        };

        Self {
            inner: Arc::new(Mutex::new(Inner { ring, file_path })),
            capacity,
        }
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
        let capacity = capacity.max(1);
        let ring = if let Some(ref path) = file_path {
            load_ring_from_disk(path, capacity)
        } else {
            VecDeque::with_capacity(capacity)
        };
        Self {
            inner: Arc::new(Mutex::new(Inner { ring, file_path })),
            capacity,
        }
    }

    /// Append a captured error to the ring buffer and persist it to disk.
    ///
    /// Why: called by `BugCaptureLayer::on_event` on every ERROR event; must
    ///      be non-blocking (no async, short lock hold) and must never panic.
    /// What: acquires the mutex, pushes to the ring (evicting oldest when at
    ///      capacity), then serialises to JSON and appends one line to the open
    ///      file. IO errors are printed to stderr and discarded.
    /// Test: `store_round_trip_write_read`.
    pub fn append(&self, record: CapturedError) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };

        // Append JSON line to disk before touching the ring so a crash after
        // write but before ring update at worst leaves the file one record
        // ahead of the ring — acceptable for our best-effort guarantees.
        if let Some(ref path) = guard.file_path {
            match serialise_and_append(path, &record) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("[bug-capture] write to {}: {e}", path.display());
                }
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

    /// Read records from an explicit JSONL file path without a live store handle.
    ///
    /// Why: Phase 2's multi-store reader needs to load records from several
    ///      daemon JSONL files (trusty-search, trusty-memory, trusty-mpm, …)
    ///      and merge them in-process without opening those files in append mode
    ///      or holding locks across daemon boundaries — a snapshot read.
    /// What: reads the JSONL at `path`, parses up to `limit` records (ring
    ///      eviction keeps the last `limit` lines), skips corrupt lines.
    ///      Returns an empty `Vec` when the file is absent — never an error.
    /// Test: `read_records_loads_file`, `read_records_missing_file_is_empty`.
    #[must_use]
    pub fn read_records(path: &std::path::Path, limit: usize) -> Vec<CapturedError> {
        load_ring_from_disk(&path.to_path_buf(), limit)
            .into_iter()
            .collect()
    }
}

// ── Disk helpers ──────────────────────────────────────────────────────────────

/// Load up to `capacity` records from a JSONL file into a `VecDeque`.
///
/// Why: on daemon restart the ring must be pre-populated from the persistent
///      store so `recent_errors` reflects prior runs, not just the current
///      session.
/// What: reads the file line-by-line; skips blank lines and lines that fail
///      JSON deserialisation (corrupt / truncated); keeps the last `capacity`
///      records (oldest lines first, newest lines last). Returns an empty deque
///      if the file is absent or unreadable — never panics.
/// Test: `store_round_trip_write_read`, `store_corrupt_line_skipped`.
fn load_ring_from_disk(path: &PathBuf, capacity: usize) -> VecDeque<CapturedError> {
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return VecDeque::with_capacity(capacity);
        }
        Err(e) => {
            eprintln!("[bug-capture] cannot read {}: {e}", path.display());
            return VecDeque::with_capacity(capacity);
        }
    };

    let mut ring: VecDeque<CapturedError> = VecDeque::with_capacity(capacity);
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<CapturedError>(line) {
            Ok(rec) => {
                ring.push_back(rec);
                while ring.len() > capacity {
                    ring.pop_front();
                }
            }
            Err(_) => {
                // Corrupt or partial line — skip silently. The line is already
                // written; we don't truncate the file (it may have more valid
                // lines after this one in edge cases).
                eprintln!(
                    "[bug-capture] skipping corrupt record in {}",
                    path.display()
                );
            }
        }
    }
    ring
}

/// Serialise one record as a JSON line and append it to the given path.
///
/// Why: append-only write keeps the file valid JSONL even if the process is
///      killed mid-write of a different line. We open the file fresh each
///      write to avoid holding an `std::fs::File` across the mutex boundary
///      (which would require `Send + Sync` on `File` in `Inner`).
/// What: opens in append+create mode, writes the JSON bytes + `\n`, flushes,
///      closes. Returns `Err` on any IO failure; the caller logs to stderr.
/// Test: `store_round_trip_write_read`.
fn serialise_and_append(path: &PathBuf, record: &CapturedError) -> std::io::Result<()> {
    let json = serde_json::to_vec(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    file.write_all(&json)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
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
