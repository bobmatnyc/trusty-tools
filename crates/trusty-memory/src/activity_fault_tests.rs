//! redb storage-fault tests for [`super::ActivityLog`] (#8254).
//!
//! Why: redb 4.3.0 fixed iterators silently omitting data after an error. An
//! iterator that yielded `Err` used to carry on past the unreadable entries;
//! it now keeps returning an error. `list` and `prune` both consumed that
//! `Result` with a combinator that discarded the `Err`, so under 4.3 a storage
//! fault became a truncated feed (or, for `.flatten()`, a scan that never
//! ends). These tests pin the propagation contract.
//! What: an in-memory `StorageBackend` that fails every read from a chosen
//! index onward, aimed past the read-transaction prologue so the fault lands
//! inside the traversal.
//! Test: this IS the test module.

use super::*;
use serde_json::json;
use std::io;
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

/// How long an armed `list` may run before the test calls it a hang.
///
/// Why: under redb 4.3 a failed iterator yields `Err` forever, so the
/// pre-#8254 `.flatten()` never returns. A bounded wait turns that hang into a
/// red assertion instead of a stalled suite.
const LIST_DEADLINE: Duration = Duration::from_secs(20);

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
    fn new(bytes: Arc<Mutex<Vec<u8>>>, reads: Arc<AtomicU64>, fail_from: Arc<AtomicU64>) -> Self {
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
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(log.list(&ActivityFilter::default(), rows as usize, 0));
    });
    let got = rx.recv_timeout(LIST_DEADLINE).unwrap_or_else(|_| {
        panic!("list did not return: the iterator error was swallowed and the scan spun")
    });
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

    let err = collected
        .err()
        .unwrap_or_else(|| panic!("a storage error must propagate out of prune's id collection"));
    assert!(
        format!("{err:#}").contains("read activity row for prune"),
        "the error must come from prune's iterator arm, got: {err:#}"
    );
}
