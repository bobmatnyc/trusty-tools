//! In-crate tests for the local session-manager PROXY routes (TELUI-6, #1440).
//!
//! Why: mirrors the `client::proxy::tests` suite but drives the handlers
//! directly with axum's `State`/`Json`/`Path` extractors against a hermetic
//! `DaemonState::with_root_isolated_managed` — the SAME convention
//! `tests/session_manager_mvp.rs` uses for `managed_routes` handler tests, kept
//! in-crate (rather than appended to that already-large integration file) so
//! this cohesive cluster has its own home. `tests/proxy_routes.rs` covers the
//! same surface over REAL HTTP (the curl-facing contract).
//! What: seeds one managed session via `create_with_id` (no real tmux — the
//! isolated-managed constructor wires a `FakeNoopTmuxDriver`), then drives
//! focus → message → summary → unfocus, plus the dead-session auto-unfocus and
//! no-focus paths, asserting on the decoded JSON body.
//! Test: this *is* the test module.

use super::*;
use std::sync::Arc;

use crate::daemon::state::DaemonState;
use crate::runtime::RuntimeKind;
use crate::session_manager::record::ManagedSessionId;

/// Decode an axum `impl IntoResponse` into its JSON body.
///
/// Why: every handler below returns `impl IntoResponse`; a test must read the
/// body to assert on the tagged `outcome`. Mirrors the identically-named helper
/// in `tests/session_manager_mvp.rs` (a separate compilation unit, so it cannot
/// be imported — duplicated here at ~8 lines).
async fn decode_response(resp: impl axum::response::IntoResponse) -> serde_json::Value {
    let resp = resp.into_response();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// Build a hermetic `DaemonState` with one seeded managed session.
///
/// Why: every proxy test needs a resolvable session to focus; centralising the
/// seed keeps each test focused on the proxy behavior, not setup.
/// What: returns the state and the seeded session's friendly name (`tmux_name`),
/// which the tests resolve by name (proving the fuzzy-name path, not just id).
async fn seeded_state() -> (Arc<DaemonState>, String) {
    let root = tempfile::tempdir().unwrap().keep();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let mgr = state.session_manager().await;
    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "proxy test task".to_string(),
            None,
            Some("proxytest".to_string()),
            None,
            None,
            None,
            RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session");
    (state, record.tmux_name)
}

#[tokio::test]
async fn proxy_focus_route_resolves_by_name_and_reports_current() {
    let (state, name) = seeded_state().await;

    // Set the focus by friendly name.
    let body = decode_response(
        proxy_focus(
            axum::extract::State(Arc::clone(&state)),
            axum::Json(ProxyFocusRequest {
                conversation_key: "conv-1".into(),
                session_id: name.clone(),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(body["outcome"], "focused");
    assert_eq!(body["name"], name);

    // An empty session_id queries the current focus without changing it.
    let body = decode_response(
        proxy_focus(
            axum::extract::State(Arc::clone(&state)),
            axum::Json(ProxyFocusRequest {
                conversation_key: "conv-1".into(),
                session_id: String::new(),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(body["outcome"], "current");
    assert_eq!(body["target"]["name"], name);
}

#[tokio::test]
async fn proxy_get_focus_route_reports_unset() {
    let (state, _name) = seeded_state().await;
    let body = decode_response(
        proxy_get_focus(
            axum::extract::State(Arc::clone(&state)),
            axum::extract::Path("conv-never-focused".to_string()),
        )
        .await,
    )
    .await;
    assert_eq!(body["outcome"], "current");
    assert!(body["target"].is_null());
}

#[tokio::test]
async fn proxy_focus_route_unknown_target_is_not_found() {
    let (state, _name) = seeded_state().await;
    let body = decode_response(
        proxy_focus(
            axum::extract::State(Arc::clone(&state)),
            axum::Json(ProxyFocusRequest {
                conversation_key: "conv-1".into(),
                session_id: "no-such-session".into(),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(body["outcome"], "not_found");
    assert_eq!(body["target"], "no-such-session");
}

#[tokio::test]
async fn proxy_message_route_focused_sends_unfocused_no_focus() {
    let (state, name) = seeded_state().await;

    // Unfocused: the message route reports `no_focus`, never an HTTP error, so
    // a caller can fall back to its own coordinator.
    let body = decode_response(
        proxy_message(
            axum::extract::State(Arc::clone(&state)),
            axum::Json(ProxyMessageRequest {
                conversation_key: "conv-2".into(),
                text: "hello?".into(),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(body["outcome"], "no_focus");

    // Focus, then the SAME message route sends to the focused session.
    decode_response(
        proxy_focus(
            axum::extract::State(Arc::clone(&state)),
            axum::Json(ProxyFocusRequest {
                conversation_key: "conv-2".into(),
                session_id: name.clone(),
            }),
        )
        .await,
    )
    .await;
    let body = decode_response(
        proxy_message(
            axum::extract::State(Arc::clone(&state)),
            axum::Json(ProxyMessageRequest {
                conversation_key: "conv-2".into(),
                text: "run the tests".into(),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(body["outcome"], "sent");
    assert_eq!(body["target"]["name"], name);
    assert_eq!(body["text"], "run the tests");
}

#[tokio::test]
async fn proxy_summary_route_returns_digest_for_focused_session() {
    let (state, name) = seeded_state().await;
    decode_response(
        proxy_focus(
            axum::extract::State(Arc::clone(&state)),
            axum::Json(ProxyFocusRequest {
                conversation_key: "conv-3".into(),
                session_id: name.clone(),
            }),
        )
        .await,
    )
    .await;
    let body = decode_response(
        proxy_summary(
            axum::extract::State(Arc::clone(&state)),
            axum::extract::Path("conv-3".to_string()),
        )
        .await,
    )
    .await;
    assert_eq!(body["outcome"], "summary");
    assert_eq!(body["target"]["name"], name);
    assert!(body["state"].is_string());
    assert!(body["summary"].as_str().unwrap().contains("proxy test task"));
}

#[tokio::test]
async fn proxy_summary_route_no_focus() {
    let (state, _name) = seeded_state().await;
    let body = decode_response(
        proxy_summary(
            axum::extract::State(Arc::clone(&state)),
            axum::extract::Path("conv-never".to_string()),
        )
        .await,
    )
    .await;
    assert_eq!(body["outcome"], "no_focus");
}

#[tokio::test]
async fn proxy_unfocus_route_clears_and_reports_none_when_absent() {
    let (state, name) = seeded_state().await;
    decode_response(
        proxy_focus(
            axum::extract::State(Arc::clone(&state)),
            axum::Json(ProxyFocusRequest {
                conversation_key: "conv-4".into(),
                session_id: name.clone(),
            }),
        )
        .await,
    )
    .await;

    let body = decode_response(
        proxy_unfocus(
            axum::extract::State(Arc::clone(&state)),
            axum::Json(ProxyUnfocusRequest {
                conversation_key: "conv-4".into(),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(body["cleared"]["name"], name);

    // Unfocusing again reports `cleared: null` — a harmless no-op.
    let body = decode_response(
        proxy_unfocus(
            axum::extract::State(Arc::clone(&state)),
            axum::Json(ProxyUnfocusRequest {
                conversation_key: "conv-4".into(),
            }),
        )
        .await,
    )
    .await;
    assert!(body["cleared"].is_null());
}

#[tokio::test]
async fn proxy_message_route_auto_unfocuses_when_session_decommissioned() {
    // Focus a real session, HARD-DELETE its record out from under the focus
    // (decommission alone leaves a tombstone the resolver still finds — a
    // deliberately different, non-"not found" failure mode; a hard delete is
    // what genuinely makes `resolve_target` report "not found"), then confirm
    // the message route auto-unfocuses (the same guarantee the Telegram
    // binding relies on) rather than failing forever.
    let (state, name) = seeded_state().await;
    decode_response(
        proxy_focus(
            axum::extract::State(Arc::clone(&state)),
            axum::Json(ProxyFocusRequest {
                conversation_key: "conv-5".into(),
                session_id: name.clone(),
            }),
        )
        .await,
    )
    .await;

    let mgr = state.session_manager().await;
    let records = mgr.list().await;
    let id = records
        .iter()
        .find(|r| r.tmux_name == name)
        .expect("seeded session present")
        .id;
    mgr.delete_record(&id, true)
        .await
        .expect("hard-delete seeded session");

    let body = decode_response(
        proxy_message(
            axum::extract::State(Arc::clone(&state)),
            axum::Json(ProxyMessageRequest {
                conversation_key: "conv-5".into(),
                text: "are you still there?".into(),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(body["outcome"], "auto_unfocused");

    // The conversation is now unfocused — a follow-up message reports no_focus.
    let body = decode_response(
        proxy_message(
            axum::extract::State(Arc::clone(&state)),
            axum::Json(ProxyMessageRequest {
                conversation_key: "conv-5".into(),
                text: "hello again".into(),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(body["outcome"], "no_focus");
}

#[test]
fn parse_managed_id_rejects_non_uuid() {
    let err = super::backend::parse_managed_id("not-a-uuid").unwrap_err();
    assert!(err.contains("invalid managed session id"));
    assert!(
        !err.contains("not found"),
        "an invalid id must not trip the auto-unfocus 'not found' predicate: {err}"
    );
}
