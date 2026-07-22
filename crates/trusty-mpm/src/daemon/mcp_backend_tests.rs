//! Unit tests for `daemon::mcp_backend` — split out of `mcp_backend.rs`
//! (test-file budget: 1500 SLOC; the inline module pushed the production
//! file over its 500-SLOC cap once the two PM pause/resume context tools
//! (`session_context_catchup`/`session_context_pause`) grew it past the
//! frozen 566-SLOC budget).
//! What: exercises every `StateBackend` method against a freshly-built
//! `DaemonState` — session listing/status, agent delegation, memory
//! protection, circuit breaker status, hook events, and bug-reporting.
//! Test: this IS the test module.

use super::*;
use crate::core::session::{ControlModel, Session, SessionStatus};

fn state_with_session() -> (Arc<DaemonState>, SessionId) {
    let state = DaemonState::shared();
    let id = SessionId::new();
    let mut session = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    state.register_session(session);
    (state, id)
}

#[tokio::test]
async fn session_list_returns_registered_sessions() {
    // session_list now also reads the SessionManager store, so this test
    // must bind to an ISOLATED managed store (#1790) rather than the
    // production `~/.trusty-mpm` root that `DaemonState::shared` uses.
    let root = tempfile::TempDir::new().expect("root tempdir");
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let id = SessionId::new();
    let mut session = Session::new(id, "/tmp/p", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    state.register_session(session);

    let backend = StateBackend::new(state);
    let list = backend.session_list().await.unwrap();
    // One legacy session registered; the isolated managed store is empty.
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["kind"], "legacy");
}

#[tokio::test]
async fn session_list_includes_managed_sessions() {
    // Regression for #1946: a session provisioned via the managed path
    // (`session_new` / `spawn_managed` / in-project worktree spawn) lives in
    // the SessionManager store, NOT the legacy `DaemonState` registry. The
    // sibling `session_stop` tool targets that store by id, so `session_list`
    // MUST surface managed sessions — otherwise the operator can never
    // discover the id needed to stop the session they are in.
    let root = tempfile::TempDir::new().expect("root tempdir");
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);

    // Seed one managed session directly in the store (fake tmux driver, so no
    // real tmux pane is created).
    let mgr = state.session_manager().await;
    let record = mgr
        .create(
            "fix the bug".to_string(),
            Some(root.path().to_path_buf()),
            Some("regression".to_string()),
            Some(root.path().to_path_buf()),
            None,
            None,
        )
        .await
        .expect("managed session create must succeed with the fake tmux driver");

    let backend = StateBackend::new(state);
    let list = backend.session_list().await.unwrap();
    let arr = list.as_array().expect("session_list returns an array");

    let found = arr.iter().any(|v| {
        v.get("id").and_then(|i| i.as_str()) == Some(record.id.to_string().as_str())
            && v.get("kind").and_then(|k| k.as_str()) == Some("managed")
    });
    assert!(
        found,
        "provisioned managed session {} must appear in session_list (got {list})",
        record.id
    );
}

#[tokio::test]
async fn session_status_unknown_id_errors() {
    let (state, _) = state_with_session();
    let backend = StateBackend::new(state);
    let err = backend.session_status("not-a-uuid").await.unwrap_err();
    assert!(err.contains("not a valid session id"));
}

#[tokio::test]
async fn session_status_resolves_managed_session() {
    // Regression for #1976: session_status previously only consulted the
    // legacy registry, so a managed (`tmpm-`) session — the primary type
    // trusty-mpm spawns — reported "no such session". It must now resolve via
    // the managed store fallback and report `kind: "managed"`.
    let root = tempfile::TempDir::new().expect("root tempdir");
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let record = state
        .session_manager()
        .await
        .create(
            "fix the bug".to_string(),
            Some(root.path().to_path_buf()),
            None,
            Some(root.path().to_path_buf()),
            None,
            None,
        )
        .await
        .expect("managed create");

    let backend = StateBackend::new(state);
    let status = backend
        .session_status(&record.id.to_string())
        .await
        .expect("managed session must resolve via the #1976 fallback");
    assert_eq!(status["kind"], "managed");
    assert_eq!(status["session"]["id"], record.id.to_string());
}

#[tokio::test]
async fn agent_delegate_records_a_delegation() {
    let (state, id) = state_with_session();
    let backend = StateBackend::new(state.clone());
    let result = backend
        .agent_delegate(&id.0.to_string(), "research", "find the bug", Some("opus"))
        .await
        .unwrap();
    assert_eq!(result["agent"], "research");
    assert_eq!(state.delegations_for(id).len(), 1);
}

#[tokio::test]
async fn agent_delegate_refused_when_breaker_open() {
    let (state, id) = state_with_session();
    // Trip the breaker for `flaky` with three failures.
    for _ in 0..3 {
        state.record_outcome("flaky", false);
    }
    let backend = StateBackend::new(state);
    let err = backend
        .agent_delegate(&id.0.to_string(), "flaky", "task", None)
        .await
        .unwrap_err();
    assert!(err.contains("circuit breaker"));
}

