//! Tests for the shared-tree-writers route (#4480).
//!
//! Why: the guard's whole decision rests on this answer, and each filter it
//! applies is a separate way to false-deny the PM. Each gets its own case.
//! Test: this *is* the test module.

use super::*;
use crate::core::agent::{Delegation, DelegationStatus};
use crate::core::paths::FrameworkPaths;

/// Build a hermetic state plus one session id, mirroring `api_tests`.
fn hermetic() -> (Arc<DaemonState>, tempfile::TempDir, SessionId) {
    let dir = tempfile::tempdir().expect("temp dir");
    let paths = FrameworkPaths::under(dir.path());
    let state = Arc::new(DaemonState::with_paths(&paths));
    (state, dir, SessionId(uuid::Uuid::new_v4()))
}

/// Insert one observed delegation with the given shape.
fn insert(
    state: &DaemonState,
    session: SessionId,
    agent: &str,
    cwd: &str,
    isolation: Option<&str>,
    tool_use_id: Option<&str>,
    status: DelegationStatus,
) {
    let mut d = Delegation::observed(session, agent, "task", tool_use_id.map(str::to_string));
    d.cwd = Some(PathBuf::from(cwd));
    d.isolation = isolation.map(str::to_string);
    d.status = status;
    state.upsert_delegation(d);
}

#[tokio::test]
async fn shared_tree_writers_route_reports_live_unisolated_writers() {
    let (state, _dir, session) = hermetic();
    // The one that matters: a live engineer in the shared tree with no
    // isolation. This is the record that makes a second dispatch dangerous.
    insert(
        &state,
        session,
        "rust-engineer",
        "/repo",
        None,
        Some("toolu_A"),
        DelegationStatus::Running,
    );
    // Each of the following must be filtered out, and each for its own reason.
    // A worktree-isolated sibling is exactly what the guard wants the PM to do.
    insert(
        &state,
        session,
        "rust-engineer",
        "/repo",
        Some("worktree"),
        Some("toolu_B"),
        DelegationStatus::Running,
    );
    // A read-only agent shares the tree harmlessly.
    insert(
        &state,
        session,
        "research",
        "/repo",
        None,
        Some("toolu_C"),
        DelegationStatus::Running,
    );
    // A finished engineer is not in flight.
    insert(
        &state,
        session,
        "python-engineer",
        "/repo",
        None,
        Some("toolu_D"),
        DelegationStatus::Completed,
    );
    // A `Stale` record is one tracking gave up on — it must not block a
    // dispatch for the rest of its retention window.
    insert(
        &state,
        session,
        "python-engineer",
        "/repo",
        None,
        Some("toolu_E"),
        DelegationStatus::Stale,
    );
    // A different directory is a different race.
    insert(
        &state,
        session,
        "python-engineer",
        "/elsewhere",
        None,
        Some("toolu_F"),
        DelegationStatus::Running,
    );
    // Another session's children are not this session's problem.
    insert(
        &state,
        SessionId(uuid::Uuid::new_v4()),
        "rust-engineer",
        "/repo",
        None,
        Some("toolu_G"),
        DelegationStatus::Running,
    );

    let Json(body) = shared_tree_writers_route(
        State(state),
        Path(session.0.to_string()),
        Query(SharedTreeWritersQuery {
            cwd: PathBuf::from("/repo"),
            exclude_tool_use_id: None,
        }),
    )
    .await
    .expect("route succeeds");

    assert_eq!(body.total, 1, "only the live unisolated engineer counts");
    assert_eq!(body.agents.len(), 1);
    assert_eq!(body.agents[0].agent, "rust-engineer");
    assert_eq!(body.agents[0].count, 1);
}

#[tokio::test]
async fn shared_tree_writers_route_excludes_the_callers_own_dispatch() {
    // Causality: the daemon's `matcher: "*"` hook and `tm hook --pm-guard`
    // race on the same PreToolUse. If the tracker wins, the asking dispatch is
    // already recorded — and without this exclusion the FIRST dispatch of a
    // session would find itself and be denied.
    let (state, _dir, session) = hermetic();
    insert(
        &state,
        session,
        "rust-engineer",
        "/repo",
        None,
        Some("toolu_SELF"),
        DelegationStatus::Running,
    );

    let Json(body) = shared_tree_writers_route(
        State(state),
        Path(session.0.to_string()),
        Query(SharedTreeWritersQuery {
            cwd: PathBuf::from("/repo"),
            exclude_tool_use_id: Some("toolu_SELF".into()),
        }),
    )
    .await
    .expect("route succeeds");

    assert_eq!(body.total, 0, "the caller must not find itself");
}

#[tokio::test]
async fn shared_tree_writers_route_counts_concurrent_same_agent_dispatches() {
    let (state, _dir, session) = hermetic();
    for id in ["toolu_1", "toolu_2"] {
        insert(
            &state,
            session,
            "rust-engineer",
            "/repo",
            None,
            Some(id),
            DelegationStatus::Running,
        );
    }

    let Json(body) = shared_tree_writers_route(
        State(state),
        Path(session.0.to_string()),
        Query(SharedTreeWritersQuery {
            cwd: PathBuf::from("/repo"),
            exclude_tool_use_id: None,
        }),
    )
    .await
    .expect("route succeeds");

    assert_eq!(body.total, 2);
    assert_eq!(body.agents.len(), 1, "one row per distinct name");
    assert_eq!(body.agents[0].count, 2);
}

#[tokio::test]
async fn shared_tree_writers_route_is_empty_for_an_unknown_session() {
    // An unknown session has no delegations. Answering 404 would read to the
    // guard as an error rather than as "nobody else is here".
    let (state, _dir, _session) = hermetic();
    let Json(body) = shared_tree_writers_route(
        State(state),
        Path(uuid::Uuid::new_v4().to_string()),
        Query(SharedTreeWritersQuery {
            cwd: PathBuf::from("/repo"),
            exclude_tool_use_id: None,
        }),
    )
    .await
    .expect("route succeeds");
    assert_eq!(body.total, 0);
}

#[tokio::test]
async fn shared_tree_writers_route_rejects_a_malformed_session_id() {
    let (state, _dir, _session) = hermetic();
    let err = shared_tree_writers_route(
        State(state),
        Path("not-a-uuid".to_string()),
        Query(SharedTreeWritersQuery {
            cwd: PathBuf::from("/repo"),
            exclude_tool_use_id: None,
        }),
    )
    .await
    .expect_err("a malformed id is a client error");
    assert!(matches!(err, DaemonError::InvalidRequest(_)));
}
