//! Joint-budget tests for the MCP write path (issue #4002).
//!
//! Why: `memory_remember` waited up to `write_lock_timeout` for the per-palace
//! write mutex and THEN up to `open_queue_timeout` to enter the per-palace open
//! queue. Each leg was bounded, their sum was not, so a caller that exhausted
//! both waited ~123 s before any error surfaced. These tests pin the replacement
//! contract: one budget, stamped once, spent across both legs.
//! What: drives the real handlers against an `AppState` whose
//! `with_write_op_budget` injects a short deadline — never
//! `TRUSTY_WRITE_OP_BUDGET_SECS`, which is process-wide and would race the
//! parallel test harness.
//! Test: this IS the test module.
//!
//! No test here sleeps to make an assertion true. The contended cases hold a
//! lock for the whole test and assert an UPPER bound that sits ~30x below the
//! pre-fix value, so a slow host cannot flip the verdict (#5943).

use super::*;
use std::time::{Duration, Instant};
use trusty_common::memory_core::timeouts;

/// Budget short enough that an exhausted-budget error lands promptly, and far
/// enough below `write_lock_timeout()` (60 s) that observing it at all proves
/// the budget — not the per-leg timeout — decided the wait.
const TEST_BUDGET: Duration = Duration::from_millis(300);

/// Ceiling the contended handlers must finish under. Pre-fix the same setup
/// waited `write_lock_timeout()` = 60 s, so this is a ~30x margin over the
/// budget and a ~30x margin under the pre-fix value.
const CEILING: Duration = Duration::from_secs(2);

/// Build a Ready `AppState` with an injected short write budget and one palace.
fn budgeted_state() -> (AppState, tempfile::TempDir) {
    skip_palace_enforcement();
    seed_embedder();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf()).with_write_op_budget(TEST_BUDGET);
    state.set_ready();
    (state, tmp)
}

/// Why (issue #4002): this is the regression proof. The defect was that the
/// write-lock wait and the open-queue wait each opened their own full window,
/// so the operation's real ceiling was their SUM. With one joint budget the
/// first leg alone cannot outlast the budget, so a permanently-held write mutex
/// surfaces an error in ~`TEST_BUDGET` instead of `write_lock_timeout()`.
///
/// Pre-fix this test fails by TIMING OUT the assertion below: the handler hands
/// `write_lock_timeout()` (60 s) straight to the mutex, so `elapsed` is ~60 s
/// against a 2 s ceiling.
///
/// What: holds the palace's write mutex for the whole test, calls the real
/// `memory_remember` handler, and asserts it errors inside `CEILING`. Also
/// asserts the pre-fix additive worst case was genuinely larger than the
/// budget, so the test cannot pass vacuously if someone raises the budget past
/// the sum it replaced.
/// Test: this test.
#[tokio::test]
async fn memory_remember_gives_up_within_one_budget_not_the_leg_sum() {
    let (state, _tmp) = budgeted_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "budget"}))
        .await
        .expect("palace_create");

    // The additive worst case this budget replaces. If this ever stops holding,
    // the assertion below proves nothing.
    let pre_fix_worst_case = timeouts::write_lock_timeout() + timeouts::open_queue_timeout();
    assert!(
        pre_fix_worst_case > TEST_BUDGET,
        "the composed pre-fix wait ({pre_fix_worst_case:?}) must exceed the injected budget \
         ({TEST_BUDGET:?}), or this test proves nothing (issue #4002)"
    );

    // Hold the per-palace write mutex for the rest of the test, so leg 1 can
    // only ever end by expiring.
    let write_lock = state.palace_write_lock("budget");
    let _held = write_lock.lock().await;

    let started = Instant::now();
    let err = handle_memory_remember(
        &state,
        json!({"palace": "budget", "text": "a sufficiently long fact to clear the content gate"}),
    )
    .await
    .expect_err("a permanently held write mutex must surface an error, never block forever");
    let elapsed = started.elapsed();

    assert!(
        elapsed < CEILING,
        "memory_remember must give up inside its {TEST_BUDGET:?} budget; got {elapsed:?} \
         (pre-fix this leg alone waited {:?} — issue #4002)",
        timeouts::write_lock_timeout()
    );
    let msg = format!("{err:#}");
    assert!(
        msg.contains("memory_remember") && msg.contains("write-lock acquisition timed out"),
        "the error must name the tool and the leg that expired; got: {msg}"
    );
}

