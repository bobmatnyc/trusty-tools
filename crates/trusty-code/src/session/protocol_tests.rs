//! Tests for `session::protocol` (parameter validation, error mapping). Split
//! out of `protocol.rs` per the crate's `_tests.rs` sibling-file convention
//! (see `registry_tests`/`sessions_write_tests` for precedent) so this
//! production file stays under its 500-SLOC cap.

use super::*;
use tokio::sync::mpsc;
use trusty_mcp::Request;

fn test_ctx() -> ConnectionContext {
    let (tx, _rx) = mpsc::unbounded_channel();
    ConnectionContext::new(tx)
}

/// Every `session.*` method must be reachable through a `Router` built
/// by `register` (proves the wiring, not just the free functions).
#[tokio::test]
async fn register_wires_every_session_method() {
    let registry = Arc::new(SessionRegistry::new());
    let mut router = Router::new();
    register(
        &mut router,
        registry.clone(),
        crate::workstreams::test_shared_store().await,
    );

    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    let cases: &[(&str, Value)] = &[
        ("session.list", json!({})),
        ("session.status", json!({"session_id": session.id})),
        (
            "session.send",
            json!({"session_id": session.id, "input": "hi"}),
        ),
        ("session.attach", json!({"session_id": session.id})),
        ("session.detach", json!({"session_id": session.id})),
        ("session.get_transcript", json!({"session_id": session.id})),
        ("session.get_goals", json!({"session_id": session.id})),
        ("session.get_readiness", json!({"session_id": session.id})),
        ("session.get_agents", json!({"session_id": session.id})),
        (
            "session.get_context_budget",
            json!({"session_id": session.id}),
        ),
        (
            "session.get_search_audit",
            json!({"session_id": session.id}),
        ),
    ];
    for (method, params) in cases {
        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: method.to_string(),
            params: Some(params.clone()),
        };
        let resp = router.dispatch(req, &test_ctx()).await;
        assert!(
            resp.error.is_none(),
            "{method} should succeed, got {:?}",
            resp.error
        );
    }

    // `session.cancel` last since it terminates the session.
    let req = Request {
        jsonrpc: Some("2.0".to_string()),
        id: Some(json!(1)),
        method: "session.cancel".to_string(),
        params: Some(json!({"session_id": session.id})),
    };
    let resp = router.dispatch(req, &test_ctx()).await;
    assert!(
        resp.error.is_none(),
        "session.cancel should succeed, got {:?}",
        resp.error
    );
}

/// `session.set_goal`/`session.clear_goal` must be reachable through the
/// `Router` (proving the wiring) even though a freshly-created session
/// with no `task.run` yet has no transcript to write into — the
/// documented #2350 "no transcript yet" error IS the proof the request
/// was routed to the real handler rather than `-32601 method not found`.
#[tokio::test]
async fn register_wires_set_goal_and_clear_goal() {
    let registry = Arc::new(SessionRegistry::new());
    let mut router = Router::new();
    register(
        &mut router,
        registry.clone(),
        crate::workstreams::test_shared_store().await,
    );
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

    for method in ["session.set_goal", "session.clear_goal"] {
        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: method.to_string(),
            params: Some(json!({"session_id": session.id, "slot": 1, "text": "x"})),
        };
        let resp = router.dispatch(req, &test_ctx()).await;
        let error = resp
            .error
            .unwrap_or_else(|| panic!("{method} must error (no transcript yet)"));
        assert_eq!(error.code, -32003, "{method} wrong error code");
    }
}

/// An empty `task` must map to `-32003 invalid_argument`, not silently
/// create a blank session.
#[tokio::test]
async fn create_rejects_empty_task() {
    let registry = SessionRegistry::new();
    let workstreams = crate::workstreams::test_shared_store().await;
    let err = create(&registry, &workstreams, json!({"task": "   "}), test_ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, -32003);
}

/// #8184: a `session.create` with no explicit choice mints the SOLO agent.
///
/// Why: `tcode tui`'s interactive session is minted through this method with
/// no `delegate` param, and the whole point of the issue is that such a
/// session reads and edits files itself instead of delegating. The default
/// lives HERE, on the daemon, so every client inherits it.
/// What: calls `create` with `task` only and asserts the returned session
/// reports `no_delegate: true`. FAILS before #8184 — the field did not exist,
/// so the assertion reads `null`.
/// Test: this test.
#[tokio::test]
async fn create_defaults_to_the_solo_agent() {
    let registry = SessionRegistry::new();
    let workstreams = crate::workstreams::test_shared_store().await;
    let value = create(
        &registry,
        &workstreams,
        json!({"task": "add a doc comment to fn X"}),
        test_ctx(),
    )
    .await
    .expect("create must succeed");
    assert_eq!(
        value["no_delegate"],
        json!(true),
        "an interactive session defaults to the solo agent; got {value}"
    );
}

