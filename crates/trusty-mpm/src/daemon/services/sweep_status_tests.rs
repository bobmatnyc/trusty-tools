//! Tests for the maintenance lane and the sweep status the doctor row reads
//! (#7965).
//!
//! The statics are process-global, which is deliberate — there is one daemon per
//! process and one lane per daemon. The tests below therefore assert on a LOCAL
//! `SweepStatus` where they can, and only `the_lane_serialises_two_sweeps`
//! touches the shared lane.

use std::time::Duration;

use super::{HYGIENE, RECLAIM, RECLAIM_NAME, SweepStatus, lane, rows};
use crate::core::doctor::CheckStatus;

/// Clears both sweep kill switches for one test; `Drop` restores them.
///
/// Why: the row tests assert "ON", and an operator shell that exports
/// `TRUSTY_MPM_WORKTREE_RECLAIM=off` / `TRUSTY_MPM_INPROJECT_HYGIENE=0` turned
/// them red. Every user is `#[serial]`, which is what makes the mutation safe.
struct SweepSwitchesOn(Vec<(&'static str, Option<String>)>);

impl SweepSwitchesOn {
    fn new() -> Self {
        let keys = [
            super::super::merged_pr_reclaim::ENV_ENABLED,
            crate::daemon::managed_routes::inproject_hygiene_sweep::ENV_ENABLED,
        ];
        let saved = keys
            .into_iter()
            .map(|key| {
                let prev = std::env::var(key).ok();
                // SAFETY: callers are `#[serial]`; restored in `Drop`.
                unsafe { std::env::remove_var(key) };
                (key, prev)
            })
            .collect();
        Self(saved)
    }
}

impl Drop for SweepSwitchesOn {
    fn drop(&mut self) {
        for (key, prev) in self.0.drain(..) {
            // SAFETY: see `new`.
            unsafe {
                match prev {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

/// A completed pass records its duration and clears the running flag.
#[test]
fn sweep_status_records_the_last_pass_duration() {
    static S: SweepStatus = SweepStatus::new("local-test-sweep");
    assert_eq!(S.last_pass(), None, "no pass has run");
    assert!(!S.is_running());
    {
        let _guard = S.begin(None);
        assert!(S.is_running(), "a pass in flight must be visible");
    }
    assert!(!S.is_running(), "the guard must clear the flag");
    assert!(S.last_pass().is_some(), "the guard must record a duration");
}

/// 🔴 #7965 REGRESSION: a pass that PANICS still clears `running`.
///
/// Why: the doctor row's whole job is telling an operator whether a sweep is in
/// flight. A flag left set by a panicked pass would report a phantom sweep for
/// the life of the daemon, which is worse than no row at all.
#[test]
fn sweep_status_guard_records_even_on_panic() {
    static S: SweepStatus = SweepStatus::new("panicking-test-sweep");
    let caught = std::panic::catch_unwind(|| {
        let _guard = S.begin(None);
        panic!("a pass that blew up");
    });
    assert!(caught.is_err(), "the panic must have happened");
    assert!(!S.is_running(), "a panicked pass must not look in-flight");
    assert!(S.last_pass().is_some());
}

/// 🔴 #7965 REGRESSION: the two sweeps cannot hold the lane at the same time.
///
/// Why this is the assertion: the reported failure was hygiene and the worktree
/// reclaim both running unbounded subprocess loops, concurrently, against one
/// machine. One permit is the mechanism that makes that impossible; a lane with
/// two permits would let the incident recur with every other fix still in place.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_lane_serialises_two_sweeps() {
    let first = lane().acquire().await.expect("lane is open");
    let blocked = tokio::time::timeout(Duration::from_millis(200), lane().acquire()).await;
    assert!(
        blocked.is_err(),
        "a second sweep must WAIT while the first holds the lane"
    );
    drop(first);
    let second = tokio::time::timeout(Duration::from_millis(500), lane().acquire()).await;
    assert!(
        second.is_ok(),
        "the lane must be released when the first pass ends"
    );
}

/// Both sweeps appear in the doctor row, each naming its own kill switch.
#[test]
fn sweep_rows_report_each_sweeps_switch_and_last_pass() {
    let root = tempfile::TempDir::new().expect("tempdir");
    let rows = rows(root.path());
    assert_eq!(rows.len(), 2, "one row per sweep");
    let names: Vec<&str> = rows.iter().map(|r| r.name).collect();
    assert!(names.contains(&"worktree-reclaim"), "{names:?}");
    assert!(names.contains(&"inproject-hygiene"), "{names:?}");
    for r in &rows {
        assert!(
            r.env.starts_with("TRUSTY_MPM_"),
            "every sweep must name the variable that switches it off: {}",
            r.env
        );
    }
}

/// An in-flight pass WARNS, because that is the state that costs request latency.
///
/// Why `#[serial]`: the two statics and the env gates the row reads are
/// process-global, so a sibling holding a `PassGuard` — or flipping a kill switch
/// — would change this row under the assertion.
#[test]
#[serial_test::serial]
fn background_sweeps_row_warns_on_a_slow_pass() {
    let _on = SweepSwitchesOn::new();
    let root = tempfile::TempDir::new().expect("tempdir");
    // The statics are shared, so drive the real ones and release immediately.
    let check = {
        let _reclaim = RECLAIM.begin(None);
        super::check_background_sweeps(root.path())
    };
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(check.message.contains("IN FLIGHT"), "{}", check.message);
    assert_eq!(check.name, "background_sweeps");
    // Leaving HYGIENE untouched keeps the row's second line at "no pass yet"
    // unless another test in this binary ran a real sweep.
    let _ = &HYGIENE;
}

/// With neither sweep running and no slow pass recorded, the row is `Ok`.
///
/// Why this inverse matters: a row that warned unconditionally would fire on every
/// `tm doctor` run of a healthy daemon and train operators to ignore the one line
/// that means a sweep is eating the request path. It also pins that the row is
/// never `Unknown` — its source is this process's own atomics, always readable.
#[test]
#[serial_test::serial]
fn background_sweeps_row_is_ok_when_both_sweeps_are_idle() {
    let _on = SweepSwitchesOn::new();
    assert!(
        !RECLAIM.is_running() && !HYGIENE.is_running(),
        "the serial guard must give this test an idle daemon"
    );
    let root = tempfile::TempDir::new().expect("tempdir");
    let check = super::check_background_sweeps(root.path());
    assert_eq!(check.name, "background_sweeps");
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        !check.message.contains("IN FLIGHT"),
        "an idle daemon must not report a pass in flight: {}",
        check.message
    );
    assert!(
        check.message.contains("worktree-reclaim") && check.message.contains("inproject-hygiene"),
        "both sweeps must appear even when idle: {}",
        check.message
    );
}

/// 🔴 #8059 REGRESSION: a sweep that STARTED and has not finished reads as
/// running from a process that did not run it.
///
/// Why this shape: `tm doctor` runs daemonless (#6336), so the row is built in
/// the CLI process, where `RECLAIM`'s atomics are untouched — and on 2026-09-15
/// it reported "ON, idle, no pass yet" while the daemon's reclaim sweep had live
/// `git`/`gh` children. The sweep state is driven DIRECTLY here, with no timing
/// and no subprocess: the defect is about which state the row reads, not about
/// how long a pass lasts.
#[test]
#[serial_test::serial]
fn a_running_sweep_is_reported_by_a_process_that_did_not_run_it() {
    let _on = SweepSwitchesOn::new();
    let root = tempfile::TempDir::new().expect("tempdir");
    // Exactly what the sweep's own guard writes when a pass begins; this
    // process is the live writer, standing in for the daemon.
    let marker = super::super::sweep_state_file::marker_path(root.path(), RECLAIM_NAME);
    super::super::sweep_state_file::record_start(&marker);
    assert!(
        !RECLAIM.is_running(),
        "the reader's own atomics stay untouched — that is the whole bug"
    );

    let reclaim = rows(root.path())
        .into_iter()
        .find(|r| r.name == RECLAIM_NAME)
        .expect("the reclaim row exists");
    assert!(
        reclaim.running,
        "a started, unfinished sweep must read as running in another process"
    );

    let check = super::check_background_sweeps(root.path());
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check
            .message
            .contains("worktree-reclaim: ON, pass IN FLIGHT"),
        "the row must name the sweep that is running: {}",
        check.message
    );
}

/// A row with no marker still reports this process's OWN pass.
///
/// Why: the fold must not become marker-only. Inside the daemon the atomics are
/// the authoritative, syscall-free answer, and a marker that could not be
/// written must never downgrade the daemon's own row to "idle".
#[test]
#[serial_test::serial]
fn a_row_with_no_marker_still_reports_this_processs_own_pass() {
    let _on = SweepSwitchesOn::new();
    let root = tempfile::TempDir::new().expect("tempdir");
    let check = {
        let _hygiene = HYGIENE.begin(None);
        super::check_background_sweeps(root.path())
    };
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check
            .message
            .contains("inproject-hygiene: ON, pass IN FLIGHT"),
        "the in-memory pass must still be reported: {}",
        check.message
    );
}