/// Why (issue #4002): `memory_note` runs the identical two-leg sequence, and a
/// fix applied to only one handler would leave the other additive.
/// What: same held-mutex setup, driven through `memory_note`.
/// Test: this test.
#[tokio::test]
async fn memory_note_gives_up_within_one_budget_not_the_leg_sum() {
    let (state, _tmp) = budgeted_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "budget"}))
        .await
        .expect("palace_create");

    let write_lock = state.palace_write_lock("budget");
    let _held = write_lock.lock().await;

    let started = Instant::now();
    let err = handle_memory_note(
        &state,
        json!({
            "palace": "budget",
            // Long enough to clear the content gate, which runs BEFORE the
            // write-lock leg this test is measuring.
            "content": "Masa prefers snake_case for every identifier in this workspace",
        }),
    )
    .await
    .expect_err("a permanently held write mutex must surface an error");
    let elapsed = started.elapsed();

    assert!(
        elapsed < CEILING,
        "memory_note must give up inside its {TEST_BUDGET:?} budget; got {elapsed:?} (issue #4002)"
    );
    assert!(
        format!("{err:#}").contains("memory_note"),
        "the error must name the tool that gave up"
    );
}

/// Why (issue #4002): a budget that shortens the UNCONTENDED path would be a
/// regression far worse than the wait it replaced — every ordinary write would
/// start failing once the budget was small. The clamp is a ceiling on waiting,
/// and an uncontended acquisition takes no measurable time, so a 300 ms budget
/// must not disturb a normal write.
/// What: writes and reads back through the real handlers with the same short
/// budget injected, asserting the drawer lands.
/// Test: this test.
#[tokio::test]
async fn a_short_budget_does_not_disturb_an_uncontended_write() {
    let (state, _tmp) = budgeted_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "budget"}))
        .await
        .expect("palace_create");

    let stored = handle_memory_remember(
        &state,
        json!({"palace": "budget", "text": "an uncontended write must still land on disk"}),
    )
    .await
    .expect("an uncontended write must succeed under a short budget");
    assert_eq!(stored["status"], "stored");

    let listed = dispatch_tool(&state, "memory_list", json!({"palace": "budget"}))
        .await
        .expect("memory_list");
    assert_eq!(
        listed["drawers"].as_array().map(Vec::len),
        Some(1),
        "the budgeted write must be readable afterwards"
    );
}

/// Why (issue #4002): `task_add` shares the same `write_lock` → `write_drawer`
/// → `open_palace_handle` sequence, so it carried the same additive ceiling.
/// What: held-mutex setup driven through `task_add`.
/// Test: this test.
#[tokio::test]
async fn task_add_gives_up_within_one_budget_not_the_leg_sum() {
    let (state, _tmp) = budgeted_state();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "budget"}))
        .await
        .expect("palace_create");

    let write_lock = state.palace_write_lock("budget");
    let _held = write_lock.lock().await;

    let started = Instant::now();
    let err = dispatch_tool(
        &state,
        "task_add",
        json!({"palace": "budget", "content": "ship the joint budget"}),
    )
    .await
    .expect_err("a permanently held write mutex must surface an error");
    let elapsed = started.elapsed();

    assert!(
        elapsed < CEILING,
        "task_add must give up inside its {TEST_BUDGET:?} budget; got {elapsed:?} (issue #4002)"
    );
    assert!(
        format!("{err:#}").contains("task_add"),
        "the error must name the tool that gave up"
    );
}

