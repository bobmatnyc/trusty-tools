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
    let bound = crate::engine::timeouts::open_queue_timeout();
    assert!(
        wedge_threshold() > bound,
        "wedge threshold {:?} must exceed the open-queue bound {bound:?}, or normal \
         contention would read as a wedge",
        wedge_threshold()
    );
}

/// Bounds with every input distinct, so a test can tell which one moved.
fn bounds(open_queue: u64, write_lock: u64, budget: u64, pipeline: u64) -> WedgeBounds {
    WedgeBounds {
        open_queue: Duration::from_secs(open_queue),
        write_lock: Duration::from_secs(write_lock),
        write_op_budget: Duration::from_secs(budget),
        write_pipeline: Duration::from_secs(pipeline),
    }
}

/// Why (#4001, #9783): the line must clear a write's whole legal span (its
/// acquisition budget plus its pipeline ceiling) and still sit past the old
/// wait line, so raising any one bound must move it. A raised
/// `TRUSTY_WRITE_LOCK_TIMEOUT_SECS` that did not move it would make a writer
/// queued inside its bound read as wedged (#4001).
/// What: checks the exact derivation at the defaults, then raises each bound in
/// turn and checks the line rises by the matching amount, and that the operator
/// override still wins outright.
/// Test: itself.
#[test]
fn wedge_line_is_the_write_span_plus_the_wait_line() {
    let defaults = bounds(60, 60, 60, 240);
    assert_eq!(
        derive_wedge_threshold(None, &defaults),
        Duration::from_secs(60 + 240 + 2 * 60),
        "budget + pipeline + 2 x max(open_queue, write_lock)"
    );
    let base = derive_wedge_threshold(None, &defaults);
    let raised = [
        (bounds(300, 60, 60, 240), 2 * 240, "open queue"),
        (bounds(60, 300, 60, 240), 2 * 240, "write lock"),
        (bounds(60, 60, 360, 240), 300, "write op budget"),
        (bounds(60, 60, 60, 900), 660, "write pipeline"),
    ];
    for (b, rise, what) in raised {
        assert_eq!(
            derive_wedge_threshold(None, &b),
            base + Duration::from_secs(rise),
            "a raised {what} bound must move the wedge line"
        );
    }
    assert_eq!(
        derive_wedge_threshold(Some(7), &defaults),
        Duration::from_secs(7),
        "the operator override is taken verbatim"
    );
}

/// Why (#9783, fail-open check): a zero, missing or absurd bound must not leave
/// a line that no stall can cross, and must not panic the daemon at
/// construction (the old `* 2` overflowed on a huge open-queue bound).
/// What: a zero acquisition budget still leaves a finite line past the
/// pipeline ceiling; all-zero bounds give a zero line, under which a 1 ms hold
/// already reads wedged (fails closed); `u64::MAX`-second bounds saturate
/// instead of panicking. The env path never reaches the all-zero case, because
/// a zero `TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS` is floored to the 240 s default.
/// Test: itself.
#[test]
fn a_zero_or_absurd_bound_never_disables_wedge_detection() {
    let no_budget = derive_wedge_threshold(None, &bounds(60, 60, 0, 240));
    assert_eq!(no_budget, Duration::from_secs(240 + 120));
    assert!(
        Duration::from_secs(1800) > no_budget,
        "a #3992-sized stall (1800 s) must still cross the line"
    );

    let zero = derive_wedge_threshold(None, &bounds(0, 0, 0, 0));
    assert!(
        Duration::from_millis(1) > zero,
        "an all-zero line must over-report, never disable detection"
    );
    let floored = crate::engine::timeouts::floor_write_pipeline(Duration::ZERO);
    assert!(
        floored > Duration::ZERO,
        "the env path floors a zero pipeline ceiling, so the line stays finite and non-zero"
    );

    let huge = derive_wedge_threshold(None, &bounds(u64::MAX, u64::MAX, u64::MAX, u64::MAX));
    assert_eq!(
        huge,
        Duration::MAX,
        "absurd bounds saturate rather than panic"
    );
}

/// Why (#9783): a `memory_remember` waits up to `write_op_budget()` (60 s) for
/// the palace write lock, then holds it across embed, upsert and persist for up
/// to `write_pipeline_timeout()` (240 s). The pre-#9783 line,
/// `2 x max(open_queue, write_lock)` = 120 s, called that write wedged while it
/// was still inside both bounds.
/// What: under the default env, a hold one second short of the pipeline
/// ceiling, and the whole legal write span, must both sit inside
/// `wedge_threshold()` — the `age > threshold` comparison `/health` makes for
/// the lock arm and the pool arm.
/// Test: itself.
#[test]
fn a_legal_write_span_does_not_cross_the_default_wedge_line() {
    let _env = crate::env_test_lock::env_test_lock().blocking_lock();
    if std::env::var("TRUSTY_WEDGE_THRESHOLD_SECS").is_ok() {
        return; // operator override in effect; the relationship is theirs to pick
    }
    let line = wedge_threshold();
    let pipeline = crate::engine::timeouts::write_pipeline_timeout();
    let span = crate::engine::timeouts::write_op_budget() + pipeline;
    let progressing = pipeline - Duration::from_secs(1);
    assert!(
        progressing > Duration::from_secs(120),
        "precondition: the hold must sit past the pre-#9783 120 s line"
    );
    assert!(
        line > progressing,
        "a write {progressing:?} into its {pipeline:?} pipeline ceiling is progressing, \
         yet the wedge line is {line:?}"
    );
    assert!(
        line > span,
        "the wedge line {line:?} must clear the full legal write span {span:?} \
         (acquisition budget + pipeline ceiling)"
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
