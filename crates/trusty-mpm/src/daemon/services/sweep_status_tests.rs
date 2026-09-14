//! Tests for the maintenance lane and the sweep status the doctor row reads
//! (#7965).
//!
//! The statics are process-global, which is deliberate — there is one daemon per
//! process and one lane per daemon. The tests below therefore assert on a LOCAL
//! `SweepStatus` where they can, and only `the_lane_serialises_two_sweeps`
//! touches the shared lane.

use std::time::Duration;

use super::{HYGIENE, RECLAIM, SweepStatus, lane, rows};
use crate::core::doctor::CheckStatus;

/// A completed pass records its duration and clears the running flag.
#[test]
fn sweep_status_records_the_last_pass_duration() {
    static S: SweepStatus = SweepStatus::new();
    assert_eq!(S.last_pass(), None, "no pass has run");
    assert!(!S.is_running());
    {
        let _guard = S.begin();
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
    static S: SweepStatus = SweepStatus::new();
    let caught = std::panic::catch_unwind(|| {
        let _guard = S.begin();
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
    let rows = rows();
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
#[test]
fn background_sweeps_row_warns_on_a_slow_pass() {
    // The statics are shared, so drive the real ones and release immediately.
    let check = {
        let _reclaim = RECLAIM.begin();
        super::check_background_sweeps()
    };
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(check.message.contains("IN FLIGHT"), "{}", check.message);
    assert_eq!(check.name, "background_sweeps");
    // Leaving HYGIENE untouched keeps the row's second line at "no pass yet"
    // unless another test in this binary ran a real sweep.
    let _ = &HYGIENE;
}
