//! Socket-vs-HTTP parity for the #6288 client-cutover methods.
//!
//! Why: a socket method that answers differently from its HTTP route turns
//! the cutover into a behaviour change the client cannot see. Each test
//! drives the REAL daemon HTTP router and the REAL method on the same state,
//! and requires the same body, or the same refusal text and its projected code.
//! Test: this file.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request};
use chrono::{TimeZone, Utc};
use serde_json::{Value, json};
use tower::ServiceExt;
use trusty_common::uds::server::{RpcResponse, RpcRouter};

use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
use crate::daemon::error::rpc_code_for_status as status_to_rpc_code;
use crate::daemon::state::DaemonState;

async fn isolated_state() -> (Arc<DaemonState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = DaemonState::with_root_isolated_managed(dir.path().to_path_buf()).await;
    (Arc::new(state), dir)
}

/// Status, text and JSON of one call through the real daemon HTTP router.
async fn http(
    state: &Arc<DaemonState>,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (u16, String) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string())),
        None => builder.body(Body::empty()),
    }
    .expect("build request");
    let router = crate::daemon::api::router(Arc::clone(state)).layer(MockConnectInfo(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
    ));
    let response = router.oneshot(request).await.expect("http call");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// One unary call through the real method.
async fn rpc(state: &Arc<DaemonState>, method: &str, params: Value) -> RpcResponse {
    let router = super::register(RpcRouter::new(), state);
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    router.dispatch(frame.to_string().as_bytes()).await
}

/// The parity rule `rpc::managed_tests::assert_parity` states, for these routes.
fn assert_parity(label: &str, (status, text): &(u16, String), rpc: &RpcResponse) {
    if (200..300).contains(status) {
        let expected: Value = serde_json::from_str(text).expect("a 2xx body is JSON");
        assert_eq!(
            rpc.result.as_ref(),
            Some(&expected),
            "{label}: bodies differ"
        );
        return;
    }
    let error = rpc
        .error
        .as_ref()
        .unwrap_or_else(|| panic!("{label}: rpc succeeded where HTTP answered {status}"));
    assert_eq!(&error.message, text, "{label}: refusal text differs");
    assert_eq!(
        error.code,
        status_to_rpc_code(*status),
        "{label}: code differs"
    );
}

/// `mpm.channels.egress_verdict` answers what the HTTP route answers — here
/// the `home_mismatch` refusal a hook under a foreign home receives.
#[tokio::test]
async fn egress_verdict_parity() {
    let (state, _dir) = isolated_state().await;
    let body = json!({"home": "/nonexistent/tm-6288-home"});
    let over_http = http(
        &state,
        Method::POST,
        crate::core::egress_verdict::VERDICT_ROUTE,
        Some(body.clone()),
    )
    .await;
    let over_rpc = rpc(&state, "mpm.channels.egress_verdict", body).await;
    assert_parity("mpm.channels.egress_verdict", &over_http, &over_rpc);
}

/// `mpm.managed.files` returns the same entries the HTTP route returns.
#[tokio::test]
async fn managed_files_parity() {
    let (state, _dir) = isolated_state().await;
    let id = SessionId::new();
    let mut session = Session::new(id, "/tmp/tm-6288", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    state.register_session(session);
    for (path, at) in [("src/a.rs", 100), ("src/b.rs", 200)] {
        state.push_hook_event(HookEventRecord {
            session: id,
            event: HookEvent::FileChanged,
            at: Utc.timestamp_opt(at, 0).single().expect("timestamp"),
            payload: json!({"path": path, "operation": "modify"}),
        });
    }
    let uri = format!("/api/v1/sessions/managed/{}/files?since=150", id.0);
    let over_http = http(&state, Method::GET, &uri, None).await;
    assert_eq!(over_http.0, 200, "{}", over_http.1);
    let over_rpc = rpc(
        &state,
        "mpm.managed.files",
        json!({"id": id.0.to_string(), "since": 150}),
    )
    .await;
    assert_parity("mpm.managed.files", &over_http, &over_rpc);
}

/// An unknown session is the same typed not-found on both transports.
#[tokio::test]
async fn managed_files_unknown_session_parity() {
    let (state, _dir) = isolated_state().await;
    let missing = "11111111-2222-3333-4444-555555555555";
    let over_http = http(
        &state,
        Method::GET,
        &format!("/api/v1/sessions/managed/{missing}/files"),
        None,
    )
    .await;
    assert_eq!(over_http.0, 404, "{}", over_http.1);
    let over_rpc = rpc(&state, "mpm.managed.files", json!({"id": missing})).await;
    assert_parity("mpm.managed.files unknown", &over_http, &over_rpc);
}
