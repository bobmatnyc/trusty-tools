//! Regression tests for the kg.redb write-transaction deadline (#8749).
//!
//! Why: redb admits one writer per file. Before #8749 a slow `apply_batch`
//! held that lock for as long as it ran — including after #6366's pipeline
//! ceiling had given up on it — and every other writer waited behind it. The
//! properties that matter: the stalled transaction rolls back (its partial
//! writes are absent), the caller is told so with a typed error, and the next
//! writer gets the lock within a bounded time.
//! What: drives real on-disk stores; the `after_batch_op` seam makes one
//! writer's ops slow while the other writer's are not.
//! Test: this file IS the test.

use super::*;
use crate::memory_core::store::kg_writer::KgWriter;
use crate::memory_core::store::write_deadline::WriteTxnError;
use chrono::Utc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long a stalled batch may hold the lock in these tests.
const BUDGET: Duration = Duration::from_millis(150);
/// How long each op of the stalled writer takes.
const OP_STALL: Duration = Duration::from_millis(40);
/// Ops in the stalled batch: without the deadline it holds the lock for about
/// `STALLED_OPS * OP_STALL` = 2.56 s.
const STALLED_OPS: usize = 64;
/// How long the second writer may wait. Well above `BUDGET + OP_STALL`, well
/// below the 2.56 s an unbounded stalled batch holds the lock.
const SECOND_WRITER_BOUND: Duration = Duration::from_millis(1500);

fn open_kg() -> (tempfile::TempDir, KgStoreRedb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let kg = KgStoreRedb::open(&dir.path().join("kg.redb")).expect("open kg.redb");
    (dir, kg)
}