/// #8184 companion: `delegate: true` is the opt-in back to the PM.
///
/// Why: without this, `create_defaults_to_the_solo_agent` would also pass if
/// the flag were hard-wired to `true` and delegation became unreachable — the
/// fix would be indistinguishable from a regression.
/// What: the same call with `delegate: true`, asserting `no_delegate: false`.
/// FAILS before #8184 — the param was unknown and the field absent.
/// Test: this test.
#[tokio::test]
async fn create_with_delegate_true_keeps_the_delegating_pm() {
    let registry = SessionRegistry::new();
    let workstreams = crate::workstreams::test_shared_store().await;
    let value = create(
        &registry,
        &workstreams,
        json!({"task": "ship the feature", "delegate": true}),
        test_ctx(),
    )
    .await
    .expect("create must succeed");
    assert_eq!(
        value["no_delegate"],
        json!(false),
        "delegate: true must mint the PM shape; got {value}"
    );
}

/// #8184: a PROJECTLESS default session still succeeds, and still runs solo.
///
/// Why: `tcode tui` without `--project` is a first-class state (the run works
/// in the executor's ephemeral scratch root), so the new default must not have
/// made the projectless mint conditional on a binding.
/// What: `create` with `task` only, asserting both the projectless binding and
/// `no_delegate: true`. FAILS before #8184 on the second assertion.
/// Test: this test.
#[tokio::test]
async fn create_without_project_defaults_to_the_solo_agent() {
    let registry = SessionRegistry::new();
    let workstreams = crate::workstreams::test_shared_store().await;
    let value = create(
        &registry,
        &workstreams,
        json!({"task": "just chat"}),
        test_ctx(),
    )
    .await
    .expect("a projectless create must stay valid");
    assert_eq!(value["binding"]["state"], "projectless");
    assert_eq!(
        value["no_delegate"],
        json!(true),
        "a projectless session runs solo against the scratch root; got {value}"
    );
}

/// A well-formed `session.create` call must return a running session.
#[tokio::test]
async fn create_returns_running_session() {
    let registry = SessionRegistry::new();
    let workstreams = crate::workstreams::test_shared_store().await;
    let result = create(
        &registry,
        &workstreams,
        json!({"task": "do it"}),
        test_ctx(),
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "running");
    assert_eq!(result["task"], "do it");
}

/// `session.list` must wrap its result under a `"sessions"` key.
#[tokio::test]
async fn list_returns_sessions_key() {
    let registry = SessionRegistry::new();
    registry.create("a".to_string(), None, crate::binding::ProjectBinding::None);
    let result = list(&registry, Value::Null, test_ctx()).await.unwrap();
    assert_eq!(result["sessions"].as_array().unwrap().len(), 1);
}

