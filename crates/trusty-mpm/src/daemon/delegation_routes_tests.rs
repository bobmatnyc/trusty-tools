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

/// Drive the granted-worktree route and unwrap its body.
async fn granted_call(
    state: &Arc<DaemonState>,
    session: SessionId,
    req: SharedTreeDispatchRequest,
) -> SharedTreeWritersResponse {
    let Json(body) =
        granted_worktree_route(State(state.clone()), Path(session.0.to_string()), Json(req))
            .await
            .expect("route succeeds");
    body
}

/// The `PreToolUse` payload the daemon's own `matcher: "*"` hook observes — the
/// ORIGINAL dispatch, before the guard rewrote it.
fn unisolated_hook_payload(tool_use_id: &str) -> Value {
    serde_json::json!({
        "cwd": "/repo",
        "tool": "Agent",
        "tool_use_id": tool_use_id,
        "input": {"subagent_type": "rust-engineer", "description": "go"},
    })
}

#[tokio::test]
async fn a_grant_and_the_tracker_converge_in_either_order() {
    // #5769, and the reason the fix is an upsert rather than a second call to
    // `observe`. TWO writers describe one dispatch: this route, carrying the
    // isolation `tm hook --pm-guard` granted, and the daemon's own `matcher: "*"`
    // tracker hook, carrying the ORIGINAL unisolated payload. They fire on the
    // same event and race, and `on_dispatch` returns early when the `tool_use_id`
    // already exists — so an `observe`-based grant would correct the record only
    // in the orders it happened to win. Both orders must converge on ONE isolated
    // record, or `git pull` in this checkout is denied for the six hours of
    // `RUNNING_STALE_AFTER_SECS` on a writer that is not there.
    for guard_first in [true, false] {
        let (state, _dir, session) = hermetic();
        let original = unisolated_hook_payload("toolu_grant");
        let granted = dispatch(
            "/repo",
            "rust-engineer",
            Some("worktree"),
            Some("toolu_grant"),
        );

        if guard_first {
            let body = granted_call(&state, session, granted).await;
            assert!(body.claimed, "an empty checkout must be claimed");
            crate::daemon::services::delegation_tracker::observe(
                &state,
                session,
                HookEvent::PreToolUse,
                &original,
            );
        } else {
            crate::daemon::services::delegation_tracker::observe(
                &state,
                session,
                HookEvent::PreToolUse,
                &original,
            );
            // Causality: the tracker's record IS the phantom at this point.
            // Only an overwrite can clear it — `observe` would return early on
            // the `tool_use_id` it already wrote and change nothing.
            assert_eq!(state.delegations_for(session)[0].isolation, None);
            let body = granted_call(&state, session, granted).await;
            assert!(
                body.claimed,
                "the caller's own record is excluded, so the checkout is still free"
            );
        }

        let records = state.delegations_for(session);
        assert_eq!(
            records.len(),
            1,
            "guard_first={guard_first}: one dispatch must leave one record, got {records:?}"
        );
        assert_eq!(
            records[0].isolation.as_deref(),
            Some("worktree"),
            "guard_first={guard_first}: the granted isolation must survive both orders"
        );
        // The property the whole fix exists for: a later `git pull` in this
        // checkout asks exactly this question, and a non-empty answer denies it.
        assert!(
            state
                .live_shared_tree_writers(&PathBuf::from("/repo"), None)
                .is_empty(),
            "guard_first={guard_first}: a granted writer must not be named as writing here"
        );
    }
}

#[tokio::test]
async fn granted_worktree_route_records_nothing_without_isolation_or_a_tool_use_id() {
    // Both keys are load-bearing. Without `tool_use_id` the record could never
    // be found again, so writing one would leave a SECOND record beside the
    // tracker's — the phantom duplicated rather than removed. Without an
    // isolating mode there is no grant to record, and this route must not become
    // a way to claim a directory for an ordinary unisolated dispatch.
    for req in [
        dispatch("/repo", "rust-engineer", Some("worktree"), None),
        dispatch("/repo", "rust-engineer", None, Some("toolu_x")),
        dispatch("/repo", "rust-engineer", Some("nonsense"), Some("toolu_x")),
    ] {
        let (state, _dir, session) = hermetic();
        let body = granted_call(&state, session, req).await;
        assert!(!body.claimed);
        assert!(state.delegations_for(session).is_empty());
    }
}

