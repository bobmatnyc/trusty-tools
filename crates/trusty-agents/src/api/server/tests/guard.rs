//! Loopback-only doctrine tests (#3329, #6433): the router-wide same-origin
//! write guard and the non-loopback startup refusal, token or not.
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
//! refuses every non-loopback bind.
//! Test: this module IS the test.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

use std::net::IpAddr;

use crate::api::server::ApiConfig;
use crate::api::server::no_api::process_no_api_refusal;
use crate::api::server::routes::{bind_refusal, build_router, serve_with_config};
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
    assert_startup_refused(cfg).await;
}

/// Run `serve_with_config` under a timeout and assert it refused before
/// binding, with either #6433 refusal.
///
/// Why: an exported `TAGENT_NO_API` on the test host makes the no-API
/// refusal fire first; both outcomes prove the bind was never reached, and
/// the test must not mutate process env to force one.
/// What: fails if the call serves past 5 s (code that accepts the bind serves
/// forever) or returns `Ok`; otherwise the error must equal `bind_refusal`'s
/// or the no-API refusal's message.
/// Test: used by the two `non_loopback_*_refuses_start` tests.
async fn assert_startup_refused(cfg: ApiConfig) {
    let bind = cfg.bind;
    let err = tokio::time::timeout(std::time::Duration::from_secs(5), serve_with_config(cfg))
        .await
        .expect("serve_with_config must return at once, not bind and serve (#6433)")
        .expect_err("a non-loopback bind must refuse to start (#6433)");
    let msg = err.to_string();
    let bind_msg = bind_refusal(bind).expect("the bind under test must be non-loopback");
    let no_api_msg = process_no_api_refusal(true, true);
    assert!(
        msg == bind_msg || no_api_msg.as_deref() == Some(msg.as_str()),
        "refusal must be the bind or the no-API refusal, got: {msg}"
    );
}

/// Why: #6433 / DOC-90 AG-2 — only loopback binds are allowed; the refusal
/// is actionable and points remote access at the console proxy.
/// What: a table of refused and accepted addresses through `bind_refusal`,
/// including IPv4-mapped IPv6 forms of private and loopback addresses.
/// Test: this test.
#[test]
fn bind_refusal_table() {
    for refused in [
        "0.0.0.0",
        "::",
        "10.0.0.1",
        "::ffff:10.0.0.1",
        "::ffff:127.0.0.1",
    ] {
        let ip: IpAddr = refused.parse().expect("table address parses");
        let msg = bind_refusal(ip).unwrap_or_else(|| panic!("{refused} must be refused"));
        assert!(
            msg.contains("non-loopback")
                && msg.contains("#6433")
                && msg.contains("--bind")
                && msg.contains("/api/agents/*"),
            "refusal for {refused} must be actionable, got: {msg}"
        );
    }
    for accepted in ["127.0.0.1", "127.0.0.2", "::1"] {
        let ip: IpAddr = accepted.parse().expect("table address parses");
        assert_eq!(bind_refusal(ip), None, "{accepted} must be accepted");
    }
}

/// Why: #6433 / DOC-90 AG-2 — a token does not make LAN exposure acceptable;
/// a non-loopback bind is refused whether or not a token is set.
/// What: `serve_with_config` with `0.0.0.0` and a token returns `Err` before
/// binding, under a timeout (see `assert_startup_refused`).
/// Test: this test.
#[tokio::test]
async fn non_loopback_with_token_refuses_start() {
    let cfg = ApiConfig {
        bind: std::net::Ipv4Addr::UNSPECIFIED.into(), // 0.0.0.0
        port: 0,
        token: Some("tok".to_string()),
    };
    assert_startup_refused(cfg).await;
}

/// Why: #3329's control is the DEFAULT bind, not the refusal — the refusal only
/// fires once something has already chosen a non-loopback address.
/// `non_loopback_without_token_refuses_start` covers that branch; nothing
/// pinned the default itself, so flipping `unauthenticated` back to `0.0.0.0`
/// would restore the original LAN exposure with every other test still green.
/// This is the assertion that fails against the pre-#3341 code, where the
/// server bound `0.0.0.0` unconditionally.
/// What: asserts the no-token constructor binds a loopback address.
/// Test: this test.
#[test]
fn unauthenticated_config_binds_loopback() {
    let cfg = ApiConfig::unauthenticated(7654);
    assert!(
        cfg.bind.is_loopback(),
        "the unauthenticated default must bind loopback, got {}",
        cfg.bind
    );
}

/// Why: `--bind` is parsed independently in `runtime::startup` (raw argv) and
/// `runtime::mode_dispatch` (clap). Both now route through
/// `ApiConfig::with_bind`, so this pins the shared default once instead of
/// trusting two open-coded `unwrap_or`s to stay in agreement.
/// What: absent `--bind` resolves to loopback; an explicit address is taken
/// verbatim so `serve_with_config` can refuse it if non-loopback (#6433).
/// Test: this test.
#[test]
fn unspecified_bind_defaults_to_loopback() {
    assert!(
        ApiConfig::with_bind(None, 7654, None).bind.is_loopback(),
        "omitting --bind must resolve to loopback"
    );
    let explicit = ApiConfig::with_bind(
        Some(std::net::Ipv4Addr::UNSPECIFIED.into()),
        7654,
        Some("tok".to_string()),
    );
    assert!(
        !explicit.bind.is_loopback(),
        "an explicit --bind must be passed through verbatim for the loopback gate to judge"
    );
}
