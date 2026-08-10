//! Tests for the shared-tree dispatch route (#4480, #5324).
//!
//! Why: the guard's whole decision rests on this answer, and each filter it
//! applies is a separate way to false-deny the PM. Each gets its own case. Since
//! #5324 the route also CLAIMS the directory, so each way it could claim one it
//! should not have — a read-only agent, an isolated dispatch, a non-dispatch
//! tool, a payload with no directory — is its own case too.
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

/// One dispatch in the daemon's forwarded hook shape — what the guard POSTs.
fn dispatch(
    cwd: &str,
    agent: &str,
    isolation: Option<&str>,
    tool_use_id: Option<&str>,
) -> SharedTreeDispatchRequest {
    let mut input = serde_json::json!({"subagent_type": agent, "description": "go"});
    if let Some(i) = isolation {
        input["isolation"] = Value::String(i.to_string());
    }
    let mut payload = serde_json::json!({"cwd": cwd, "tool": "Agent", "input": input});
    if let Some(id) = tool_use_id {
        payload["tool_use_id"] = Value::String(id.to_string());
    }
    SharedTreeDispatchRequest { payload }
}

/// Drive the route and unwrap its body.
async fn call(
    state: &Arc<DaemonState>,
    session: SessionId,
    req: SharedTreeDispatchRequest,
) -> SharedTreeWritersResponse {
    let Json(body) =
        shared_tree_dispatch_route(State(state.clone()), Path(session.0.to_string()), Json(req))
            .await
            .expect("route succeeds");
    body
}

#[tokio::test]
async fn shared_tree_dispatch_route_reports_live_unisolated_writers() {
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

    let body = call(
        &state,
        session,
        dispatch("/repo", "rust-engineer", None, Some("toolu_NEW")),
    )
    .await;

    assert_eq!(body.total, 1, "only the live unisolated engineer counts");
    assert_eq!(body.agents.len(), 1);
    assert_eq!(body.agents[0].agent, "rust-engineer");
    assert_eq!(body.agents[0].count, 1);
    assert!(!body.claimed, "a denied dispatch must not claim the tree");
}

#[tokio::test]
async fn shared_tree_dispatch_route_excludes_the_callers_own_dispatch() {
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

    let body = call(
        &state,
        session,
        dispatch("/repo", "rust-engineer", None, Some("toolu_SELF")),
    )
    .await;

    assert_eq!(body.total, 0, "the caller must not find itself");
    // Redelivery of the same dispatch must not add a second record either —
    // the tracker's observer is idempotent on `tool_use_id`.
    assert_eq!(state.delegations_for(session).len(), 1);
}

#[tokio::test]
async fn shared_tree_dispatch_route_counts_concurrent_same_agent_dispatches() {
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

    let body = call(
        &state,
        session,
        dispatch("/repo", "rust-engineer", None, Some("toolu_3")),
    )
    .await;

    assert_eq!(body.total, 2);
    assert_eq!(body.agents.len(), 1, "one row per distinct name");
    assert_eq!(body.agents[0].count, 2);
}

#[tokio::test]
async fn shared_tree_dispatch_route_is_empty_for_an_unknown_session() {
    // An unknown session has no delegations. Answering 404 would read to the
    // guard as an error rather than as "nobody else is here".
    let (state, _dir, _session) = hermetic();
    let Json(body) = shared_tree_dispatch_route(
        State(state),
        Path(uuid::Uuid::new_v4().to_string()),
        Json(dispatch("/repo", "rust-engineer", None, Some("toolu_X"))),
    )
    .await
    .expect("route succeeds");
    assert_eq!(body.total, 0);
}

#[tokio::test]
async fn shared_tree_dispatch_route_rejects_a_malformed_session_id() {
    let (state, _dir, _session) = hermetic();
    let err = shared_tree_dispatch_route(
        State(state),
        Path("not-a-uuid".to_string()),
        Json(dispatch("/repo", "rust-engineer", None, Some("toolu_X"))),
    )
    .await
    .expect_err("a malformed id is a client error");
    assert!(matches!(err, DaemonError::InvalidRequest(_)));
}

// ---------------------------------------------------------------------------
// #5324 — the claim
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shared_tree_dispatch_route_denies_the_second_claim() {
    // THE regression case. Two dispatches into one directory, neither recorded
    // by anything else. Pre-#5324 this route only answered, so BOTH calls saw an
    // empty set and both were admitted. The claim taken by the first is what
    // the second now finds.
    let (state, _dir, session) = hermetic();

    let first = call(
        &state,
        session,
        dispatch("/repo", "rust-engineer", None, Some("toolu_A")),
    )
    .await;
    assert_eq!(first.total, 0, "the first dispatch must be admitted");
    assert!(first.claimed, "and it must take the directory");

    let second = call(
        &state,
        session,
        dispatch("/repo", "python-engineer", None, Some("toolu_B")),
    )
    .await;
    assert_eq!(second.total, 1, "the second must find the first");
    assert_eq!(second.agents[0].agent, "rust-engineer");
    assert!(!second.claimed);
}