#[tokio::test]
async fn granted_worktree_route_reports_a_live_writer_without_claiming() {
    // The deny arm the reorder restored (#5769 finding 2): the grant is emitted
    // only after this answer comes back empty, so a sibling already holding the
    // checkout denies the dispatch instead of silently sharing it — which is
    // what happened while the grant returned before the concurrency check.
    let (state, _dir, session) = hermetic();
    insert(
        &state,
        session,
        "python-engineer",
        "/repo",
        None,
        Some("toolu_first"),
        DelegationStatus::Running,
    );
    let body = granted_call(
        &state,
        session,
        dispatch(
            "/repo",
            "rust-engineer",
            Some("worktree"),
            Some("toolu_second"),
        ),
    )
    .await;
    assert_eq!(body.total, 1);
    assert_eq!(body.agents[0].agent, "python-engineer");
    assert!(!body.claimed, "a denied dispatch must record nothing");
    assert_eq!(state.delegations_for(session).len(), 1);
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
    // Another session's child in THIS directory now counts (ADR-0048). This
    // line used to assert the opposite — "not this session's problem" — and
    // that assumption is what made the guard blind to the reported incident:
    // the writers sharing one checkout each belonged to a different session.
    // Dedicated coverage is `shared_tree_writers_span_sessions_in_one_checkout`.
    insert(
        &state,
        SessionId(uuid::Uuid::new_v4()),
        "python-engineer",
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

    assert_eq!(
        body.total, 2,
        "the live unisolated engineers in this directory, whichever session dispatched them"
    );
    assert_eq!(body.agents.len(), 2, "{:?}", body.agents);
    assert_eq!(body.agents[0].agent, "python-engineer");
    assert_eq!(body.agents[1].agent, "rust-engineer");
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
async fn shared_tree_dispatch_route_answers_a_bash_query_without_claiming() {
    // ADR-0048 decision 10: the HEAD-moving Bash rule reads this route through
    // `pm_guard_dispatch::live_shared_tree_writers`, which sends the Bash call's
    // own payload — `tool: "Bash"` and an `input` projected to nothing. Two
    // halves of that contract are pinned here, because the rule depends on both.
    // It must ANSWER: a `git pull` beside a live writer is the deny this exists
    // for. And it must claim NOTHING: a pull is not a dispatch, and a claim it
    // took would occupy a directory no `SubagentStop` will ever release.
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
    let req = SharedTreeDispatchRequest {
        payload: serde_json::json!({
            "cwd": "/repo",
            "tool": "Bash",
            "input": {},
            "tool_use_id": "toolu_pull",
        }),
    };
    let body = call(&state, session, req).await;
    assert_eq!(body.total, 1, "the live writer must be reported");
    assert_eq!(body.agents[0].agent, "rust-engineer");
    assert!(!body.claimed, "a Bash query must never claim the tree");
    assert_eq!(
        state.delegations_for(session).len(),
        1,
        "no record may be added by a Bash query"
    );
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
async fn shared_tree_writers_span_sessions_in_one_checkout() {
    // THE REGRESSION (ADR-0048). The reported incident, reduced: three sessions
    // standing in one `mcp-services` checkout, each dispatching a writer into
    // it. Every guard saw an empty answer and admitted its writer, because the
    // answer was filtered to the ASKING session's own delegations before any
    // other test ran — so the writers were invisible to each other by
    // construction, not by timing. Downstream that produced branches switching
    // under each other and a commit landing on a workstream it did not belong
    // to.
    //
    // Session B asks about the directory session A's writer is already in.
    // With the session filter this answered 0 and claimed the tree a second
    // time; the hazard is a shared git HEAD, which belongs to the DIRECTORY and
    // knows nothing about session ids.
    let (state, _dir, session_a) = hermetic();
    let session_b = SessionId(uuid::Uuid::new_v4());
    let session_c = SessionId(uuid::Uuid::new_v4());

    insert(
        &state,
        session_a,
        "rust-engineer",
        "/repo/mcp-services",
        None,
        Some("toolu_A"),
        DelegationStatus::Running,
    );

    let body = call(
        &state,
        session_b,
        dispatch(
            "/repo/mcp-services",
            "python-engineer",
            None,
            Some("toolu_B"),
        ),
    )
    .await;
    assert_eq!(
        body.total, 1,
        "session B must see session A's writer in the same checkout"
    );
    assert_eq!(body.agents[0].agent, "rust-engineer");
    assert!(
        !body.claimed,
        "a denied dispatch must not also occupy the directory"
    );

    // A third session gets the same answer, naming every writer it would be
    // joining rather than only the ones its own session dispatched.
    insert(
        &state,
        session_b,
        "documentation",
        "/repo/mcp-services",
        None,
        Some("toolu_B2"),
        DelegationStatus::Running,
    );
    let body = call(
        &state,
        session_c,
        dispatch("/repo/mcp-services", "qa", None, Some("toolu_C")),
    )
    .await;
    assert_eq!(body.total, 2, "every live writer in the directory counts");

    // The widening must not reach across DIRECTORIES — that would deny every
    // dispatch on the machine as soon as one agent was running anywhere. A
    // worktree is a different directory and stays free, which is what makes the
    // remedy the deny offers actually work.
    let body = call(
        &state,
        session_c,
        dispatch(
            "/repo/mcp-services/.claude/worktrees/w1",
            "rust-engineer",
            None,
            Some("toolu_D"),
        ),
    )
    .await;
    assert_eq!(
        body.total, 0,
        "a different directory is a different git HEAD and must stay admitted"
    );
    assert!(body.claimed, "the first writer in its own tree claims it");
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
