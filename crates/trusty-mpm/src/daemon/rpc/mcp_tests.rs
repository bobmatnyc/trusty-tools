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