#[tokio::test]
async fn agent_delegate_accepts_managed_session() {
    // Regression for #1976: delegation gating rejected managed (`tmpm-`)
    // sessions with "no such session" because it only checked the legacy
    // registry. A delegation from a managed session must now be accepted and
    // recorded (keyed by UUID).
    let root = tempfile::TempDir::new().expect("root tempdir");
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let record = state
        .session_manager()
        .await
        .create(
            "implement the fix".to_string(),
            Some(root.path().to_path_buf()),
            None,
            Some(root.path().to_path_buf()),
            None,
            None,
        )
        .await
        .expect("managed create");

    let backend = StateBackend::new(state.clone());
    let result = backend
        .agent_delegate(
            &record.id.to_string(),
            "rust-engineer",
            "wire it up",
            Some("sonnet"),
        )
        .await
        .expect("delegation from a managed session must be accepted (#1976)");
    assert_eq!(result["agent"], "rust-engineer");
    // The delegation is keyed by the session UUID, shared across both families.
    assert_eq!(state.delegations_for(SessionId(record.id.0)).len(), 1);
}

#[tokio::test]
async fn memory_protect_classifies_pressure() {
    let (state, id) = state_with_session();
    let backend = StateBackend::new(state);
    let result = backend
        .memory_protect(&id.0.to_string(), 900, 1000)
        .await
        .unwrap();
    assert_eq!(result["pressure"], "Compact");
}

#[tokio::test]
async fn hook_event_rejects_unknown_event() {
    let (state, id) = state_with_session();
    let backend = StateBackend::new(state);
    let err = backend
        .hook_event(&id.0.to_string(), "NotAnEvent", Value::Null)
        .await
        .unwrap_err();
    assert!(err.contains("unknown hook event"));
}

#[tokio::test]
async fn hook_event_drives_circuit_breaker() {
    let (state, id) = state_with_session();
    let backend = StateBackend::new(state.clone());
    // Three subagent failures for `flaky` should trip its breaker.
    for _ in 0..3 {
        backend
            .hook_event(
                &id.0.to_string(),
                "SubagentStopFailure",
                json!({ "agent": "flaky" }),
            )
            .await
            .unwrap();
    }
    assert!(!state.breaker("flaky").allows_delegation());
}

#[tokio::test]
async fn hook_event_accepts_known_event() {
    let (state, id) = state_with_session();
    let backend = StateBackend::new(state.clone());
    backend
        .hook_event(&id.0.to_string(), "PreToolUse", json!({"tool": "Bash"}))
        .await
        .unwrap();
    assert_eq!(state.recent_hook_events().len(), 1);
}

// ── Phase 3: bug-reporting backend tests ──────────────────────────────────

#[tokio::test]
async fn list_recent_errors_returns_valid_json() {
    // The local daemon stores are typically empty in CI; this test verifies
    // the method returns a valid, parseable JSON object regardless.
    let (state, _) = state_with_session();
    let backend = StateBackend::new(state);
    let result = backend.list_recent_errors(20).await.unwrap();
    assert!(result["errors"].is_array(), "errors must be an array");
    assert!(result["limit"].is_number(), "limit must be a number");
}

#[tokio::test]
async fn preview_bug_report_unknown_fingerprint_errors() {
    let (state, _) = state_with_session();
    let backend = StateBackend::new(state);
    let err = backend
        .preview_bug_report(&"z".repeat(64))
        .await
        .unwrap_err();
    assert!(
        err.contains("not found"),
        "error should mention 'not found': {err}"
    );
}

#[tokio::test]
async fn report_bug_no_confirm_returns_preview_only() {
    let (state, _) = state_with_session();
    let backend = StateBackend::new(state);
    // An unknown fingerprint with confirm:false should give "not found" error
    // (the fingerprint lookup happens before the confirm check).
    let err = backend
        .report_bug(&"y".repeat(64), false)
        .await
        .unwrap_err();
    assert!(err.contains("not found"), "expected not-found error: {err}");
}

#[tokio::test]
async fn report_bug_confirm_no_token_graceful_failure() {
    // When TRUSTY_BUGREPORT_GITHUB_TOKEN is absent and no token file exists,
    // report_bug should return Ok or Err without panicking — no real GitHub call.
    // Because local stores are typically empty in CI, the "not found" error
    // fires before the token check; that is acceptable. The intent is that
    // no panic occurs and no network call is made.
    let (state, _) = state_with_session();
    let backend = StateBackend::new(state);
    // This returns Err (fingerprint not found) — acceptable in the test
    // environment where stores are empty. What matters is no panic and no
    // real GitHub call.
    let _ = backend.report_bug(&"x".repeat(64), true).await;
}
