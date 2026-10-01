//! Coverage for the kernel-bound session `claude` registry (#8531).

use super::*;
use crate::core::agent::Delegation;

const CLAUDE: ClaudeProcess = ClaudeProcess {
    pid: 300,
    start_time: 50,
};

/// A process table: 100 (the peer) → 200 (a shell) → 300 (`claude`).
fn table(pid: u32) -> Result<ProcessFacts, String> {
    let (parent, start_time) = match pid {
        100 => (200, 90),
        200 => (300, 60),
        300 => (1, 50),
        _ => return Err(format!("no entry for {pid}")),
    };
    Ok(ProcessFacts {
        parent: Some(parent),
        start_time,
    })
}

/// #8531: first writer wins; a second announcement never rebinds.
#[test]
fn a_session_is_bound_once_first_writer_wins_8531() {
    let state = DaemonState::new();
    let session = SessionId::new();
    assert_eq!(state.session_claudes().get(session), None);
    state.bind_session_claude(session, CLAUDE).expect("vacant");
    let other = ClaudeProcess {
        pid: 999,
        start_time: 1,
    };
    assert!(state.bind_session_claude(session, other).is_err());
    assert_eq!(state.session_claudes().get(session), Some(CLAUDE));
}

/// #8531 Fail-Open Check: an id that already owns records — announced to
/// the daemon by a process nothing identified — is never bound.
#[test]
fn a_session_that_already_owns_records_is_not_bound_8531() {
    let state = DaemonState::new();
    let session = SessionId::new();
    state.upsert_delegation(Delegation::observed(session, "engineer", "task", None));
    let got = state.bind_session_claude(session, CLAUDE);
    assert!(
        got.as_ref()
            .is_err_and(|e| e.contains("already owns delegation records")),
        "{got:?}"
    );
    assert_eq!(state.session_claudes().get(session), None);
}

/// #8531: the peer's nearest `claude`, with its start time.
#[test]
fn the_peer_claude_is_the_nearest_claude_ancestor_8531() {
    let got = peer_claude_with(100, 1_000, table, |pid| Ok(pid == 300));
    assert_eq!(got, Ok(CLAUDE));
}

/// #8531 MEDIUM, Fail-Open Check: a peer pid now naming a process that
/// started after the request arrived is a reused pid, and is refused before
/// any walk.
#[test]
fn a_peer_started_after_the_request_is_refused_8531() {
    let got = peer_claude_with(100, 89, table, |_| panic!("no walk for a reused pid"));
    assert!(
        got.as_ref()
            .is_err_and(|e| e.contains("started after the request arrived")),
        "{got:?}"
    );
}

/// #8531 Fail-Open Check: a peer or an ancestor the table cannot read
/// refuses.
#[test]
fn an_unreadable_peer_is_refused_8531() {
    let got = peer_claude_with(7, 1_000, table, |_| Ok(false));
    assert!(
        got.as_ref().is_err_and(|e| e.contains("no entry for 7")),
        "{got:?}"
    );
    let got = peer_claude_with(100, 1_000, table, |_| Err("ps failed".to_string()));
    assert!(
        got.as_ref()
            .is_err_and(|e| e.contains("ancestry") && e.contains("ps failed")),
        "{got:?}"
    );
}

/// #8531 Fail-Open Check: a peer with no `claude` above it runs in no
/// session.
#[test]
fn a_peer_with_no_claude_above_it_is_refused_8531() {
    let got = peer_claude_with(100, 1_000, table, |_| Ok(false));
    assert!(
        got.as_ref()
            .is_err_and(|e| e.contains("no claude session process")),
        "{got:?}"
    );
}
