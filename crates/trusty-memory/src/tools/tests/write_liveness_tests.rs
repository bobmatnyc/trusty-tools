//! Worker-liveness coverage for the budgeted write path (issue #4001).
//!
//! Why: on 2026-09-13 a `memory_remember` held the `trusty-tools` palace write
//! lock and never finished. Every later writer failed after 60 s with
//! "write-lock acquisition timed out", while `memory.health` reported `ok` with
//! 0 in flight, so both doctors read HEALTHY. The gauge covered only the palace
//! open, which a write leaves long before it finishes.
//! What: drives the real `memory_remember` handler against contended locks and
//! reads the verdict back through the real `memory.health` handler.
//! Test: this IS the test module.
//!
//! No assertion here depends on a sleep. Each test waits for a condition with a
//! bounded poll ([`wait_for`]), so a slow host lengthens the run but cannot
//! flip the verdict.

use super::*;
use crate::transport::methods::health::{health, HealthQuery};
use std::time::{Duration, Instant};

/// Upper bound on any single condition wait in this module.
const SETTLE: Duration = Duration::from_secs(10);

/// Build a Ready `AppState` with an injected write budget.
fn liveness_state(budget: Duration) -> (AppState, tempfile::TempDir) {
    skip_palace_enforcement();
    seed_embedder();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf()).with_write_op_budget(budget);
    state.set_ready();
    (state, tmp)
}

/// Poll `cond` until it holds, failing the test after [`SETTLE`].
async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let started = Instant::now();
    while !cond() {
        assert!(
            started.elapsed() < SETTLE,
            "timed out after {SETTLE:?} waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Read `memory.health` on the cheap path, as both doctors do.
async fn health_body(state: &AppState) -> serde_json::Value {
    health(state, HealthQuery::default())
        .await
        .expect("the cheap health path never fails")
}

/// Why (issue #4001): THE regression lock for the 2026-09-13 recurrence. A
/// writer took the palace write lock and then stalled, so every later writer
/// timed out at its bound. Waiters are bounded below the wedge threshold by
/// design, so tracking only the wait can never trip it; only the stalled
/// HOLDER's age grows without bound. Pre-fix nothing on the write path was
/// tracked once the palace open returned, and `/health` said `ok`, 0 in flight.
/// What: stalls writer #1 on the handle's own write mutex (the lock the dream
/// cycle also takes) while it holds the palace write lock. Writer #2 then
/// times out exactly as the logged incident did. Once #2 has given up, the
/// stalled holder alone must carry `/health` to `wedged`. Releasing the stall
/// must clear the verdict again.
/// Test: this test.
#[tokio::test]
async fn a_write_stalled_holding_the_palace_lock_reads_as_wedged() {
    // Waiters give up at 300 ms; the wedge line sits above that, mirroring the
    // production relationship (60 s waiter bound, 120 s threshold).
    let budget = Duration::from_millis(300);
    let threshold = Duration::from_millis(600);
    let (mut state, _tmp) = liveness_state(budget);
    state.wedge_threshold = threshold;
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "stuck"}))
        .await
        .expect("palace_create");

    let handle = open_palace_handle(&state, "stuck").expect("open palace");
    let inner = handle.write_mutex.clone();
    let stall = inner.lock().await;

    let holder_state = state.clone();
    let holder = tokio::spawn(async move {
        handle_memory_remember(
            &holder_state,
            json!({"palace": "stuck", "text": "the first writer records a sufficiently long fact about the palace write lock"}),
        )
        .await
    });
    let write_lock = state.palace_write_lock("stuck");
    wait_for("writer #1 to take the palace write lock", || {
        write_lock.try_lock().is_err()
    })
    .await;

    let err = handle_memory_remember(
        &state,
        json!({"palace": "stuck", "text": "the second writer records a sufficiently long fact about the palace write lock"}),
    )
    .await
    .expect_err("writer #2 must time out behind the stalled holder");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("memory_remember") && msg.contains("write-lock acquisition timed out"),
        "writer #2 must fail the way the 2026-09-13 incident logged; got: {msg}"
    );

    wait_for("the stalled holder to outlive the wedge threshold", || {
        state.worker_liveness.is_wedged(threshold)
    })
    .await;
    let v = health_body(&state).await;
    assert_eq!(
        v["status"], "wedged",
        "a writer stalled on the palace lock past the threshold is the #4001 wedge; got {v}"
    );
    assert_eq!(
        v["worker"]["in_flight"], 1,
        "the timed-out waiter must have released its slot, leaving only the holder; got {v}"
    );

    drop(stall);
    tokio::time::timeout(SETTLE, holder)
        .await
        .expect("the holder finishes once the stall clears")
        .expect("holder task joins")
        .expect("the stalled write lands once the lock frees");
    let v = health_body(&state).await;
    assert_eq!(v["status"], "ok", "the verdict must clear; got {v}");
    assert_eq!(v["worker"]["in_flight"], 0, "got {v}");
}

