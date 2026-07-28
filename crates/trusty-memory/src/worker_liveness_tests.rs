//! Unit tests for the [`super::WorkerLiveness`] occupancy gauge (issue #4001).
//!
//! Why: split out of `worker_liveness.rs` so the production module stays under
//! the 500-SLOC cap. Wired back in via `#[path] mod tests;`.
//! What: covers the idle case, age tracking, drop/panic release, overflow, and
//! the wedge verdict.
//! Test: this *is* the test file.

use super::*;

#[test]
fn idle_tracker_reports_no_work() {
    let t = WorkerLiveness::new();
    assert_eq!(t.in_flight(), 0);
    assert_eq!(t.oldest_age(), None);
    // An idle daemon is never wedged, no matter how small the threshold.
    assert!(!t.is_wedged(Duration::from_millis(0)));
}

#[test]
fn in_flight_counts_active_operations() {
    let t = WorkerLiveness::new();
    let a = t.track();
    assert_eq!(t.in_flight(), 1);
    let b = t.track();
    assert_eq!(t.in_flight(), 2);
    drop(a);
    assert_eq!(t.in_flight(), 1);
    drop(b);
    assert_eq!(t.in_flight(), 0);
}

#[test]
fn guard_releases_slot_on_drop() {
    let t = WorkerLiveness::new();
    {
        let _g = t.track();
        assert_eq!(t.in_flight(), 1);
    }
    assert_eq!(t.in_flight(), 0);
    assert_eq!(t.oldest_age(), None);
}

/// Why (issue #4001): the operations this gauge tracks are exactly the ones
/// that fail and unwind. If a slot leaked on the panic path, the gauge would
/// report a permanent phantom wedge — trading the original false positive for
/// a new one. Drop-driven release is what prevents that, and this test is the
/// proof.
/// What: panics inside a tracked scope and asserts the slot came back.
/// Test: itself.
#[test]
fn guard_releases_slot_on_panic() {
    let t = WorkerLiveness::new();
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _g = t.track();
        assert_eq!(t.in_flight(), 1);
        panic!("simulated failure inside a tracked operation");
    }));
    assert!(res.is_err(), "the panic must propagate");
    assert_eq!(
        t.in_flight(),
        0,
        "slot must be released when the operation unwinds"
    );
}

#[test]
fn oldest_age_tracks_the_earliest_operation() {
    let t = WorkerLiveness::new();
    let _old = t.track();
    std::thread::sleep(Duration::from_millis(30));
    let _new = t.track();
    let age = t.oldest_age().expect("two operations are in flight");
    assert!(
        age >= Duration::from_millis(30),
        "oldest_age must report the EARLIEST operation, got {age:?}"
    );
}

#[test]
fn oldest_age_recovers_when_the_slow_operation_finishes() {
    let t = WorkerLiveness::new();
    let old = t.track();
    std::thread::sleep(Duration::from_millis(30));
    drop(old);
    let _fresh = t.track();
    let age = t.oldest_age().expect("one operation is in flight");
    assert!(
        age < Duration::from_millis(30),
        "age must drop back once the slow operation completes, got {age:?}"
    );
}

/// Why (issue #4001): this is the verdict doctor reports. A pool with work
/// outstanding longer than the bound is wedged even though the process is
/// alive and its HTTP listener still answers.
/// What: asserts the threshold comparison in both directions.
/// Test: itself.
#[test]
fn wedged_when_oldest_exceeds_threshold() {
    let t = WorkerLiveness::new();
    let _g = t.track();
    std::thread::sleep(Duration::from_millis(30));
    assert!(
        t.is_wedged(Duration::from_millis(10)),
        "an operation older than the threshold must read as wedged"
    );
    assert!(
        !t.is_wedged(Duration::from_secs(60)),
        "the same operation must NOT read as wedged under a generous threshold"
    );
}

/// Why: the gauge must degrade gracefully rather than block or allocate when
/// concurrency exceeds the slot table. Overflow operations still count toward
/// `in_flight` so the daemon never under-reports its load.
/// What: fills every slot plus two extra and checks both the count and the
/// clean return to zero.
/// Test: itself.
/// Why: the wedge line must sit ABOVE every legitimately-bounded wait, or
/// healthy load reads as a wedge and operators learn to ignore the signal. The
/// longest bound on the palace open path is `open_queue_timeout()` (issue
/// #3992), after which `open_palace` gives up on its own.
/// What: asserts the default threshold strictly exceeds that bound. Skipped
/// when the env override is set, since it deliberately decouples the two.
/// Test: itself.
#[test]
fn wedge_threshold_exceeds_the_open_queue_bound() {
    if std::env::var("TRUSTY_WEDGE_THRESHOLD_SECS").is_ok() {
        return; // operator override in effect; the relationship is theirs to pick
    }
    let bound = trusty_common::memory_core::timeouts::open_queue_timeout();
    assert!(
        wedge_threshold() > bound,
        "wedge threshold {:?} must exceed the open-queue bound {bound:?}, or normal \
         contention would read as a wedge",
        wedge_threshold()
    );
}

#[test]
fn overflow_is_counted_when_slots_exhausted() {
    let t = WorkerLiveness::new();
    let guards: Vec<_> = (0..SLOTS + 2).map(|_| t.track()).collect();
    assert_eq!(t.in_flight(), SLOTS + 2);
    // An age sample still exists — the slot table is full of real timestamps.
    assert!(t.oldest_age().is_some());
    drop(guards);
    assert_eq!(t.in_flight(), 0, "overflow must unwind cleanly");
    assert_eq!(t.oldest_age(), None);
}
