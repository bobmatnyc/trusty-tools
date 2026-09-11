//! Tests for the automatic post-merge reclaim sweep (#7504).
//!
//! Why: the engine's own gates are covered exhaustively in
//! `session_manager::worktree_reclaim_sweep_tests`, each with a refusal test that
//! fails if its gate is deleted. What is NOT covered there is this layer's three
//! decisions: whether the sweep runs at all, whether it refuses a host state it
//! must not sweep, and whether a failed removal is reported rather than folded
//! into a success.
//! What: pure policy tests for the two env knobs, a Fail-Open test on
//! [`SweepReport`], and two tests driving the real loop against a scratch
//! framework root.
//!
//! The loop tests are `#[serial_test::serial]`. The resource they share is
//! process-global `$HOME`, which ~20 tests in this binary write: the host-state
//! refusal that stops a scratch-rooted daemon from sweeping the operator's real
//! worktrees reads `$HOME`, and a concurrent rewrite of it could let a
//! DESTRUCTIVE pass through. An in-process lock is the right instrument for
//! in-process state — under nextest each test already has its own process, so
//! there is no cross-process `$HOME` to guard (#4162).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::{
    DEFAULT_INTERVAL_SECS, SweepReport, parse_enabled, parse_interval_secs, reclaim, reclaim_loop,
};
use crate::daemon::state::DaemonState;
use crate::session_manager::worktree_reclaim::{ReclaimMode, ReclaimOutcome, ReclaimSurvey};

/// A `DaemonState` rooted at a throwaway directory, plus the directory.
///
/// `$HOME` is left exactly as the test binary inherited it — that is the #6348
/// quadrant the host-state refusal exists for, and reassigning it would silence
/// the root arm behind the `$HOME` arm that already worked.
fn scratch_root_state() -> (Arc<DaemonState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("scratch framework root");
    let state = Arc::new(DaemonState::with_root(dir.path().to_path_buf()));
    (state, dir)
}

/// The sweep is ON unless explicitly switched off (#7504).
///
/// Why this is the assertion rather than the inverse: a reclaim that must be
/// enabled is the manual step #2919 already shipped, which is what this issue
/// exists to remove. A default flip would leave every test here green while the
/// feature did nothing.
#[test]
fn parse_enabled_defaults_on_and_honours_the_off_switch() {
    assert!(parse_enabled(None), "unset must enable the sweep");
    assert!(parse_enabled(Some("1")));
    assert!(parse_enabled(Some("true")));
    assert!(
        parse_enabled(Some("")),
        "an empty value is not an off switch"
    );
    for off in ["0", "false", "off", "no", " OFF ", "False"] {
        assert!(!parse_enabled(Some(off)), "`{off}` must disable the sweep");
    }
}

/// Junk and zero fall back to the default cadence, never to "disabled" (#7504).
///
/// A zero period panics `tokio::time::interval`, and inferring "off" from a typo
/// would turn a fat-fingered number into silent non-reclamation — that is
/// `TRUSTY_MPM_WORKTREE_RECLAIM`'s job.
#[test]
fn parse_interval_falls_back_on_junk_and_zero() {
    assert_eq!(parse_interval_secs(None), DEFAULT_INTERVAL_SECS);
    assert_eq!(parse_interval_secs(Some("nonsense")), DEFAULT_INTERVAL_SECS);
    assert_eq!(parse_interval_secs(Some("0")), DEFAULT_INTERVAL_SECS);
    assert_eq!(parse_interval_secs(Some("-5")), DEFAULT_INTERVAL_SECS);
    assert_eq!(parse_interval_secs(Some(" 120 ")), 120);
}

/// An outcome carrying a failed removal, with everything else clean.
fn outcome_with(removal_failed: Vec<String>, removed: Vec<PathBuf>) -> ReclaimOutcome {
    ReclaimOutcome {
        survey: ReclaimSurvey::from_candidates(Vec::new()),
        removed,
        removed_bytes: 0,
        refused_at_recheck: Vec::new(),
        removal_failed,
    }
}

