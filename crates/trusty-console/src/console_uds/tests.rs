//! Tests for the console's own socket (#10902).
//!
//! Why: the router contract, the cache-only fleet answer, its equality with
//! the HTTP route, and the hardened bind over a real socket.
//! What: router-level dispatch tests, then tests that bind in a temp dir and
//! dial with `trusty_common::uds::send_framed_request`.
//! Test: this module IS the suite for `console_uds/mod.rs`.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use trusty_common::console_rpc::{
    ConsoleFleetStatus, ConsoleHealth, METHOD_FLEET_STATUS, METHOD_HEALTH, METHODS,
};
use trusty_common::uds::server::{CODE_METHOD_NOT_FOUND, RpcResponse};
use trusty_common::uds::{UdsSecurityError, send_framed_request};

use super::*;
use crate::connector::{ServiceConnector, ServiceInfo, ServiceStatus};

const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// A connector that returns a fixed row and counts its detections.
struct StubConnector {
    id: &'static str,
    status: ServiceStatus,
    detections: Arc<AtomicUsize>,
}

impl ServiceConnector for StubConnector {
    fn id(&self) -> &'static str {
        self.id
    }
    fn display_name(&self) -> &'static str {
        self.id
    }
    fn detect(&self) -> ServiceInfo {
        self.detections.fetch_add(1, Ordering::SeqCst);
        ServiceInfo {
            id: self.id.to_string(),
            display_name: self.id.to_string(),
            status: self.status.clone(),
            version: None,
            url: None,
            hint: None,
            lifecycle: self.lifecycle(),
            cpu_pct: None,
            rss_bytes: None,
        }
    }
}

/// A state over two stubs (absent listed first, so ordering is exercised),
/// plus the shared detection counter.
fn state() -> (AppState, Arc<AtomicUsize>) {
    let detections = Arc::new(AtomicUsize::new(0));
    let stub = |id, status| -> Box<dyn ServiceConnector> {
        Box::new(StubConnector {
            id,
            status,
            detections: Arc::clone(&detections),
        })
    };
    let state = AppState::new(vec![
        stub("stub-absent", ServiceStatus::Absent),
        stub("stub-running", ServiceStatus::Running),
    ]);
    (state, detections)
}

async fn dispatch(router: &RpcRouter, method: &str) -> RpcResponse {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "method": method}).to_string();
    router.dispatch(frame.as_bytes()).await
}

/// Why: both methods must answer through the router with their typed shapes.
/// What: dispatches each method with no params and decodes the result.
/// Test: this test itself.
#[tokio::test]
async fn router_answers_both_methods() {
    let (state, _) = state();
    let router = router(state);

    let health = dispatch(&router, METHOD_HEALTH).await;
    let health: ConsoleHealth =
        serde_json::from_value(health.result.expect("health result")).expect("decode health");
    assert_eq!(health.status, "ok");
    assert_eq!(health.service, "trusty-console");
    assert_eq!(health.version, env!("CARGO_PKG_VERSION"));

    let fleet = dispatch(&router, METHOD_FLEET_STATUS).await;
    let fleet: ConsoleFleetStatus =
        serde_json::from_value(fleet.result.expect("fleet result")).expect("decode fleet");
    assert!(!fleet.ready, "no poll has run, so the cache is cold");
}

/// Why: a rename must fail here, not surface as `method_not_found` in the TUI.
/// What: the router's names equal `console_rpc::METHODS`, and only health is
/// liveness.
/// Test: this test itself.
#[test]
fn router_registers_exactly_the_documented_methods() {
    let (state, _) = state();
    let router = router(state);
    let mut expected: Vec<&str> = METHODS.to_vec();
    expected.sort_unstable();
    assert_eq!(router.method_names().collect::<Vec<_>>(), expected);
    assert_eq!(
        router.liveness_names().collect::<Vec<_>>(),
        vec![METHOD_HEALTH]
    );
}

/// Why: an unknown name must come back as a JSON-RPC error, never a hang or a
/// dropped connection.
/// What: dispatches `console.nope` and checks the error code.
/// Test: this test itself.
#[tokio::test]
async fn unknown_method_returns_an_rpc_error() {
    let (state, _) = state();
    let response = dispatch(&router(state), "console.nope").await;
    let error = response.error.expect("an unknown method is an RPC error");
    assert_eq!(error.code, CODE_METHOD_NOT_FOUND);
    assert!(response.result.is_none());
}

