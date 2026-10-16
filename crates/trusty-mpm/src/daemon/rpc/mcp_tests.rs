//! Transport-parity tests for `mpm.mcp.dispatch` (#6288 step 2a).
//!
//! Why parity: the method's claim is that the socket answers every MCP
//! envelope exactly as `POST /rpc` does. Each case sends one envelope to the
//! real axum router (as a loopback peer) and the same envelope to the RPC
//! router, both over one hermetic `DaemonState`, and compares the bodies.
//!
//! Test: this file IS the test module for [`super`].

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
use trusty_common::uds::server::{CODE_INVALID_PARAMS, RpcResponse, RpcRouter};

use super::{METHOD, register};
use crate::core::paths::FrameworkPaths;
use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
use crate::daemon::api;
use crate::daemon::state::DaemonState;

/// One daemon state rooted at an empty temp directory.
fn hermetic() -> (Arc<DaemonState>, TempDir) {
    let dir = tempfile::tempdir().expect("temp dir for hermetic DaemonState");
    let paths = FrameworkPaths::under(dir.path());
    (Arc::new(DaemonState::with_paths(&paths)), dir)
}

/// POST `body` to `/rpc` from `peer`, returning the status and decoded body.
async fn http_rpc(state: &Arc<DaemonState>, peer: IpAddr, body: &str) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/rpc")
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .expect("build request");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::new(peer, 50505)));
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

/// Dispatch `params` to [`METHOD`] and return the whole response frame.
async fn rpc_frame(router: &RpcRouter, params: Value) -> Value {
    let frame = json!({ "jsonrpc": "2.0", "id": 1, "method": METHOD, "params": params });
    let response: RpcResponse = router
        .dispatch(&serde_json::to_vec(&frame).expect("encode the request frame"))
        .await;
    serde_json::to_value(response).expect("the response frame must serialise")
}

/// Why: one envelope per dispatch arm — a listing, a tool call that succeeds,
/// a tool call the dispatcher refuses inside the envelope, an unknown method,
/// and a notification — must come back byte-identical on both transports.
/// Test: this function IS the test.
#[tokio::test]
async fn parity_mcp_dispatch_agrees_across_transports() {
    let (state, _dir) = hermetic();
    let router = register(RpcRouter::new(), &state);

    for envelope in [
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 8, "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
               "params": {"name": "session_list", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 10, "method": "tools/call",
               "params": {"name": "session_status", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 11, "method": "tools/call",
               "params": {"name": "no_such_tool", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": "s", "method": "no/such/method"}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    ] {
        let (status, body) = http_rpc(
            &state,
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            &envelope.to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{envelope}: {body}");

        let frame = rpc_frame(&router, envelope.clone()).await;
        assert!(frame.get("error").is_none(), "{envelope}: {frame}");
        assert_eq!(
            body, frame["result"],
            "{envelope} must answer identically over HTTP and the socket"
        );
    }
}

/// Why (the Fail-Open Check): a params object that is not an MCP envelope is
/// the method's one new failure branch. It must refuse with a coded error and
/// run no tool, as HTTP refuses the same body before the handler runs.
/// Test: this function IS the test.
#[tokio::test]
async fn rpc_mcp_dispatch_refuses_a_params_object_that_is_not_an_mcp_envelope() {
    let (state, _dir) = hermetic();
    let router = register(RpcRouter::new(), &state);

    for bad in [
        json!({"jsonrpc": "2.0", "id": 1}),
        json!({"method": 5}),
        json!(null),
        json!("tools/list"),
    ] {
        let (status, _) = http_rpc(&state, IpAddr::V4(Ipv4Addr::LOCALHOST), &bad.to_string()).await;
        assert!(status.is_client_error(), "HTTP refuses {bad}: {status}");

        let frame = rpc_frame(&router, bad.clone()).await;
        assert!(
            frame.get("result").is_none(),
            "{bad} must not answer: {frame}"
        );
        assert_eq!(
            frame["error"]["code"],
            json!(CODE_INVALID_PARAMS),
            "{bad}: {frame}"
        );
    }
}

/// Why: the HTTP route keeps its own loopback gate now that its body is shared;
/// moving the body must not have moved the gate.
/// Test: this function IS the test.
#[tokio::test]
async fn http_rpc_still_refuses_a_non_loopback_peer() {
    let (state, _dir) = hermetic();
    let (status, _) = http_rpc(
        &state,
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50)),
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Call `sessions_chat` with `arguments` through [`METHOD`]; the tool result.
async fn sessions_chat_result(router: &RpcRouter, arguments: Value) -> Value {
    let envelope = json!({"jsonrpc": "2.0", "id": 21, "method": "tools/call",
                          "params": {"name": "sessions_chat", "arguments": arguments}});
    let frame = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        rpc_frame(router, envelope),
    )
    .await
    .expect("#9979: a chat turn must answer, never hang");
    assert!(frame.get("error").is_none(), "{frame}");
    frame["result"]["result"].clone()
}

/// Why (#9979): the tool is listed, and a turn through `mpm.mcp.dispatch`
/// returns the `mpm.sessions.chat` response unchanged. A `@prefix:` turn needs
/// no model, so it runs hermetically; the unique tmux name reaches no pane.
/// Test: this function IS the test.
#[tokio::test]
async fn mcp_dispatch_sessions_chat_matches_mpm_sessions_chat() {
    let (state, _dir) = hermetic();
    let router = register(RpcRouter::new(), &state);
    let listing = rpc_frame(
        &router,
        json!({"jsonrpc": "2.0", "id": 20, "method": "tools/list"}),
    )
    .await;
    let tools = listing["result"]["result"]["tools"]
        .as_array()
        .expect("a tool list");
    assert!(
        tools.iter().any(|t| t["name"] == "sessions_chat"),
        "{listing}"
    );

    let name = format!("tm-mcpchat-{}", uuid::Uuid::new_v4().simple());
    let mut session = Session::new(SessionId::new(), "/tmp/p", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    session.tmux_name = name.clone();
    state.register_session(session);
    let turn = json!({ "message": format!("@{}: echo hi", name.trim_start_matches("tm-")) });

    let result = sessions_chat_result(&router, turn.clone()).await;
    assert_eq!(result["isError"], false, "{result}");
    let text = result["content"][0]["text"].as_str().expect("text");
    let reply: Value = serde_json::from_str(text).expect("the tool text is JSON");
    assert_eq!(reply["routed_to_session"], json!(name), "{reply}");

    let chat = super::super::coordinator::register(RpcRouter::new(), &state);
    let frame = json!({ "jsonrpc": "2.0", "id": 1, "method": "mpm.sessions.chat", "params": turn });
    let response: RpcResponse = chat
        .dispatch(&serde_json::to_vec(&frame).expect("encode"))
        .await;
    let direct = serde_json::to_value(response).expect("serialise the frame");
    assert_eq!(
        reply, direct["result"],
        "the tool must not reshape the reply"
    );
}

/// Why (#9979, the Fail-Open Check): with no model configured the turn is a
/// tool error carrying the `503` status text, answered at once.
/// Test: this function IS the test.
#[tokio::test]
async fn mcp_dispatch_sessions_chat_without_a_model_is_a_503_tool_error() {
    let (state, _dir) = hermetic();
    let router = register(RpcRouter::new(), &state);
    let result = sessions_chat_result(&router, json!({ "message": "what is running?" })).await;
    assert_eq!(result["isError"], true, "{result}");
    let text = result["content"][0]["text"].as_str().expect("text");
    assert!(text.starts_with("sessions_chat failed (503): "), "{text}");
    assert!(text.contains("not configured"), "{text}");
}
