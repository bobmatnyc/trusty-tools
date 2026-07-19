//! Regression tests for the router-wide same-origin write guard (#3304).
//!
//! Why: before #3304 the daemon guarded exactly ONE handler (`coordinator_chat`)
//! via `origin_allowed`; every OTHER destructive route (`POST /sessions` spawn,
//! `DELETE /sessions/{id}`, control-session stop, managed-session mutation,
//! `/claude-config/apply`, `/pair/*`) was exposed to cross-origin CSRF. The
//! daemon now layers the shared `trusty_common` guard router-wide via
//! [`super::guard_router`] at the serve site. These tests prove a formerly
//! unguarded write route (`POST /sessions`) is now covered, while reads and
//! server-side/loopback callers are unaffected.
//! What: builds `guard_router(router(state), SelfOrigins::default())` (the
//! loopback-only primary-listener configuration) and drives it with `oneshot`.
//! Test: this module.

use super::guard_router;
use crate::daemon::api::router;
use crate::daemon::state::DaemonState;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use trusty_common::server::SelfOrigins;

/// Build the guarded daemon router exactly as the primary (loopback) listener
/// does in `daemon::mod` — the router-wide guard trusting only loopback.
fn guarded() -> axum::Router {
    guard_router(router(DaemonState::shared()), SelfOrigins::default())
}

/// Why: the CSRF threat — a cross-origin page POSTing to `POST /sessions` (spawn
/// a managed session), a route that had NO guard before #3304. The router-wide
/// guard MUST reject it with `403` before the handler runs.
/// Test: this test.
#[tokio::test]
async fn session_spawn_rejects_cross_origin_write() {
    let resp = guarded()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("origin", "http://evil.example.com")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin POST /sessions must be rejected by the router-wide write guard"
    );
}

/// Why: the daemon's own loopback UI and server-side callers (the `serve
/// --stdio` bridge, `curl`, the TUI/Telegram adapters — which send NO `Origin`)
/// must keep driving writes; loopback and missing-Origin writes must NOT 403.
/// Test: this test.
#[tokio::test]
async fn session_spawn_allows_loopback_and_missing_origin() {
    // Loopback origin → allowed (handler runs; never 403).
    let loopback = guarded()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("origin", "http://127.0.0.1:4317")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        loopback.status(),
        StatusCode::FORBIDDEN,
        "loopback-origin POST /sessions must pass the guard"
    );
    // No Origin (server-side caller) → allowed.
    let missing = guarded()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        missing.status(),
        StatusCode::FORBIDDEN,
        "missing-Origin POST /sessions (server-side caller) must pass the guard"
    );
}

/// Why: the guard is method-gated — a cross-origin GET read leaks no destructive
/// capability and must NOT be blocked.
/// Test: this test.
#[tokio::test]
async fn read_route_allows_cross_origin() {
    let resp = guarded()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/health")
                .header("origin", "http://evil.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin GET /health must not be blocked by the write guard"
    );
}
