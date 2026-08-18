//! Unit tests for the bounded background-index dispatcher (issue #2798).
//!
//! Why: isolated in a sibling file (declared via `#[path =
//! "index_dispatch_tests.rs"] mod tests;` in `index_dispatch.rs`) so the
//! production module stays small; as a child module `super::` still reaches its
//! private items.
//!
//! What: pins the three properties the bound is worth having for — jobs never
//! run more than `workers` at a time, submissions past `workers + queue` are
//! rejected and counted, and a panicking job does not retire its worker.
//!
//! Test: `cargo test -p trusty-common --features search-index -- index_dispatch`

use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::channel;
use std::time::Duration;

/// Generous ceiling for "this should have happened by now" waits — long enough
/// that a loaded CI box does not trip it, short enough to fail rather than hang.
const WAIT: Duration = Duration::from_secs(30);

/// Once every worker is busy and the queue is full, further submissions are
/// rejected — not queued, not blocked — and counted.
///
/// Why: this is the saturation contract #2798 asks the fix to STATE. A drop
/// that is not counted is a silent-loss path; a submission that blocked would
/// stall the tool executor that called it.
/// What: a 1-worker/1-slot pool. Job A occupies the worker and reports that it
/// started (so it is provably out of the queue); job B then fills the single
/// queue slot; job C must be rejected, with the counter reading 1. Releasing A
/// lets B run, proving the queued job was kept rather than dropped.
/// Test: this test.
#[test]
fn dispatcher_rejects_submissions_once_workers_and_queue_are_full() {
    let pool = BoundedDispatcher::new(1, 1);

    let (started_tx, started_rx) = channel();
    let (release_tx, release_rx) = channel::<()>();
    assert!(
        pool.try_submit(Box::new(move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv_timeout(WAIT);
        })),
        "an idle pool must accept the first job"
    );
    started_rx
        .recv_timeout(WAIT)
        .expect("the worker never picked up the first job");

    let (ran_tx, ran_rx) = channel();
    assert!(
        pool.try_submit(Box::new(move || {
            let _ = ran_tx.send(());
        })),
        "the single queue slot must accept a second job"
    );

    assert!(
        !pool.try_submit(Box::new(|| {})),
        "a third job must be rejected: the only worker is busy and the queue is full"
    );
    assert_eq!(pool.rejected(), 1, "the rejection must be counted");

    let _ = release_tx.send(());
    ran_rx
        .recv_timeout(WAIT)
        .expect("the queued job never ran after the worker was released");
}

/// The pool never runs more jobs at once than it has workers.
///
/// Why: THE property #2798 is about. Before the fix `index_files_best_effort`
/// spawned one detached thread per batch, so 16 batches meant 16 concurrent
/// threads; this test would observe a peak of 16 against that implementation.
/// What: submits 16 jobs to a 2-worker pool, each recording the live-job count
/// as it enters and holding it for 20ms so overlap is observable; asserts the
/// observed peak never exceeded the worker count and that all 16 still ran.
/// Test: this test.
#[test]
fn no_more_jobs_run_concurrently_than_the_worker_count() {
    const WORKERS: usize = 2;
    const JOBS: usize = 16;

    let pool = BoundedDispatcher::new(WORKERS, JOBS);
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (done_tx, done_rx) = channel();

    for _ in 0..JOBS {
        let live = Arc::clone(&live);
        let peak = Arc::clone(&peak);
        let done = done_tx.clone();
        assert!(
            pool.try_submit(Box::new(move || {
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                live.fetch_sub(1, Ordering::SeqCst);
                let _ = done.send(());
            })),
            "the queue was sized to hold every job"
        );
    }
    drop(done_tx);

    for i in 0..JOBS {
        done_rx
            .recv_timeout(WAIT)
            .unwrap_or_else(|e| panic!("job {i} never completed: {e}"));
    }

    let observed = peak.load(Ordering::SeqCst);
    assert!(
        observed <= WORKERS,
        "{observed} jobs ran concurrently on a {WORKERS}-worker pool"
    );
    assert_eq!(pool.rejected(), 0, "nothing should have been rejected");
}

