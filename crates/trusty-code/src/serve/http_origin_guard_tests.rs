//! Both-arm regression tests for the shared origin guard on the `tcode serve
//! --http` router (#6003).
//!
//! Why: this router served every route behind `with_standard_middleware` —
//! permissive CORS (`allow_origin(Any)`) and no origin guard — while every
//! sibling daemon routed through the shared guard. `tcode serve --http`
//! defaults to a fixed loopback port and `tcode tui` auto-spawns it, so a page
//! in the operator's browser could POST cross-origin to `/rpc`, `/tasks`,
//! `/sessions`, `/sessions/{id}/messages`, `/agents` and the workstream write
//! routes, and read `GET /sessions/{id}/transcript` plus both SSE streams.
//! These tests pin both arms so a future edit to `build_axum_router`'s
//! middleware line fails here rather than silently reopening the surface.
//! What: drives the full router via `tower::util::ServiceExt::oneshot` (no
//! socket). Write arm — a foreign `Origin` on `POST /rpc` and `POST /tasks` is
//! `403`; a loopback `Origin` and an absent `Origin` are not. Read arm — a
//! foreign `Origin` on `GET /sessions/{id}/transcript` and on both SSE routes
//! gets no `access-control-allow-origin` reflection, so a browser cannot read
//! the body; a loopback `Origin` is reflected. Fail-closed — a non-UTF-8
//! `Origin` on a write is `403`.
//! Test: this module. Run with
//! `cargo test -p trusty-code serve::http::origin_guard_tests`.
//!
//! This is CSRF/read-disclosure defence, not caller authentication — #5439
//! (auth, and the HTTP-vs-UDS transport question under ADR-0032) stays open.

use super::tests::{authed_request, router_and_sessions, test_auth, test_binding};
use super::*;
use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use tower::util::ServiceExt;

/// The exact router `tcode serve --http` serves, over a fresh empty session
/// registry and workstream store.
///
/// Why: the guard must be asserted against the assembled production router,
/// not a stand-in — the whole class of bug this file exists for is a
/// middleware line that stops covering routes merged in later.
async fn guarded_router() -> AxumRouter {
    let (router, sessions, workstreams) = router_and_sessions().await;
    build_axum_router(router, sessions, workstreams, test_binding(), test_auth())
}

/// A well-formed `session.create` JSON-RPC body, so the write-arm requests
/// reach a handler that would genuinely mutate state if the guard let them
/// through.
fn rpc_create_body() -> Body {
    Body::from(
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session.create",
            "params": {}
        })
        .to_string(),
    )
}

/// A `ping` JSON-RPC body for the ALLOW arms.
///
/// Why: those arms assert only that the guard let the request reach the
/// dispatcher, so they use the one method that mutates nothing — a real
/// `session.create` would leave a live session behind in each test.
fn rpc_ping_body() -> Body {
    Body::from(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string())
}

/// Why: `POST /rpc` is the whole JSON-RPC method surface (`session.*`,
/// `workstream.*`, task execution) reachable in one route. A page on a foreign
/// origin driving it is the CSRF threat this guard exists for, so it MUST be
/// `403`ed before `Router::dispatch_json` runs.
/// Test: this test.
#[tokio::test]
async fn rpc_rejects_cross_origin_write() {
    let resp = guarded_router()
        .await
        .oneshot(
            authed_request()
                .method("POST")
                .uri("/rpc")
                .header(header::ORIGIN, "http://evil.example.com")
                .header(header::CONTENT_TYPE, "application/json")
                .body(rpc_create_body())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin POST /rpc must be rejected by the write guard"
    );
}

/// Why: the REST write routes are merged into the router AFTER the core routes
/// are registered. A `route_layer` (or a guard applied mid-chain) would miss
/// them — the #3268 lesson — so `POST /tasks` is asserted separately from
/// `/rpc` rather than assumed to follow from it.
/// Test: this test.
#[tokio::test]
async fn merged_rest_write_route_rejects_cross_origin() {
    let resp = guarded_router()
        .await
        .oneshot(
            authed_request()
                .method("POST")
                .uri("/tasks")
                .header(header::ORIGIN, "https://evil.example.com")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-origin POST /tasks must be rejected — the guard is router-wide, \
         so it covers routes merged in after the core ones"
    );
}