/// A removal that did not complete makes the tick a FAILURE (#7504).
///
/// The Fail-Open check: without this branch the loop logs the same `info` line for
/// a tick that reclaimed two worktrees and for one that reclaimed two and left a
/// third half-removed. Fails if `SweepReport::failed` ever keys on `refused`, or
/// on nothing.
#[test]
fn a_tick_with_a_failed_removal_is_reported_as_a_failure() {
    let outcome = outcome_with(
        vec!["/tmp/wt: git worktree remove exited 128".to_string()],
        vec![PathBuf::from("/tmp/other")],
    );
    let report = SweepReport::from_outcome(&outcome);
    assert!(report.failed(), "report: {report:?}");
    assert_eq!(report.failed, 1);
    assert_eq!(
        report.reclaimed, 1,
        "a partially successful tick still reports what it did reclaim"
    );
}

/// A tick that removed things cleanly is not a failure (#7504).
///
/// The inverse of the test above: a `failed()` that is always true would make
/// every sweep log at `warn` and train operators to ignore the line.
#[test]
fn a_clean_tick_is_not_a_failure() {
    let outcome = outcome_with(Vec::new(), vec![PathBuf::from("/tmp/gone")]);
    let report = SweepReport::from_outcome(&outcome);
    assert!(!report.failed(), "report: {report:?}");
}

/// A candidate a gate SPARED is not a failure (#7504).
///
/// Spared worktrees are the sweep working. Counting them as failures would make
/// the `warn` arm fire on every healthy tick in a workspace with one live agent.
#[test]
fn a_spared_candidate_is_not_a_failure() {
    let mut outcome = outcome_with(Vec::new(), Vec::new());
    outcome
        .refused_at_recheck
        .push("/tmp/wt: holds unsaved work".to_string());
    let report = SweepReport::from_outcome(&outcome);
    assert!(!report.failed(), "report: {report:?}");
    assert_eq!(report.refused, 1);
}

/// A sweep against a scratch-rooted daemon reclaims nothing (#7504, #6348).
///
/// Why it matters more here than anywhere else: this loop is ON by default and
/// DELETES directories. A test process whose framework root is a tempdir has an
/// empty session registry, so every worktree the operator is really working in
/// looks unclaimed to it. `run_one_tick` refuses before any survey runs.
#[tokio::test]
#[serial_test::serial]
async fn reclaim_loop_reclaims_nothing_on_a_scratch_framework_root() {
    let (state, dir) = scratch_root_state();
    super::run_one_tick(&state).await;
    assert!(
        dir.path().exists(),
        "the tick must not disturb the scratch root itself"
    );
}

/// The loop exits when its cancel token fires (#7504).
///
/// A liveness pin: a loop that ignores cancellation is dropped mid-sweep on
/// SIGTERM, which is how a `git worktree remove` gets interrupted between its
/// directory removal and its administrative-file removal.
#[tokio::test]
#[serial_test::serial]
async fn reclaim_loop_exits_on_cancel() {
    let (state, _dir) = scratch_root_state();
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = tokio::spawn(reclaim_loop(
        state,
        Duration::from_secs(3600),
        cancel.child_token(),
    ));
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(30), handle)
        .await
        .expect("the loop must exit on cancel rather than hang")
        .expect("the loop must not panic");
}

/// [`reclaim`] against an EMPTY workspace root returns an outcome rather than
/// panicking (#7504).
///
/// Why the root is a tempdir and not the operator's: the entry point binds five
/// probes, one of which blocks on the async session store from a blocking-pool
/// thread — a wiring mistake there surfaces as a panicked `JoinError`, and this
/// asserts it is reported as `Err` rather than as an empty success. Surveying the
/// real workspace would prove the same thing and take ten minutes, because the
/// destructive path runs with an unbounded budget by design.
#[tokio::test]
#[serial_test::serial]
async fn reclaim_on_an_empty_root_reclaims_nothing() {
    let (state, _dir) = scratch_root_state();
    let empty_root = tempfile::tempdir().expect("empty workspace root");
    let out = reclaim(&state, empty_root.path(), ReclaimMode::Remove, None)
        .await
        .expect("the probe assembly must not panic");
    assert!(
        out.removed.is_empty() && out.removal_failed.is_empty(),
        "a workspace root with no registered worktree has nothing to reclaim: {out:?}"
    );
    assert_eq!(out.survey.candidates.len(), 0, "outcome: {out:?}");
}
