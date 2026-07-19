//! Regression tests for the router-wide same-origin write guard (#3304).
//!
//! Why: trusty-memory exposes DESTRUCTIVE write routes behind the
//! permissive-CORS shared middleware stack — `POST /rpc` (the full JSON-RPC
//! tool surface), `DELETE /api/v1/palaces/{id}` (palace deletion),
//! `POST /api/v1/admin/stop`, KG asserts/deletes. Before #3304 these had NO
//! write-origin guard, so any page the operator visited could drive them
//! cross-origin (CSRF). The daemon now composes the shared
//! `trusty_common::server::with_guarded_middleware` router-wide.
//! What: builds the router via `router()` (loopback-only default) and drives it
//! with `oneshot` HTTP requests against `POST /rpc` (write) and
//! `/api/v1/status` (read).
//! Test: this module.

use super::super::router;
use super::test_state;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::util::ServiceExt;

/// Why: the CSRF threat — a malicious cross-origin page POSTing to the JSON-RPC
/// tool surface (which can delete palaces/drawers, run dreams, mutate the KG).
/// The router-wide guard MUST reject it with `403` before the handler runs.
/// Test: this test.
#[tokio::test]
async fn rpc_rejects_cross_origin_write() {
    let app = router().with_state(test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/rpc")
                .header("origin", "http://evil.example.com")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin POST /rpc must be rejected by the write guard"
    );
}

/// Why: server-side callers — the console reverse proxy, the `serve --stdio`
/// bridge fallback, `curl` — send NO `Origin`; and the daemon's own loopback UI
/// sends a loopback `Origin`. Both must keep driving `/rpc` (fail-open contract).
/// Test: this test.
#[tokio::test]
async fn rpc_allows_loopback_and_missing_origin() {
    let body = || Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
    // Loopback origin → allowed (handler processes; never 403).
    let app = router().with_state(test_state());
    let loopback = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/rpc")
                .header("origin", "http://127.0.0.1:7070")
                .header("content-type", "application/json")
                .body(body())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        loopback.status(),
        StatusCode::FORBIDDEN,
        "loopback-origin POST /rpc must pass the guard"
    );
    // No Origin (server-side caller / stdio bridge) → allowed.
    let app = router().with_state(test_state());
    let missing = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/rpc")
                .header("content-type", "application/json")
                .body(body())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        missing.status(),
        StatusCode::FORBIDDEN,
        "missing-Origin POST /rpc (server-side caller) must pass the guard"
    );
}

/// Why: the guard is method-gated — a cross-origin GET read leaks no
/// destructive capability and must NOT be blocked (dashboards read cross-origin
/// under the permissive CORS policy).
/// Test: this test.
#[tokio::test]
async fn read_route_allows_cross_origin() {
    let app = router().with_state(test_state());
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/status")
                .header("origin", "http://evil.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin GET /api/v1/status must not be blocked by the write guard"
    );
}