/// `session.status` on an unknown id must map to `session_not_found`.
#[tokio::test]
async fn status_unknown_session_maps_to_session_not_found() {
    let registry = SessionRegistry::new();
    let err = status(&registry, json!({"session_id": "nope"}), test_ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `session.get_transcript` on an unknown id must map to
/// `session_not_found` (#2058).
#[tokio::test]
async fn get_transcript_unknown_session_maps_to_session_not_found() {
    let registry = SessionRegistry::new();
    let err = get_transcript(&registry, json!({"session_id": "nope"}), test_ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `session.get_transcript` on a session that has never run a task
/// returns an empty transcript, not an error (#2058).
#[tokio::test]
async fn get_transcript_on_never_run_session_is_empty() {
    let registry = SessionRegistry::new();
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let result = get_transcript(&registry, json!({"session_id": session.id}), test_ctx())
        .await
        .unwrap();
    assert_eq!(result["session_id"], session.id);
    assert_eq!(result["turns"].as_array().unwrap().len(), 0);
    assert_eq!(result["cost_usd"], Value::Null);
}

/// `session.send` on an unknown id must map to `session_not_found`.
#[tokio::test]
async fn send_unknown_session_maps_to_session_not_found() {
    let registry = SessionRegistry::new();
    let err = send(
        &registry,
        json!({"session_id": "nope", "input": "hi"}),
        test_ctx(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `session.attach` on an unknown id must map to `session_not_found`.
#[tokio::test]
async fn attach_unknown_session_maps_to_session_not_found() {
    let registry = SessionRegistry::new();
    let err = attach(&registry, json!({"session_id": "nope"}), test_ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `session.detach` on an unknown id must map to `session_not_found`.
#[tokio::test]
async fn detach_unknown_session_maps_to_session_not_found() {
    let registry = SessionRegistry::new();
    let err = detach(&registry, json!({"session_id": "nope"}), test_ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `session.cancel` on an unknown id must map to `session_not_found`.
#[tokio::test]
async fn cancel_unknown_session_maps_to_session_not_found() {
    let registry = SessionRegistry::new();
    let err = cancel(&registry, json!({"session_id": "nope"}), test_ctx())
        .await
        .unwrap_err();
    assert_eq!(err.code, -32007);
}

/// `session.cancel` on a session with an in-flight execution must set the
/// cooperative-cancel flag the executing loop observes (#2056).
///
/// (#8207) It must ALSO wait for that loop to stop before answering — see
/// `cancel_waits_for_the_task_to_stop_before_reporting`, which is where the
/// wait itself is pinned. This one covers only the signal.
#[tokio::test]
async fn cancel_executing_session_requests_cooperative_cancel() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let flag = registry.begin_execution(&session.id).unwrap();
    spawn_flag_watching_run(&registry, &session.id, Arc::clone(&flag));

    cancel(&registry, json!({"session_id": &session.id}), test_ctx())
        .await
        .unwrap();

    assert!(
        flag.load(std::sync::atomic::Ordering::Relaxed),
        "the shared cancel flag must have been set"
    );
}

// ── #8207: cancel reports a stop, not a request ───────────────────────────────

/// Spawn a stand-in for `task::executor::run_and_record`: a run that polls the
/// cooperative-cancel flag and, like the real one, clears its own execution
/// slot as its last act.
///
/// Why: the #8207 wait is a claim about a REAL spawned task's lifetime, so a
/// test that only flips registry fields would prove nothing. This is the
/// smallest double with the two properties the wait depends on — it outlives
/// the cancel request, and it calls `finish_execution` before returning.
/// What: spawns the poller, attaches its `JoinHandle` exactly where
/// `spawn_task_run` does, and returns once the handle is attached.
fn spawn_flag_watching_run(
    registry: &Arc<SessionRegistry>,
    session_id: &str,
    flag: Arc<std::sync::atomic::AtomicBool>,
) {
    let registry_for_task = Arc::clone(registry);
    let id = session_id.to_string();
    let handle = tokio::spawn(async move {
        while !flag.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let _ = registry_for_task.finish(&id, crate::session::SessionStatus::Cancelled);
        registry_for_task.finish_execution(&id);
    });
    registry.attach_execution_handle(session_id, handle);
}

/// `session.cancel` must not answer until the daemon-side task has actually
/// stopped (#8207).
///
/// Why: the owner's transcript showed `[tcode] cancelled` printed while the
/// run kept going. The reply used to be sent the instant the flag was set, so
/// "reported cancelled" and "actually stopped" were two different facts. A
/// test that only checked the reported status would pass against that bug —
/// so this asserts the EXECUTION SLOT is empty by the time cancel returns,
/// which is only true once the spawned task has run to completion.
/// Test: this test.
#[tokio::test]
async fn cancel_waits_for_the_task_to_stop_before_reporting() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let flag = registry.begin_execution(&session.id).unwrap();
    spawn_flag_watching_run(&registry, &session.id, Arc::clone(&flag));

    cancel(&registry, json!({"session_id": &session.id}), test_ctx())
        .await
        .unwrap();

    assert!(
        !registry.is_executing(&session.id),
        "cancel must not return while the daemon-side task is still running"
    );
}

/// A prompt submitted straight after a cancel must be accepted on the SAME
/// session, not rejected with `-32003` (#8207).
///
/// Why: this is the user-visible half of the bug — `session <id> already has a
/// task running` reached the TUI as a raw JSON-RPC error on the very next
/// prompt. The session identity is the whole point: the TUI holds ONE session
/// for the conversation (`tui_client::engine_state::run_chat_turn` reuses the
/// same id for every prompt), so an earlier cut of this test that started a
/// BRAND NEW session proved nothing about the reported bug — the real session
/// landed `Cancelled` and `begin_execution` rejected it as terminal, which is
/// the SECOND way the next prompt failed. Both are closed here: the slot is
/// released, and a cancelled session is resumable.
/// Test: this test.
#[tokio::test]
async fn a_prompt_right_after_cancel_is_accepted_not_rejected() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let flag = registry.begin_execution(&session.id).unwrap();
    spawn_flag_watching_run(&registry, &session.id, Arc::clone(&flag));

    cancel(&registry, json!({"session_id": &session.id}), test_ctx())
        .await
        .unwrap();

    assert!(
        !registry.is_executing(&session.id),
        "the cancelled run must have released its execution slot"
    );
    assert_eq!(
        registry.status(&session.id).unwrap().status.as_str(),
        "cancelled",
        "the run really did land Cancelled — that is the state the next prompt \
         has to be accepted from"
    );
    let started = registry.begin_execution(&session.id);
    assert!(
        started.is_ok(),
        "a prompt on the SAME session right after cancel must start, got {:?}",
        started.err()
    );
    assert_eq!(
        registry.status(&session.id).unwrap().status.as_str(),
        "running",
        "resuming must publish the same Running transition every other resume does"
    );
}

/// A cancel the daemon cannot confirm must be an ERROR, never a cancelled
/// snapshot (#8207, fail-open check).
///
/// Why: the whole point of the wait is that the client stops being told a
/// comfortable lie. Downgrading an unconfirmed stop back to "cancelled" would
/// reintroduce the bug with extra latency. The execution-tracked-but-not-yet-
/// joinable window is the cheapest deterministic way to reach that arm.
/// What: begin an execution and never attach a handle, so the stop cannot be
/// confirmed; assert `cancel` errors and that the session is NOT reported
/// cancelled.
/// Test: this test.
#[tokio::test]
async fn cancel_that_cannot_confirm_the_stop_is_an_error() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let _flag = registry.begin_execution(&session.id).unwrap();

    let err = cancel(&registry, json!({"session_id": &session.id}), test_ctx())
        .await
        .expect_err("an unconfirmable stop must not report success");

    assert_eq!(err.code, -32010, "unexpected error shape: {err:?}");
    assert_eq!(
        err.data,
        Some(json!({"error_type": "cancel_unconfirmed"})),
        "an unconfirmed cancel must be distinguishable from a daemon fault"
    );
    let status = registry.status(&session.id).unwrap();
    assert_ne!(
        status.status.as_str(),
        "cancelled",
        "a cancel that could not be confirmed must not claim the session stopped"
    );
}

/// The daemon's cancel grace and EVERY in-repo client's per-call budget are ONE
/// contract (#8207).
///
/// Why: the first cut paired a thirty-second daemon grace with the TUI's
/// fifteen-second `DEFAULT_CALL_TIMEOUT`, so every cancel taking 15-30s reached
/// the user as a transport timeout and the `cancel_unconfirmed` reply the grace
/// exists to produce was unreachable from the TUI. Two compile-time assertions
/// beside `CANCEL_CONFIRM_GRACE` block the halves being changed out of order;
/// this test states the same contract where a reader of the cancel tests will
/// find it. The CLI client is the second budget over the same call: pinning only
/// the TUI's left `cli_client::stdio` free to drop below the grace and
/// reintroduce the identical transport timeout for `tcode session cancel`.
/// Test: this test.
#[test]
fn the_cancel_grace_fits_inside_the_clients_call_budget() {
    for (client, budget) in [
        (
            "tui_client::uds_rpc",
            crate::tui_client::uds_rpc::DEFAULT_CALL_TIMEOUT,
        ),
        (
            "cli_client::stdio",
            crate::cli_client::stdio::DEFAULT_CALL_TIMEOUT,
        ),
    ] {
        assert!(
            CANCEL_CONFIRM_GRACE < budget,
            "{client}: the daemon's answer must arrive inside the client's call \
             budget: grace {CANCEL_CONFIRM_GRACE:?} vs budget {budget:?}"
        );
        assert!(
            CANCEL_CONFIRM_GRACE + CANCEL_ANSWER_HEADROOM <= budget,
            "{client}: the grace must leave headroom for the reply itself: \
             grace {CANCEL_CONFIRM_GRACE:?} + headroom {CANCEL_ANSWER_HEADROOM:?} \
             vs budget {budget:?}"
        );
    }
}