/// Poll `memory.health` until `pred` holds, failing after [`SETTLE`].
async fn wait_for_health(
    state: &AppState,
    what: &str,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let started = Instant::now();
    loop {
        let v = health_body(state).await;
        if pred(&v) {
            return v;
        }
        assert!(
            started.elapsed() < SETTLE,
            "timed out after {SETTLE:?} waiting for {what}; last health: {v}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Spawn `n` `memory_remember` writers on `palace`, each abandoned by its
/// client after `bound`, and assert none of them landed.
///
/// Why: dropping the future at the bound is what a writer's own timeout does;
/// it releases every lock and liveness slot the writer held.
async fn writers_give_up(state: &AppState, palace: &str, n: usize, bound: Duration) {
    let mut writers = Vec::new();
    for i in 0..n {
        let s = state.clone();
        let args = json!({"palace": palace, "text": format!("queued writer {i} records a sufficiently long fact about the handle write mutex")});
        writers.push(tokio::spawn(async move {
            tokio::time::timeout(bound, handle_memory_remember(&s, args)).await
        }));
    }
    for w in writers {
        let outcome = w.await.expect("writer task joins");
        assert!(
            !matches!(outcome, Ok(Ok(_))),
            "no writer may land while the handle write mutex is held"
        );
    }
}

/// Why (issue #4001, 2026-09-13 dream-cycle shape): a dream cycle held the
/// palace's handle write mutex, writers queued and timed out, and health said
/// `ok`. The dream never registers with the gauge, and the writers leave it at
/// their bound, so nothing tracked ever aged past the threshold.
/// What: holds `handle.write_mutex` as the dream does, lets three writers queue
/// and give up at 300 ms, asserts the gauge is empty, then polls health until
/// it reports `wedged` naming the palace's write lock. Releasing the lock must
/// clear the verdict.
/// Test: this test.
#[tokio::test]
async fn a_dream_cycle_holding_the_handle_write_mutex_reads_as_wedged() {
    let bound = Duration::from_millis(300);
    let threshold = Duration::from_millis(600);
    let (mut state, _tmp) = liveness_state(bound);
    state.wedge_threshold = threshold;
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "dreaming"}))
        .await
        .expect("palace_create");
    let handle = open_palace_handle(&state, "dreaming").expect("open palace");
    let dream = handle.write_mutex.clone().lock_owned().await;

    writers_give_up(&state, "dreaming", 3, bound).await;
    assert_eq!(
        state.worker_liveness.in_flight(),
        0,
        "every writer left at its bound; only the untracked dream holds a lock"
    );

    let v = wait_for_health(
        &state,
        "health to report the held handle lock as wedged",
        |v| v["status"] == "wedged",
    )
    .await;
    assert_eq!(v["worker"]["wedged"], true, "got {v}");
    assert_eq!(v["worker"]["stalled_lock"]["palace"], "dreaming", "got {v}");
    assert_eq!(v["worker"]["stalled_lock"]["lock"], "write", "got {v}");

    drop(dream);
    let v = wait_for_health(&state, "the verdict to clear after release", |v| {
        v["status"] == "ok" && v["worker"].get("stalled_lock").is_none()
    })
    .await;
    assert_eq!(v["worker"]["wedged"], false, "got {v}");
}

/// Why (issue #4001, no false alarm): a dream cycle legitimately runs 38-53 s
/// under the default 120 s threshold. Holding the handle lock inside the
/// threshold must be visible but not wedged.
/// What: holds `handle.write_mutex` under the production-derived threshold
/// with a writer queued, polls until health reports the held lock, and asserts
/// `status: "ok"` and `wedged: false`.
/// Test: this test.
#[tokio::test]
async fn a_handle_lock_held_inside_the_threshold_is_visible_but_not_wedged() {
    let (state, _tmp) = liveness_state(Duration::from_millis(300));
    assert!(state.wedge_threshold >= Duration::from_secs(120));
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "napping"}))
        .await
        .expect("palace_create");
    let handle = open_palace_handle(&state, "napping").expect("open palace");
    let dream = handle.write_mutex.clone().lock_owned().await;
    writers_give_up(&state, "napping", 1, Duration::from_millis(300)).await;

    let v = wait_for_health(&state, "health to report the held handle lock", |v| {
        v["worker"]["stalled_lock"]["palace"] == "napping"
    })
    .await;
    assert_eq!(
        v["status"], "ok",
        "held inside the threshold is busy, not wedged; got {v}"
    );
    assert_eq!(v["worker"]["wedged"], false, "got {v}");
    drop(dream);
}

/// Why (issue #4001, no false alarm): tracking writes must not turn ordinary
/// contention into a wedge. A writer queued behind a live holder, well inside
/// its bound, is busy rather than stuck.
/// What: holds the palace write lock from the test, lets one real
/// `memory_remember` queue behind it, and asserts `/health` shows the waiter in
/// flight with `status: "ok"` under the production threshold. Releasing the
/// lock must let the write land and the slot clear.
/// Test: this test.
#[tokio::test]
async fn a_write_queued_inside_its_bound_is_in_flight_but_not_wedged() {
    let (state, _tmp) = liveness_state(Duration::from_secs(60));
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "queued"}))
        .await
        .expect("palace_create");

    let write_lock = state.palace_write_lock("queued");
    let held = write_lock.lock().await;
    let waiter_state = state.clone();
    let waiter = tokio::spawn(async move {
        handle_memory_remember(
            &waiter_state,
            json!({"palace": "queued", "text": "a queued writer records a sufficiently long fact about the palace write lock"}),
        )
        .await
    });
    wait_for("the queued writer to register in the gauge", || {
        state.worker_liveness.in_flight() == 1
    })
    .await;

    let v = health_body(&state).await;
    assert_eq!(v["status"], "ok", "busy is not wedged; got {v}");
    assert_eq!(v["worker"]["wedged"], false, "got {v}");
    assert_eq!(
        v["worker"]["in_flight"], 1,
        "the waiter must be visible; got {v}"
    );

    drop(held);
    tokio::time::timeout(SETTLE, waiter)
        .await
        .expect("the queued write finishes once the lock frees")
        .expect("waiter task joins")
        .expect("the queued write lands");
    let v = health_body(&state).await;
    assert_eq!(v["status"], "ok", "got {v}");
    assert_eq!(v["worker"]["in_flight"], 0, "the slot must clear; got {v}");
}
