//! Tests for the #9007 unbound-delegation count.
//! Test: this file IS the test module.

use super::*;
use crate::core::agent::{Delegation, DelegationStatus};
use crate::core::paths::FrameworkPaths;
use crate::core::session::SessionId;
use crate::core::twin_identity::ClaudeProcess;
use crate::daemon::state::session_claudes::SESSION_CLAUDES_FILE;
use crate::daemon::state::sessions::RUNNING_STALE_AFTER_SECS;

const CLAUDE: ClaudeProcess = ClaudeProcess {
    pid: 300,
    start_time: 50,
};

#[test]
fn no_records_counts_zero_9007() {
    let state = DaemonState::new();
    assert_eq!(count_unbound_waiting(&state, chrono::Utc::now()), Ok(0));
}

/// Only a live, unbound, not-yet-stale record counts; a bound session, a
/// terminal record, and a record past the budget do not.
#[test]
fn only_unbound_unstaled_live_records_count_9007() {
    let state = DaemonState::new();
    let now = chrono::Utc::now();
    let bound = SessionId::new();
    state.bind_session_claude(bound, CLAUDE).expect("bind");
    state.upsert_delegation(Delegation::observed(bound, "engineer", "bound", None));

    let unbound = SessionId::new();
    state.upsert_delegation(Delegation::observed(unbound, "engineer", "counts", None));
    let mut done = Delegation::observed(unbound, "engineer", "done", None);
    done.status = DelegationStatus::Completed;
    state.upsert_delegation(done);

    assert_eq!(count_unbound_waiting(&state, now), Ok(1));
    // Past the 6 h budget the next sweep stales it, so it no longer waits.
    let later = now + chrono::Duration::seconds(RUNNING_STALE_AFTER_SECS + 60);
    assert_eq!(count_unbound_waiting(&state, later), Ok(0));
}

/// #9007 Fail-Open Check: a sealed registry makes every id read as unbound, so
/// the count is an error naming the seal, never a number.
#[test]
fn a_sealed_registry_is_an_error_not_a_zero_9007() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Write a registry through a real state so the file sits where a daemon
    // loads it from, then corrupt it and load again.
    let first = DaemonState::with_paths(&FrameworkPaths::under(dir.path()));
    first
        .settle_unproven_session(SessionId::new())
        .expect("saved");
    let file = first.framework_root().join(SESSION_CLAUDES_FILE);
    std::fs::write(&file, b"{\"version\":1,\"sessions\":").expect("corrupt");
    let state = DaemonState::with_paths(&FrameworkPaths::under(dir.path()));
    assert!(state.session_claudes().sealed().is_some(), "loaded sealed");
    state.upsert_delegation(Delegation::observed(
        SessionId::new(),
        "engineer",
        "t",
        None,
    ));
    let got = count_unbound_waiting(&state, chrono::Utc::now());
    assert!(got.as_ref().is_err_and(|e| e.contains("sealed")), "{got:?}");
}