/// Why (issue #4002): the budget's whole point is that the SECOND leg spends
/// the remainder. Once the first leg has consumed the budget the open-queue
/// wait must be zero, not a fresh 60 s window.
/// What: stamps a budget, exhausts it, and asserts the clamp the write path
/// applies to the open-queue leg is `Duration::ZERO`. Deterministic — a zero
/// total makes `remaining()` exactly zero on any host.
/// Test: this test.
#[test]
fn an_exhausted_write_budget_gives_the_open_queue_leg_nothing() {
    let spent = timeouts::OpBudget::start(Duration::ZERO);
    assert_eq!(
        spent.leg(timeouts::open_queue_timeout()),
        Duration::ZERO,
        "once the write-lock leg has spent the budget, the open-queue leg must not open \
         a fresh {:?} window (issue #4002)",
        timeouts::open_queue_timeout()
    );
}

// ---------------------------------------------------------------------------
// Pipeline ceiling (issue #6366). The budget above bounds the two waits BEFORE
// the palace write mutex is held; these bound the pipeline that runs once it IS
// held — the leg that produced the reported 1800 s client-side abort.
// ---------------------------------------------------------------------------

/// Ceiling on one write's critical section, short enough that no real pipeline
/// fits inside it. Injected per-instance, never via
/// `TRUSTY_WRITE_PIPELINE_TIMEOUT_SECS`, which is process-wide.
const TEST_PIPELINE_BUDGET: Duration = Duration::ZERO;

/// Build a Ready `AppState` with an injected pipeline ceiling and one palace.
async fn pipeline_capped_state() -> (AppState, tempfile::TempDir) {
    skip_palace_enforcement();
    seed_embedder();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state =
        AppState::new(tmp.path().to_path_buf()).with_write_pipeline_budget(TEST_PIPELINE_BUDGET);
    state.set_ready();
    let _ = dispatch_tool(&state, "palace_create", json!({"name": "pipeline"}))
        .await
        .expect("palace_create");
    (state, tmp)
}

/// Why (issue #6366): the daemon's write handlers are the surface the reported
/// stall came through, so the ceiling has to be reachable from there. A bound
/// that existed only on `PalaceHandle` and was never threaded through
/// `write_drawer` would leave `memory_note` exactly as unbounded as before.
/// What: drives the real `memory_note` handler against a state whose pipeline
/// ceiling cannot be met, and asserts the error names the ceiling instead of
/// reporting a stored drawer.
/// Test: this test.
#[tokio::test]
async fn memory_note_surfaces_the_pipeline_ceiling() {
    let (state, _tmp) = pipeline_capped_state().await;

    let started = Instant::now();
    let err = handle_memory_note(
        &state,
        json!({"palace": "pipeline", "content": "a curated fact that clears the content gate"}),
    )
    .await
    .expect_err("a write that cannot fit its pipeline ceiling must error, not stall");
    let elapsed = started.elapsed();

    assert!(
        elapsed < CEILING,
        "the ceiling must be enforced promptly; got {elapsed:?}"
    );
    let msg = format!("{err:#}");
    assert!(
        msg.contains("#6366") && msg.contains("write pipeline exceeded"),
        "the error must name the pipeline ceiling and its issue; got: {msg}"
    );
}

/// Why (issue #6366): the reported symptom is that ONE slow write stalls every
/// other writer on that palace. A ceiling that fired but left the mutex held
/// would move the stall rather than remove it.
/// What: runs a write that trips the ceiling, then asserts the palace's write
/// mutex is immediately available to the next caller.
/// Test: this test.
#[tokio::test]
async fn a_refused_write_leaves_the_palace_write_mutex_free() {
    let (state, _tmp) = pipeline_capped_state().await;

    let _ = handle_memory_note(
        &state,
        json!({"palace": "pipeline", "content": "a curated fact that clears the content gate"}),
    )
    .await;

    let write_lock = state.palace_write_lock("pipeline");
    assert!(
        write_lock.try_lock().is_ok(),
        "#6366: a refused write must not leave the palace write mutex held — \
         that would stall every writer behind it, which is the reported defect"
    );
}
