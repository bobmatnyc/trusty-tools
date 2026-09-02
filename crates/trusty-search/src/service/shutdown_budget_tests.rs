//! Regression tests for the #4393 shutdown-window fix.
//!
//! Why: the defect was arithmetic — a 30 s-floored, 20 min-ceilinged per-index
//! flush budget inside a 3–5 s termination window — so the tests are arithmetic
//! too. Each one fails if the clamp in
//! [`super::ShutdownBudget::flush_deadline_for`] is removed and the size-scaled
//! deadline is handed straight through, which is exactly the pre-fix code.
//!
//! Test: this IS the test module.

use super::*;
use crate::service::shutdown_flush::MIN_FLUSH_TIMEOUT_SECS;
// `CLEANUP_RESERVE` arrives through `use super::*` above — the module
// re-exports it (#6601 review), so this file exercises the alias rather than
// reaching past it to `trusty_common`.
use serial_test::serial;

/// A snapshot path that does not exist, so `shutdown_flush_deadline_for` returns
/// exactly its floor (`MIN_FLUSH_TIMEOUT_SECS`) with no size scaling.
fn missing_snapshot() -> &'static Path {
    Path::new("/nonexistent/hnsw.usearch")
}

/// Why (#4393 — the core defect): a per-index deadline larger than the process's
/// remaining life is a promise the process cannot keep. Before the clamp, a
/// missing snapshot alone bought 30 s inside a window that measured 3–5 s, and a
/// 200 MB one bought 40 s; the flush was SIGKILLed partway through index #1 and
/// indexes #2..N were never attempted. Deleting the `.min(remaining)` in
/// `flush_deadline_for` makes this assertion fail.
/// What: opens a 10 s window (5 s after the cleanup reserve) and asks for a
/// deadline against a missing snapshot, whose unclamped value is the 30 s floor.
/// Asserts the grant is bounded by the window, not by the floor.
/// Test: this IS the test.
#[test]
#[serial]
fn flush_deadline_is_clamped_to_the_remaining_window() {
    // SAFETY: `#[serial]` — no other thread reads the environment during this body.
    unsafe { std::env::remove_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS") };

    let budget = ShutdownBudget::from_window(Duration::from_secs(10));
    let granted = budget
        .flush_deadline_for(missing_snapshot())
        .expect("a 10 s window is not exhausted")
        .as_duration();

    assert!(
        granted <= Duration::from_secs(5),
        "grant must fit the 10 s window minus the 5 s cleanup reserve; got {granted:?}"
    );
    assert!(
        granted < Duration::from_secs(MIN_FLUSH_TIMEOUT_SECS),
        "grant must be smaller than the {MIN_FLUSH_TIMEOUT_SECS}s size-scaled floor — \
         handing the floor through unclamped is the #4393 defect; got {granted:?}"
    );
}

/// Why (#4393): the clamp must not become a cap. When the process genuinely has
/// the time, a large index must still get its size-scaled budget — otherwise the
/// fix for one data-loss mode introduces another (a 500 MB snapshot cut off at an
/// arbitrary small deadline).
/// What: opens a window far larger than the floor and asserts the grant equals
/// the size-scaled value `shutdown_flush_deadline_for` computed.
/// Test: this IS the test.
#[test]
#[serial]
fn flush_deadline_keeps_the_size_scaled_value_when_the_window_is_ample() {
    // SAFETY: `#[serial]`.
    unsafe { std::env::remove_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS") };

    let budget = ShutdownBudget::from_window(Duration::from_secs(3600));
    let granted = budget
        .flush_deadline_for(missing_snapshot())
        .expect("an hour-long window is not exhausted")
        .as_duration();

    assert_eq!(
        granted,
        Duration::from_secs(MIN_FLUSH_TIMEOUT_SECS),
        "with time to spare the size-scaled deadline must survive intact"
    );
}

/// Why (#4393): once the window is spent the sweep must stop at an index
/// boundary rather than start a flush it cannot finish. `None` is what makes
/// that structural — there is no deadline to hand out, so `flush_index_bounded`
/// is never called for that index and its last checkpoint stands.
/// What: builds a budget whose deadline is already in the past and asserts no
/// deadline can be minted from it.
/// Test: this IS the test.
#[test]
#[serial]
fn exhausted_budget_mints_no_deadline() {
    // SAFETY: `#[serial]`.
    unsafe { std::env::remove_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS") };

    let long_ago = Instant::now() - Duration::from_secs(600);
    let budget = ShutdownBudget::from_window_at(long_ago, Duration::from_secs(60));

    assert!(budget.is_exhausted(), "a window opened 10 min ago is spent");
    assert!(
        budget.flush_deadline_for(missing_snapshot()).is_none(),
        "an exhausted budget must refuse to grant a deadline"
    );
}

/// Why (#4393): the flush is not the last thing the process does — the port
/// file, `http_addr`, the discovery registry, and the lockfile all still have to
/// be cleared. Spending the whole window flushing hands SIGKILL the cleanup
/// instead.
/// What: asserts the usable budget is the window minus [`CLEANUP_RESERVE`].
/// Test: this IS the test.
#[test]
fn budget_reserves_time_for_post_flush_cleanup() {
    let now = Instant::now();
    let budget = ShutdownBudget::from_window_at(now, Duration::from_secs(60));
    let remaining = budget.remaining();
    assert!(
        remaining <= Duration::from_secs(55),
        "budget must hold back the cleanup reserve; got {remaining:?}"
    );
    assert!(
        remaining > Duration::from_secs(50),
        "budget must not hold back more than the reserve; got {remaining:?}"
    );
}

/// Why (#4393): a declared window smaller than the cleanup reserve must
/// saturate to zero, not wrap. `Instant + Duration` has no wrapping hazard, but
/// `Duration - Duration` does, and an underflow here would panic or (worse, with
/// a different formulation) produce an enormous budget inside a tiny window —
/// the exact bug, restored by accident.
/// What: declares a 1 s window and asserts the budget is immediately exhausted.
/// Test: this IS the test.
#[test]
fn budget_shorter_than_the_cleanup_reserve_is_exhausted() {
    let budget = ShutdownBudget::from_window(Duration::from_secs(1));
    assert!(
        budget.is_exhausted(),
        "a window under the cleanup reserve leaves nothing for flushing"
    );
}

/// Why (#4393 — the mismatch this issue reports, asserted directly): the per-
/// index floor is 30 s and the shipped termination window must cover it, or the
/// very first index of every shutdown is planned to outlive the process. This is
/// the arithmetic that failed on `main`: floor 30 s, window 3–5 s.
/// What: asserts the configured termination grace covers the floor plus the
/// cleanup reserve.
/// Test: this IS the test.
#[test]
#[serial]
fn flush_floor_fits_the_termination_window() {
    // SAFETY: `#[serial]`; clear any operator override so the shipped default
    // is what gets asserted.
    unsafe { std::env::remove_var(trusty_common::shutdown::TERMINATION_GRACE_ENV) };

    let window = trusty_common::shutdown::termination_grace();
    let floor = Duration::from_secs(MIN_FLUSH_TIMEOUT_SECS);
    assert!(
        window >= floor + CLEANUP_RESERVE,
        "the termination window ({window:?}) must cover one index's flush floor \
         ({floor:?}) plus the cleanup reserve ({CLEANUP_RESERVE:?}) — this is the \
         #4393 mismatch, which measured 5 s against 30 s on main"
    );
}