/// Why: the UDS call serves the cache; it must never run a detection, which
/// the HTTP route does on a cold cache.
/// What: calls `fleet_status` on a cold cache and checks no connector ran.
/// Test: this test itself.
#[tokio::test]
async fn fleet_status_on_a_cold_cache_answers_not_ready_without_probing() {
    let (state, detections) = state();
    let fleet = fleet_status(&state).await;
    assert!(!fleet.ready);
    assert!(fleet.services.is_empty());
    assert_eq!(detections.load(Ordering::SeqCst), 0, "no fan-out per call");
}

/// Why: `console.fleet_status` must be exactly what `GET /api/console/services`
/// serves for the same cache state, with no extra detection per call.
/// What: fills the cache with one poll, then compares the UDS rows with the
/// HTTP body as JSON values.
/// Test: this test itself.
#[tokio::test]
async fn fleet_status_equals_the_http_services_body() {
    let (state, detections) = state();
    state.poller_cache().poll_once(state.connectors()).await;
    let after_poll = detections.load(Ordering::SeqCst);

    let fleet = fleet_status(&state).await;
    assert!(fleet.ready && fleet.overlaid);
    assert_eq!(fleet.services[0].id, "stub-running", "ordered for display");

    let response = crate::server::build_router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/console/services")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let http: Value = serde_json::from_slice(&bytes).expect("json");

    assert_eq!(serde_json::to_value(&fleet.services).expect("encode"), http);
    assert_eq!(detections.load(Ordering::SeqCst), after_poll);
}

/// Why: the listener must serve both methods over a real hardened socket and
/// unlink it on shutdown.
/// What: binds in a temp dir, calls each method with the trusty-common UDS
/// client, shuts down, and checks the file is gone.
/// Test: this test itself.
#[tokio::test]
async fn listener_serves_both_methods_over_a_real_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("console.sock");
    let (state, _) = state();
    let socket = start_at(state, &path).await.expect("bind");
    assert_eq!(socket.path(), path);

    for method in [METHOD_HEALTH, METHOD_FLEET_STATUS, "console.nope"] {
        let request = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": {}});
        let response: RpcResponse = send_framed_request(&path, &request, CALL_TIMEOUT)
            .await
            .expect("framed call");
        assert_eq!(response.id, json!(7));
        match method {
            METHOD_HEALTH => {
                let health: ConsoleHealth =
                    serde_json::from_value(response.result.expect("result")).expect("decode");
                assert_eq!(health.status, "ok");
            }
            METHOD_FLEET_STATUS => {
                let _: ConsoleFleetStatus =
                    serde_json::from_value(response.result.expect("result")).expect("decode");
            }
            _ => assert_eq!(response.error.expect("error").code, CODE_METHOD_NOT_FOUND),
        }
    }

    socket.shutdown().await;
    assert!(!path.exists(), "shutdown unlinks the socket");
}

/// Why: ADR-0034 §3 — the socket is private to this uid.
/// What: the socket file is 0600 and its directory 0700 after bind.
/// Test: this test itself.
#[tokio::test]
async fn socket_is_0600_inside_a_0700_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock_dir = dir.path().join("data");
    std::fs::create_dir(&sock_dir).expect("mkdir");
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = sock_dir.join("console.sock");
    let (state, _) = state();
    let socket = start_at(state, &path).await.expect("bind");

    let mode = |p: &std::path::Path| std::fs::metadata(p).expect("stat").permissions().mode();
    assert_eq!(mode(&path) & 0o777, 0o600);
    assert_eq!(mode(&sock_dir) & 0o777, 0o700);
    socket.shutdown().await;
}

/// Why: singleton semantics — a second console must not take over a live
/// socket; `start` turns this error into a warning and HTTP-only serving.
/// What: binds once, then a second `start_at` on the same path is refused
/// with `AlreadyServing`, and the first listener still answers.
/// Test: this test itself.
#[tokio::test]
async fn a_live_socket_refuses_a_second_bind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("console.sock");
    let (state, _) = state();
    let first = start_at(state.clone(), &path).await.expect("first bind");
    let second = start_at(state, &path).await;
    assert!(
        matches!(second, Err(UdsSecurityError::AlreadyServing { .. })),
        "got {second:?}"
    );
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": METHOD_HEALTH});
    let response: RpcResponse = send_framed_request(&path, &request, CALL_TIMEOUT)
        .await
        .expect("first listener still answers");
    assert!(response.result.is_some());
    first.shutdown().await;
}