fn assert_op(subject: &str) -> BatchWriteOp {
    BatchWriteOp::Assert(Triple {
        subject: subject.into(),
        predicate: "p".into(),
        object: "o".into(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    })
}

fn set_budget(kg: &KgStoreRedb, budget: Option<Duration>) {
    *kg.test_hooks().txn_budget.lock().expect("hook lock") = budget;
}

/// Poll `cond` every 2 ms until it holds or `bound` elapses.
fn wait_until(bound: Duration, cond: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    cond()
}

fn deadline_error(err: &anyhow::Error) -> Option<&WriteTxnError> {
    err.downcast_ref::<WriteTxnError>()
}

/// Why (#8749): the symptom — a writer that stalls keeps redb's write lock and
/// every other writer waits for as long as it runs.
/// What: writer A runs a 64-op batch whose every op takes 40 ms under a 150 ms
/// budget; writer B starts once A holds the lock. B must finish inside
/// `SECOND_WRITER_BOUND`, A must fail with `DeadlineExceeded`, and none of A's
/// rows may be present while B's is.
/// Test: itself.
#[test]
fn a_stalled_batch_rolls_back_and_the_next_writer_proceeds() {
    let (_dir, kg) = open_kg();
    set_budget(&kg, Some(BUDGET));
    let entered = Arc::new(AtomicBool::new(false));
    {
        let entered = Arc::clone(&entered);
        *kg.test_hooks().after_batch_op.lock().expect("hook lock") =
            Some(Arc::new(move |op: &BatchWriteOp| {
                if let BatchWriteOp::Assert(t) = op
                    && t.subject.starts_with("stalled")
                {
                    entered.store(true, Ordering::SeqCst);
                    std::thread::sleep(OP_STALL);
                }
            }));
    }

    let stalled: Vec<BatchWriteOp> = (0..STALLED_OPS)
        .map(|i| assert_op(&format!("stalled-{i}")))
        .collect();
    let writer_a = {
        let kg = kg.clone();
        std::thread::spawn(move || kg.apply_batch(&stalled))
    };
    assert!(
        wait_until(Duration::from_secs(5), || entered.load(Ordering::SeqCst)),
        "writer A never entered its stalled batch"
    );

    let started = Instant::now();
    let writer_b = {
        let kg = kg.clone();
        std::thread::spawn(move || kg.apply_batch(&[assert_op("second")]))
    };
    let finished = wait_until(SECOND_WRITER_BOUND, || writer_b.is_finished());
    let waited = started.elapsed();
    assert!(
        finished,
        "#8749: the second writer waited {waited:?} (> {SECOND_WRITER_BOUND:?}) \
         behind a stalled transaction"
    );

    writer_b
        .join()
        .expect("writer B thread")
        .expect("the second writer must commit");
    let err = writer_a
        .join()
        .expect("writer A thread")
        .expect_err("#8749: a batch past its deadline must fail, not commit");
    assert!(
        matches!(
            deadline_error(&err),
            Some(WriteTxnError::DeadlineExceeded {
                store: "kg.redb",
                ..
            })
        ),
        "the failure must be the typed deadline error: {err:#}"
    );
    assert!(
        kg.query_active("stalled-0").expect("query").is_empty(),
        "#8749: the rolled-back batch's partial writes must be absent"
    );
    assert_eq!(kg.query_active("second").expect("query").len(), 1);
}

/// Why (#8749, after #8729): a stall that is rolled back silently leaves the
/// operator nothing to correlate a failed write with. The abort must be logged
/// at warn, naming the palace and how long the transaction held the lock.
/// What: opens kg.redb under `<tmp>/stall-palace/`, stalls one op 60 ms under a
/// 20 ms budget, runs `apply_batch` on this thread under log capture, and
/// checks the warn line.
/// Test: itself.
#[test]
fn a_stalled_commit_is_logged_with_the_palace_and_the_lock_hold() {
    let dir = tempfile::tempdir().expect("tempdir");
    let palace_dir = dir.path().join("stall-palace");
    std::fs::create_dir_all(&palace_dir).expect("palace dir");
    let kg = KgStoreRedb::open(&palace_dir.join("kg.redb")).expect("open kg.redb");
    set_budget(&kg, Some(Duration::from_millis(20)));
    *kg.test_hooks().after_batch_op.lock().expect("hook lock") =
        Some(Arc::new(|_: &BatchWriteOp| {
            std::thread::sleep(Duration::from_millis(60))
        }));

    let (outcome, lines) =
        crate::log_buffer::capture_logs(|| kg.apply_batch(&[assert_op("logged")]));
    let err = outcome.expect_err("#8749: the stalled batch must not commit");
    assert!(deadline_error(&err).is_some(), "{err:#}");
    let warn = lines
        .iter()
        .find(|l| l.contains("WARN") && l.contains("#8749"))
        .unwrap_or_else(|| panic!("#8749: no warn line for the abort: {lines:#?}"));
    assert!(warn.contains("stall-palace"), "names the palace: {warn}");
    assert!(warn.contains("held_ms="), "names the lock hold: {warn}");
    assert!(kg.query_active("logged").expect("query").is_empty());
}

/// Why (#8749, Fail-Open Check): an abort reported as success — or flattened
/// into a string no caller can recognise — is the silent failure this fix must
/// not introduce. Every op queued in the aborted batch has to hear about it.
/// What: under a zero budget, two ops through the coalescing `KgWriter` actor
/// must each fail with a downcastable `WriteTxnError`, and neither may land;
/// with the budget restored, the next write lands (the lock was released).
/// Test: itself.
#[tokio::test]
async fn a_deadline_abort_reaches_every_queued_caller_as_a_typed_error() {
    let (_dir, kg) = open_kg();
    let store = Arc::new(kg);
    let writer = KgWriter::spawn(Arc::clone(&store));
    set_budget(&store, Some(Duration::ZERO));

    let t = |subject: &str| match assert_op(subject) {
        BatchWriteOp::Assert(t) => t,
        _ => unreachable!("assert_op builds an Assert"),
    };
    let (a, b) = tokio::join!(writer.assert(t("aborted-a")), writer.assert(t("aborted-b")));
    for (name, res) in [("a", a), ("b", b)] {
        let err = res.expect_err("#8749: an over-deadline write must not report success");
        assert!(
            deadline_error(&err).is_some(),
            "caller {name} must get the typed deadline error: {err:#}"
        );
    }
    assert!(store.query_active("aborted-a").expect("query").is_empty());
    assert!(store.query_active("aborted-b").expect("query").is_empty());

    set_budget(&store, None);
    writer
        .assert(t("after"))
        .await
        .expect("the lock must be free once the aborted batch rolled back");
    assert_eq!(store.query_active("after").expect("query").len(), 1);
}