#[tokio::test]
async fn shared_tree_dispatch_route_reserves_the_tree_on_an_empty_answer() {
    // The claim is not a new kind of state: it is the delegation record the
    // tracker's own PreToolUse observer writes, carrying the same correlation
    // key, directory, and isolation — so it is released by the same
    // `SubagentStop` and swept by the same staleness sweep.
    let (state, _dir, session) = hermetic();
    call(
        &state,
        session,
        dispatch("/repo", "rust-engineer", None, Some("toolu_A")),
    )
    .await;

    let records = state.delegations_for(session);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].agent, "rust-engineer");
    assert_eq!(records[0].tool_use_id.as_deref(), Some("toolu_A"));
    assert_eq!(
        records[0].cwd.as_deref(),
        Some(PathBuf::from("/repo").as_path())
    );
    assert_eq!(records[0].status, DelegationStatus::Running);
}

#[tokio::test]
async fn shared_tree_dispatch_route_does_not_reserve_when_it_denies() {
    // A denied dispatch never runs, so recording it would occupy the directory
    // for a subagent that does not exist — a false-deny generator.
    let (state, _dir, session) = hermetic();
    insert(
        &state,
        session,
        "rust-engineer",
        "/repo",
        None,
        Some("toolu_A"),
        DelegationStatus::Running,
    );

    let body = call(
        &state,
        session,
        dispatch("/repo", "python-engineer", None, Some("toolu_B")),
    )
    .await;

    assert!(!body.claimed);
    assert_eq!(
        state.delegations_for(session).len(),
        1,
        "the denied dispatch must leave no record of its own"
    );
}

#[tokio::test]
async fn shared_tree_dispatch_route_does_not_reserve_a_read_only_agent() {
    // The daemon re-derives eligibility rather than trusting the caller: a
    // research/review dispatch, or one that declared isolation, must never
    // occupy a directory even if something asked it to.
    for (agent, isolation) in [
        ("research", None),
        ("code-critic", None),
        ("rust-engineer", Some("worktree")),
        ("rust-engineer", Some("remote")),
        ("some-project-agent", None),
    ] {
        let (state, _dir, session) = hermetic();
        let body = call(
            &state,
            session,
            dispatch("/repo", agent, isolation, Some("toolu_A")),
        )
        .await;
        assert!(
            !body.claimed,
            "{agent}/{isolation:?} must not claim the tree"
        );
        assert!(state.delegations_for(session).is_empty());
    }
}

#[tokio::test]
async fn shared_tree_dispatch_route_does_not_reserve_a_non_dispatch_tool() {
    // The recording path is the tracker's, which acts only on a dispatch tool.
    // Reporting a claim it did not take would be a lie the response tells.
    let (state, _dir, session) = hermetic();
    let mut req = dispatch("/repo", "rust-engineer", None, Some("toolu_A"));
    req.payload["tool"] = Value::String("Read".into());
    let body = call(&state, session, req).await;
    assert!(!body.claimed);
    assert!(state.delegations_for(session).is_empty());
}

#[tokio::test]
async fn shared_tree_dispatch_route_is_empty_without_a_cwd() {
    // Declared fail-open branch: with no directory in the payload there is
    // nothing to compare against and nothing to claim. Seeding a live writer
    // makes the assertion causal — only the missing-cwd short-circuit can keep
    // the answer empty.
    let (state, _dir, session) = hermetic();
    insert(
        &state,
        session,
        "rust-engineer",
        "/repo",
        None,
        Some("toolu_A"),
        DelegationStatus::Running,
    );
    let mut req = dispatch("/repo", "rust-engineer", None, Some("toolu_B"));
    req.payload["cwd"] = Value::String(String::new());

    let body = call(&state, session, req).await;
    assert_eq!(body.total, 0);
    assert!(!body.claimed);
    assert_eq!(state.delegations_for(session).len(), 1);
}

#[tokio::test]
async fn shared_tree_dispatch_route_claim_is_idempotent_on_redelivery() {
    // A redelivered hook must not deny the dispatch it already admitted, and
    // must not double-record it. Both follow from excluding the caller's own
    // `tool_use_id` and from the observer's own idempotence.
    let (state, _dir, session) = hermetic();
    for _ in 0..3 {
        let body = call(
            &state,
            session,
            dispatch("/repo", "rust-engineer", None, Some("toolu_A")),
        )
        .await;
        assert_eq!(body.total, 0, "a redelivery must stay admitted");
    }
    assert_eq!(state.delegations_for(session).len(), 1);
}
