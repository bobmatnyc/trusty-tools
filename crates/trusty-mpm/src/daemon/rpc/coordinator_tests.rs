//! Transport-parity tests for the coordinator methods (#6288 step 2a).
//!
//! Test: this file IS the test module for [`super`].

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use trusty_common::uds::server::{CODE_INVALID_PARAMS, RpcResponse, RpcRouter};

use super::{METHODS, register};
use crate::core::paths::FrameworkPaths;
use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
use crate::daemon::api;
use crate::daemon::error::CODE_UNAVAILABLE;
use crate::daemon::state::DaemonState;

/// One daemon state rooted at an empty temp directory: no overseer, no SM
/// runtime, so a free-text chat turn takes the 503 leg on both transports.
fn hermetic() -> (Arc<DaemonState>, TempDir) {
    let dir = tempfile::tempdir().expect("temp dir for hermetic DaemonState");
    let paths = FrameworkPaths::under(dir.path());
    (Arc::new(DaemonState::with_paths(&paths)), dir)
}

/// Drive one request through the real HTTP router.
async fn http(
    state: &Arc<DaemonState>,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(v) => request
            .header("content-type", "application/json")
            .body(Body::from(v.to_string())),
        None => request.body(Body::empty()),
    }
    .expect("build request");
    let response = api::router(Arc::clone(state))
        .oneshot(request)
        .await
        .expect("the router must answer");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the response body");
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, value)
}

/// Dispatch one call and return the whole response frame.
async fn rpc(state: &Arc<DaemonState>, method: &str, params: Value) -> Value {
    let router = register(RpcRouter::new(), state);
    let frame = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let response: RpcResponse = router
        .dispatch(&serde_json::to_vec(&frame).expect("encode"))
        .await;
    serde_json::to_value(response).expect("serialise the frame")
}

/// Test: this function IS the test.
#[test]
fn coordinator_methods_are_registered() {
    let (state, _dir) = hermetic();
    let router = register(RpcRouter::new(), &state);
    let names: Vec<&str> = router.method_names().collect();
    assert_eq!(names, METHODS);
}

/// Why: both HTTP aliases and the socket must return the same snapshot for the
/// same state. `generated_at` is stamped per call, so it is excused.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_context_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let mut session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
    session.status = SessionStatus::Stopped;
    session.tmux_name = "tm-parity-context".to_string();
    state.register_session(session);

    let mut result = rpc(&state, "mpm.sessions.context", json!({})).await["result"].clone();
    result
        .as_object_mut()
        .expect("object")
        .remove("generated_at");
    assert_eq!(result["sessions"][0]["name"], json!("tm-parity-context"));
    for uri in [
        "/api/v1/sessions/context",
        "/api/v1/session-manager/context",
    ] {
        let (status, mut body) = http(&state, "GET", uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        body.as_object_mut().expect("object").remove("generated_at");
        assert_eq!(body, result, "{uri} must match mpm.sessions.context");
    }
}

/// Why: a `@prefix:` turn is the chat leg that needs no model, so it is the
/// success case both transports can run hermetically. The tmux name is unique
/// so the send reaches no real pane.
///
/// Why the unroutable socket (#11718, the #10893 mechanism): with no socket
/// the send reaches `TmuxDriver::discover`, which reads the live `$HOME` gate
/// per call, so a concurrent test moving `$HOME` gave one transport "tmux
/// unavailable" and the other a `send-keys` failure. A socket outside this
/// process's socket directory is refused by `server_of_socket` first, so both
/// calls answer from fixed inputs whether or not the host has tmux.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_sessions_chat_agrees_across_transports() {
    let (state, dir) = hermetic();
    let name = format!("tm-parity-{}", uuid::Uuid::new_v4().simple());
    let mut session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    session.tmux_name = name.clone();
    // See #11718: refused before `discover`, so no env read can differ between calls.
    session.tmux_socket = Some(dir.path().join("tm-ws-parity-11718"));
    state.register_session(session);
    let prefix = name.trim_start_matches("tm-");
    let turn = json!({ "message": format!("@{prefix}: echo hi") });

    let (status, body) = http(&state, "POST", "/api/v1/sessions/chat", Some(turn.clone())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["routed_to_session"], json!(name), "{body}");
    // See #11718: pin the branch taken; a route that reaches `discover` fails here.
    let reply = body["reply"].as_str().unwrap_or_default();
    assert!(reply.contains("cannot be reached"), "{body}");
    let frame = rpc(&state, "mpm.sessions.chat", turn).await;
    assert_eq!(body, frame["result"], "chat must match across transports");
}

/// Why: with no model configured HTTP answers 503; the socket must refuse with
/// the matching code and the same message rather than answer empty.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_sessions_chat_without_a_model_is_unavailable_like_http() {
    let (state, _dir) = hermetic();
    let turn = json!({ "message": "what is happening?" });
    let (status, body) = http(&state, "POST", "/api/v1/sessions/chat", Some(turn.clone())).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    let frame = rpc(&state, "mpm.sessions.chat", turn).await;
    assert!(frame.get("result").is_none(), "{frame}");
    assert_eq!(frame["error"]["code"], json!(CODE_UNAVAILABLE), "{frame}");
    assert_eq!(frame["error"]["message"], body["error"]);
}

/// Why (the Fail-Open Check): a turn with no `message` is refused on both
/// transports, never answered as an empty turn.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_sessions_chat_rejects_a_turn_without_a_message() {
    let (state, _dir) = hermetic();
    let (status, _) = http(&state, "POST", "/api/v1/sessions/chat", Some(json!({}))).await;
    assert!(status.is_client_error(), "{status}");
    let frame = rpc(&state, "mpm.sessions.chat", json!({ "history": [] })).await;
    assert_eq!(
        frame["error"]["code"],
        json!(CODE_INVALID_PARAMS),
        "{frame}"
    );
}

/// Why: the origin guard stays on HTTP now that the body is shared.
/// Test: this function IS the test.
#[tokio::test]
async fn http_chat_still_refuses_a_cross_origin_page() {
    let (state, _dir) = hermetic();
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/sessions/chat")
        .header("content-type", "application/json")
        .header("origin", "https://evil.example")
        .body(Body::from(json!({ "message": "hi" }).to_string()))
        .expect("build request");
    let response = api::router(state).oneshot(request).await.expect("answer");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
