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

/// A `tracing` writer that keeps every formatted line for the test to read.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn text(&self) -> String {
        let bytes = self.0.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One plain `GET /health` over TCP; answers the raw response text.
async fn http_get_health(addr: std::net::SocketAddr) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let request = format!("GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut response = String::new();
    tokio::time::timeout(CALL_TIMEOUT, stream.read_to_string(&mut response))
        .await
        .expect("HTTP answer within the timeout")
        .expect("read");
    response
}

/// Why: the Architect's ruling on #10902 — a UDS bind failure warns and the
/// HTTP console keeps serving; it must never take the console down.
/// What: points the data dir at a tempdir whose `trusty-console.sock` is a
/// regular file, so the real [`start`] resolves the canonical path and the
/// hardened bind refuses it. Asserts `start` answered `None`, the warning
/// names the path and the error, and an HTTP listener started first still
/// answers `GET /health` afterwards.
/// Test: this test itself.
#[test]
fn a_failed_bind_warns_and_http_keeps_serving() {
    // DATA_DIR_OVERRIDE is process-global; the connector tests read it too.
    let _env = crate::detect::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let sock_dir = tmp.path().join(CONSOLE_SERVICE);
    std::fs::create_dir(&sock_dir).expect("mkdir");
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let blocker = sock_dir.join(format!("{CONSOLE_SERVICE}.sock"));
    std::fs::write(&blocker, b"not a socket").expect("plant a regular file");

    let logs = CapturedLogs::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _log_guard = tracing::subscriber::set_default(subscriber);

    // SAFETY: serialised by ENV_LOCK; removed before the lock is released.
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let (socket, before, after) = runtime.block_on(async {
        let (state, _) = state();
        let http = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind HTTP");
        let addr = http.local_addr().expect("addr");
        let app = crate::server::build_router(state.clone());
        let server = tokio::spawn(async move {
            let _ = axum::serve(http, app).await;
        });
        let before = http_get_health(addr).await;
        let socket = start(state).await;
        let after = http_get_health(addr).await;
        server.abort();
        (socket, before, after)
    });
    unsafe {
        std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
    }

    assert!(
        socket.is_none(),
        "a refused bind answers None, not a socket"
    );
    assert!(
        before.starts_with("HTTP/1.1 200"),
        "HTTP up first: {before}"
    );
    assert!(
        after.starts_with("HTTP/1.1 200"),
        "HTTP still serves after the failed bind: {after}"
    );
    let text = logs.text();
    assert!(
        text.contains("WARN") && text.contains("UDS listener not bound; serving HTTP only"),
        "the warning path ran: {text}"
    );
    assert!(
        text.contains(&blocker.display().to_string()),
        "the warning names the path: {text}"
    );
    assert!(
        text.contains("error="),
        "the warning carries the error: {text}"
    );
    assert!(blocker.is_file(), "the occupant is left untouched");
}