/// A pool that has never rejected anything says so, distinguishably.
///
/// Why: the health surface has to separate "no drops ever" from "drops right
/// now". If a never-dropped pool reported a timestamp, every consumer would
/// have to guess whether `0` meant the epoch or meant nothing.
/// What: asserts a fresh pool reports zero rejections and `None` for the last
/// drop.
/// Test: this test.
#[test]
fn a_fresh_pool_reports_no_drop_ever() {
    let pool = BoundedDispatcher::new(1, 1);
    assert_eq!(pool.rejected(), 0);
    assert_eq!(pool.last_drop_unix_secs(), None);
}

/// A rejection records when it happened, not just that it did.
///
/// Why: `dropped_batches` alone cannot answer "is saturation happening now",
/// which is the question a health check asks.
/// What: saturates a 1-worker/1-slot pool, forces a rejection, and asserts the
/// recorded unix-second stamp is within a minute of now — i.e. it is a real
/// timestamp of this drop, not a placeholder.
/// Test: this test.
#[test]
fn a_rejection_records_when_it_happened() {
    let pool = BoundedDispatcher::new(1, 1);
    let (started_tx, started_rx) = channel();
    let (release_tx, release_rx) = channel::<()>();
    assert!(pool.try_submit(Box::new(move || {
        let _ = started_tx.send(());
        let _ = release_rx.recv_timeout(WAIT);
    })));
    started_rx.recv_timeout(WAIT).expect("worker never started");
    assert!(pool.try_submit(Box::new(|| {})), "queue slot must accept");
    assert!(!pool.try_submit(Box::new(|| {})), "third must be rejected");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let at = pool
        .last_drop_unix_secs()
        .expect("a rejection must record when it happened");
    assert!(
        now.saturating_sub(at) <= 60,
        "drop stamp {at} is not close to now ({now})"
    );

    let _ = release_tx.send(());
}

/// A truncation is counted and stamped, and does NOT move the drop counters.
///
/// Why: the two losses need different fixes — a full pool versus one batch
/// overrunning its budget — so an operator who sees them summed, or sees a
/// truncation surface as a "drop", is pointed at the wrong cause. This pins
/// that they stay separate numbers rather than one aggregate.
/// What: records a truncation on a fresh pool and asserts the truncation count
/// went to 1 with a stamp within a minute of now, while `rejected` and
/// `last_drop_unix_secs` stayed at their never-happened values.
/// Test: this test.
#[test]
fn a_truncation_is_counted_apart_from_a_rejection() {
    let pool = BoundedDispatcher::new(1, 1);
    assert_eq!(pool.truncated(), 0, "a fresh pool has truncated nothing");
    assert_eq!(pool.last_truncation_unix_secs(), None);

    pool.record_truncation();

    assert_eq!(pool.truncated(), 1, "the truncation must be counted");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let at = pool
        .last_truncation_unix_secs()
        .expect("a truncation must record when it happened");
    assert!(
        now.saturating_sub(at) <= 60,
        "truncation stamp {at} is not close to now ({now})"
    );

    assert_eq!(
        pool.rejected(),
        0,
        "a truncation must not be reported as a pool rejection"
    );
    assert_eq!(
        pool.last_drop_unix_secs(),
        None,
        "a truncation must not stamp the drop clock"
    );
}

/// A panicking job does not retire the worker that ran it.
///
/// Why: with a fixed worker count, a worker lost to a panic is never replaced —
/// lose all of them and every later submission is rejected forever, turning the
/// bound into a permanent outage that looks exactly like a wedged daemon.
/// What: submits a job that panics to a 1-worker pool, then a job that reports
/// it ran; the second must still run. The panic message this prints to stderr
/// during the run is expected.
/// Test: this test.
#[test]
fn a_panicking_job_does_not_kill_its_worker() {
    let pool = BoundedDispatcher::new(1, 4);

    assert!(pool.try_submit(Box::new(|| panic!("deliberate test panic"))));

    let (ran_tx, ran_rx) = channel();
    assert!(pool.try_submit(Box::new(move || {
        let _ = ran_tx.send(());
    })));

    ran_rx
        .recv_timeout(WAIT)
        .expect("the worker died with the panicking job instead of surviving it");
}
