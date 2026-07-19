//! Regression tests for the router-wide same-origin write guard (#3304).
//!
//! Why: trusty-search exposes destructive write routes (`POST /admin/stop` —
//! daemon shutdown, `POST /indexes`, `DELETE /indexes/{id}`, `POST /upgrade`,
//! reindex) behind the permissive-CORS shared middleware stack. Before #3304
//! these had NO write-origin guard, so any page the operator visited could
//! drive them cross-origin (CSRF). The daemon now composes the shared
//! `trusty_common::server::with_guarded_middleware` router-wide. These tests
//! mirror trusty-console's guard suite: a cross-origin write is `403`, a
//! loopback / missing-Origin write is allowed, and reads are unaffected.
//! What: builds the full router via `build_router` (loopback-only default) and
//! drives it with `oneshot` HTTP requests against `/admin/stop` (write) and
//! `/health` (read).
//! Test: this module. Run with `cargo test -p trusty-search tests_3304`.

use super::build_router;
use crate::core::registry::IndexRegistry;
use crate::service::server::SearchAppState;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

/// Build the full daemon router over a fresh, empty registry (loopback-only
/// self-origins — the `build_router` default). No embedder is installed: the
/// guard runs in front of the handlers, and `/admin/stop` / `/health` need no
/// embedding, so this is sufficient to exercise the middleware.
fn guarded_router() -> axum::Router {
    build_router(SearchAppState::new(IndexRegistry::new()))
}

/// Why: the CSRF threat — a malicious cross-origin page POSTing to the
/// daemon-shutdown endpoint. Its `Origin` is neither loopback nor a self-origin,
/// so the router-wide guard MUST reject it with `403` before the handler runs.
/// Test: this test.
#[tokio::test]
async fn admin_stop_rejects_cross_origin_write() {
    let resp = guarded_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/stop")
                .header("origin", "http://evil.example.com")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin POST /admin/stop must be rejected by the write guard"
    );
}

/// Why: the daemon's own loopback-served UI must keep driving destructive
/// routes — a loopback `Origin` is trusted and must NOT be 403'd.
/// Test: this test.
#[tokio::test]
async fn admin_stop_allows_loopback_write() {
    let resp = guarded_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/stop")
                .header("origin", "http://127.0.0.1:7654")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "loopback-origin POST /admin/stop must pass the guard"
    );
}

/// Why: server-side callers — the console reverse proxy, `curl`, the MCP stdio
/// bridge — send NO `Origin` header; the guard MUST let them through or every
/// existing caller breaks. This is the fail-open-on-missing-Origin contract.
/// Test: this test.
#[tokio::test]
async fn admin_stop_allows_missing_origin() {
    let resp = guarded_router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/stop")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "missing-Origin POST /admin/stop (server-side caller) must pass the guard"
    );
}

/// Why: the guard is method-gated — cross-origin GET reads leak no destructive
/// capability and must NOT be blocked (the daemon SPA and monitoring probes
/// read cross-origin under the permissive CORS policy).
/// Test: this test.
#[tokio::test]
async fn read_route_allows_cross_origin() {
    let resp = guarded_router()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/health")
                .header("origin", "http://evil.example.com")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin GET /health must not be blocked by the write guard"
    );
}