/// Why: SECURITY fail-closed. An `Origin` header whose bytes are not UTF-8
/// cannot be classified, so the guard must reject rather than fall through to
/// the handler.
/// Test: this test.
#[tokio::test]
async fn write_with_non_utf8_origin_is_rejected() {
    let resp = guarded_router()
        .await
        .oneshot(
            authed_request()
                .method("POST")
                .uri("/rpc")
                .header(
                    header::ORIGIN,
                    HeaderValue::from_bytes(&[0xff, 0xfe]).expect("header value"),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(rpc_create_body())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "an unclassifiable Origin must fail closed, not fall through"
    );
}

/// Why: the daemon's own loopback-served surface — the GUI's Vite dev server on
/// `http://localhost:5174`, a browser tab on `http://127.0.0.1:7882` — must keep
/// driving writes. A loopback `Origin` is trusted and must NOT be `403`ed.
/// Test: this test.
#[tokio::test]
async fn rpc_allows_same_origin_write() {
    for origin in ["http://127.0.0.1:7882", "http://localhost:5174"] {
        let resp = guarded_router()
            .await
            .oneshot(
                authed_request()
                    .method("POST")
                    .uri("/rpc")
                    .header(header::ORIGIN, origin)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(rpc_ping_body())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "loopback origin {origin} must pass the write guard"
        );
    }
}

/// Why: every non-browser caller — `tcode tui`'s own HTTP client, the console
/// reverse proxy, `curl`, the auto-spawn health probe — sends no `Origin` at
/// all. The guard fails OPEN on an absent `Origin` by design; if that ever
/// changed, the CLI would break wholesale.
/// Test: this test.
#[tokio::test]
async fn rpc_allows_cli_shaped_write_with_no_origin() {
    let resp = guarded_router()
        .await
        .oneshot(
            authed_request()
                .method("POST")
                .uri("/rpc")
                .header(header::CONTENT_TYPE, "application/json")
                .body(rpc_ping_body())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a request with no Origin header is not the CSRF threat model and must pass"
    );
}

/// Why: the read half of #6003. `GET /sessions/{id}/transcript` and both SSE
/// streams carry conversation content, and the write guard is method-gated —
/// it deliberately lets `GET` through. What stops a foreign page reading them
/// is the CORS policy: with `allow_origin(Any)` the browser handed the body
/// over, and with the same-origin policy no `access-control-allow-origin`
/// header comes back, so the browser refuses it.
/// Test: this test.
#[tokio::test]
async fn content_reads_are_not_cors_reflected_to_a_foreign_origin() {
    for uri in [
        "/sessions/does-not-exist/transcript",
        "/sessions/does-not-exist/events",
        "/workstreams/does-not-exist/events",
    ] {
        let resp = guarded_router()
            .await
            .oneshot(
                authed_request()
                    .uri(uri)
                    .header(header::ORIGIN, "https://evil.example.com")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none(),
            "{uri} must not reflect a foreign origin — a browser page would \
             otherwise read the response body"
        );
    }
}

/// Why: the tightened CORS policy must not break the local browser surfaces it
/// exists to preserve — the GUI's `pnpm dev` server and a tab on the daemon's
/// own loopback address must still get the reflection header back.
/// Test: this test.
#[tokio::test]
async fn content_reads_are_cors_reflected_to_a_same_machine_origin() {
    for origin in ["http://127.0.0.1:7882", "http://localhost:5174"] {
        let resp = guarded_router()
            .await
            .oneshot(
                authed_request()
                    .uri("/sessions/does-not-exist/transcript")
                    .header(header::ORIGIN, origin)
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router responds");
        assert_eq!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .map(|v| v.to_str().expect("ascii header")),
            Some(origin),
            "a same-machine origin must still be reflected"
        );
    }
}

/// Why: SECURITY — the reflection decision must parse the `Origin` host as an
/// IP, never prefix-match it. `http://127.0.0.1.evil.com` is an
/// attacker-registrable DNS name; reflecting it would hand a remote page every
/// transcript on the box.
/// Test: this test.
#[tokio::test]
async fn ip_prefixed_dns_lookalike_is_neither_reflected_nor_allowed_to_write() {
    let lookalike = "http://127.0.0.1.evil.com";

    let read = guarded_router()
        .await
        .oneshot(
            authed_request()
                .uri("/sessions/does-not-exist/transcript")
                .header(header::ORIGIN, lookalike)
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert!(
        read.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "a 127-prefixed DNS name must not be reflected"
    );

    let write = guarded_router()
        .await
        .oneshot(
            authed_request()
                .method("POST")
                .uri("/rpc")
                .header(header::ORIGIN, lookalike)
                .header(header::CONTENT_TYPE, "application/json")
                .body(rpc_create_body())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(
        write.status(),
        StatusCode::FORBIDDEN,
        "a 127-prefixed DNS name must not pass the write guard"
    );
}

/// Why: `GET /health` is the liveness probe `tcode tui`'s auto-attach and
/// trusty-console's gateway poll. It is a `GET`, so the method-gated guard
/// leaves it alone — this pins that the guard did not accidentally start
/// blocking reads outright.
/// Test: this test.
#[tokio::test]
async fn health_read_is_unaffected_by_the_guard() {
    let resp = guarded_router()
        .await
        .oneshot(
            authed_request()
                .uri("/health")
                .header(header::ORIGIN, "https://evil.example.com")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the guard is method-gated; GET /health must still answer"
    );
}
