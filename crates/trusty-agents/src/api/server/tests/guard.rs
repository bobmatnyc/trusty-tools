//! Loopback-only doctrine tests (#3329): the router-wide same-origin write
//! guard and the non-loopback-without-token startup refusal.
//!
//! Why: `POST /api/task` (and the `/api/tm/*`, `/api/ctrl/*`, `POST /rpc`
//! surfaces) can spawn subprocesses and mutate live sessions. The shared
//! `trusty_common::server::with_guarded_middleware` guard (adopted router-wide
//! in `routes::build_router_with_origins`) must reject cross-origin writes
//! (CSRF) while leaving GET reads, SSE, and the legitimate agents-ui webview
//! path working. The webview-origin finding: in Tauri desktop mode every
//! backend WRITE travels over Tauri IPC (`@tauri-apps/api/core#invoke`), never
//! HTTP, so the guard never sees it; the only HTTP the desktop webview makes is
//! the `GET /api/events` SSE stream (a safe method the guard ignores). In
//! browser mode the SPA is served same-origin from `tagent --api`, so its
//! `POST /api/task` carries a loopback `Origin`. Both paths are admitted below;
//! a cross-origin `Origin` is rejected.
//! What: exercises the guard via `oneshot` on `build_router` (loopback-only
//! self-origins, the production test default) and asserts `serve_with_config`
//! refuses an unauthenticated non-loopback bind.
//! Test: this module IS the test.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

use crate::api::server::ApiConfig;
use crate::api::server::routes::{build_router, serve_with_config};
use crate::api::server::state::AppState;

/// POST `/api/tm/sessions` carrying `origin`, returning the response status.
///
/// Why: `/api/tm/sessions` is a guarded write route that returns a clean 503
/// (no TmManager in a default `AppState`) once the guard admits it — so a 403
/// unambiguously came from the guard and a 503 unambiguously means the guard
/// let the request through to the handler.
/// What: builds a default (loopback-only) router and oneshots one POST.
/// Test: used by every guard case below.
async fn post_tm_sessions_with_origin(origin: Option<&str>) -> StatusCode {
    let app = build_router(AppState::default());
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/api/tm/sessions")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(o) = origin {
        builder = builder.header(header::ORIGIN, o);
    }
    let req = builder
        .body(Body::from(r#"{"project_path":"/tmp"}"#))
        .unwrap();
    app.oneshot(req).await.unwrap().status()
}

/// Why: a genuinely cross-origin browser page firing `POST /api/tm/sessions`
/// (CSRF) must be rejected with 403 by the router-wide guard.
/// Test: this test.
#[tokio::test]
async fn cross_origin_write_rejected_403() {
    let status = post_tm_sessions_with_origin(Some("http://evil.example.com")).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "cross-origin write must be 403'd by the same-origin guard"
    );
}

/// Why: the browser-mode agents-ui is served same-origin from `tagent --api`,
/// so its writes carry a loopback `Origin` and MUST be admitted (the guard
/// passes them to the handler, which returns 503 without a TmManager).
/// Test: this test.
#[tokio::test]
async fn loopback_write_allowed() {
    let status = post_tm_sessions_with_origin(Some("http://localhost:8765")).await;
    assert_ne!(
        status,
        StatusCode::FORBIDDEN,
        "loopback (browser same-origin) write must NOT be blocked by the guard"
    );
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "admitted write should reach the handler (503 without a TmManager)"
    );
}

/// Why: server-side callers (the console reverse proxy, curl, the Tauri IPC
/// path) send no `Origin` header; the guard fails open on a missing Origin so
/// those keep working.
/// Test: this test.
#[tokio::test]
async fn missing_origin_write_allowed() {
    let status = post_tm_sessions_with_origin(None).await;
    assert_ne!(status, StatusCode::FORBIDDEN);
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

/// Why: the real Origin a Tauri (macOS) webview presents on any HTTP request is
/// `tauri://localhost`, which the shared guard classifies as loopback (host ==
/// "localhost"). Documenting it here proves the desktop webview is admitted
/// even on the (IPC-only in practice) write path, not just for its SSE GET.
/// Test: this test.
#[tokio::test]
async fn tauri_localhost_origin_allowed() {
    let status = post_tm_sessions_with_origin(Some("tauri://localhost")).await;
    assert_ne!(
        status,
        StatusCode::FORBIDDEN,
        "the Tauri webview origin (tauri://localhost) must be admitted as loopback"
    );
}

/// Why: the guard is method-gated — GET reads leak no destructive capability
/// and must pass through untouched even from a cross-origin page.
/// Test: this test.
#[tokio::test]
async fn cross_origin_get_read_unaffected() {
    let app = build_router(AppState::default());
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/health")
        .header(header::ORIGIN, "http://evil.example.com")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a safe GET must never be blocked by the write guard"
    );
}

/// Why: loopback-only doctrine (#3329) — an unauthenticated non-loopback bind
/// exposes an arbitrary-subprocess-spawning API to the LAN and MUST be refused
/// at startup with an actionable error, before any listener is bound.
/// What: `serve_with_config` bails at the top of the function (before the docs
/// spawn / bind), so this asserts the refusal without opening a socket.
/// Test: this test.
#[tokio::test]
async fn non_loopback_without_token_refuses_start() {
    let cfg = ApiConfig {
        bind: std::net::Ipv4Addr::UNSPECIFIED.into(), // 0.0.0.0
        port: 0,
        token: None,
    };
    let err = serve_with_config(cfg)
        .await
        .expect_err("non-loopback bind without a token must refuse to start");
    let msg = err.to_string();
    assert!(
        msg.contains("non-loopback") && msg.contains("token"),
        "refusal must be actionable (mention non-loopback + token), got: {msg}"
    );
    assert!(
        msg.contains("/api/agents/*"),
        "refusal should point at the console proxy as the intended remote path, got: {msg}"
    );
}
