//! The #5439 / #6472 authentication arms of `serve::http`'s router.
//!
//! Why: a sibling file for the same reason `http_origin_guard_tests` is one —
//! these assert a SECURITY property of the assembled production router, and
//! keeping them apart from the routing smoke tests makes it obvious which
//! tests would go green again if the guard were removed. Every test here
//! builds the real `build_axum_router`, not a stand-in; the class of bug they
//! exist for is a middleware line that stops covering routes merged in later.
//!
//! What: the deny arms (missing, malformed, wrong credential across the RPC,
//! REST, and SSE surfaces), the allow arms (the same routes with a valid
//! credential), `/health`'s two payloads (#6472), and the SSE ticket exchange
//! that exists because `EventSource` cannot send a header.
//!
//! Test: this module is itself the test surface.

use super::tests::{
    TEST_TOKEN, authed_request, router_and_sessions, test_auth, test_binding,
    test_workstreams_store,
};
use super::*;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::util::ServiceExt;

/// The exact router `tcode serve --http` serves.
async fn guarded_router() -> AxumRouter {
    let (router, sessions, workstreams) = router_and_sessions().await;
    build_axum_router(router, sessions, workstreams, test_binding(), test_auth())
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("body is JSON")
}

/// A well-formed `session.create` body, so a request that got PAST the guard
/// would genuinely mutate daemon state — the deny arms below assert the guard
/// stops it before that happens, not merely that it returns a non-200.
fn rpc_create_body() -> Body {
    Body::from(
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session.create",
            "params": {"task": "auth-arm"}
        })
        .to_string(),
    )
}

/// The #5439 regression, stated for the whole surface: an unauthenticated
/// caller must be refused on the JSON-RPC route, every REST group, and both
/// SSE streams.
///
/// Why one table rather than one test per route: the guard is a single
/// router-wide layer, so per-route tests would assert the same line fourteen
/// times — what actually needs pinning is that no route is MISSING from its
/// coverage, which a table makes visible at a glance.
/// This fails before the fix: every one of these returned `200` on `main`.
#[tokio::test]
async fn unauthenticated_requests_are_rejected_on_every_route() {
    for (method, uri) in [
        ("POST", "/rpc"),
        ("GET", "/sessions"),
        ("POST", "/sessions"),
        ("GET", "/sessions/any-id"),
        ("GET", "/sessions/any-id/transcript"),
        ("GET", "/sessions/any-id/events"),
        ("POST", "/sessions/any-id/messages"),
        ("POST", "/sessions/any-id/cancel"),
        ("POST", "/tasks"),
        ("GET", "/fs"),
        ("GET", "/projects"),
        ("GET", "/agents"),
        ("GET", "/skills"),
        ("GET", "/workstreams"),
        ("POST", "/workstreams"),
        ("GET", "/workstreams/any-id/events"),
        ("POST", "/auth/sse-ticket?path=%2Fsessions%2Fs1%2Fevents"),
    ] {
        let resp = guarded_router()
            .await
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(rpc_create_body())
                    .expect("build request"),
            )
            .await
            .expect("router response");
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri} must require a credential"
        );
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.expect("body");
        assert!(
            bytes.is_empty(),
            "{method} {uri} 401 body must disclose nothing, got {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }
}

/// A wrong or malformed credential must be refused exactly like a missing
/// one — same status, same empty body, so nothing distinguishes "you guessed
/// wrong" from "you sent nothing".
#[tokio::test]
async fn wrong_and_malformed_credentials_are_rejected() {
    let wrong_token = "f".repeat(TEST_TOKEN.len());
    for value in [
        format!("Bearer {wrong_token}"),
        TEST_TOKEN.to_string(),
        format!("Basic {TEST_TOKEN}"),
        format!("Bearer {}", &TEST_TOKEN[..TEST_TOKEN.len() - 1]),
        "Bearer".to_string(),
        String::new(),
    ] {
        let resp = guarded_router()
            .await
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header(header::AUTHORIZATION, &value)
                    .header("content-type", "application/json")
                    .body(rpc_create_body())
                    .expect("build request"),
            )
            .await
            .expect("router response");
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "credential {value:?} must not authenticate"
        );
    }
}

/// The allow arm: the same mutating call, with the credential, reaches the
/// handler and creates a session — proving the guard is a check, not a wall.
#[tokio::test]
async fn an_authenticated_caller_reaches_the_handler() {
    let resp = guarded_router()
        .await
        .oneshot(
            authed_request()
                .method("POST")
                .uri("/rpc")
                .header("content-type", "application/json")
                .body(rpc_create_body())
                .expect("build request"),
        )
        .await
        .expect("router response");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert!(
        v["result"]["id"].is_string(),
        "session.create must have run: {v}"
    );
}

/// #6472: an anonymous `GET /health` must answer liveness and NOTHING else.
///
/// Asserting the exact key set (not just the absence of `pid`) is what makes a
/// future field addition fail here rather than leak. The route stays
/// unauthenticated on purpose — trusty-console's gateway polls it holding no
/// credential.
#[tokio::test]
async fn health_is_public_but_discloses_only_liveness() {
    let resp = guarded_router()
        .await
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(super::PUBLIC_HEALTH_PATH)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router response");

    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a liveness poll must succeed"
    );
    let v = body_json(resp).await;
    assert_eq!(v, serde_json::json!({"status": "ok"}));
    let obj = v.as_object().expect("object");
    for leaked in ["pid", "binding", "version", "server", "incremental_index"] {
        assert!(
            !obj.contains_key(leaked),
            "anonymous /health disclosed {leaked}: {v}"
        );
    }
}

/// The same route WITH a credential must still carry `pid` and `binding` —
/// `tcode tui`'s auto-attach (#4512) reads the binding from here to refuse a
/// daemon bound to a different project, and narrowing the route for everyone
/// would have broken that.
#[tokio::test]
async fn health_with_a_credential_still_reports_pid_and_binding() {
    let resp = guarded_router()
        .await
        .oneshot(
            authed_request()
                .method("GET")
                .uri(super::PUBLIC_HEALTH_PATH)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router response");

    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["pid"], std::process::id());
    assert!(v["binding"].is_object(), "binding missing: {v}");
    assert_eq!(v["version"], crate::VERSION);
}

/// The public carve-out is exactly one path. A near-miss must stay guarded,
/// so `/health` can never be widened into a prefix by a future route name.
#[tokio::test]
async fn only_health_is_public() {
    for uri in ["/healthz", "/health/detail", "/HEALTH"] {
        let resp = guarded_router()
            .await
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("router response");
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{uri} must not inherit /health's exemption"
        );
    }
}

/// A ticket is minted only for a caller that already holds the credential —
/// it carries an existing right onto a URL, it is never a way in.
#[tokio::test]
async fn sse_ticket_requires_a_credential() {
    let resp = guarded_router()
        .await
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "{}?path=/sessions/s1/events",
                    super::SSE_TICKET_PATH
                ))
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// A ticket may be minted only for one of the two SSE streams.
///
/// Why: the ticket's narrowness is exactly the set of paths it can be minted
/// for. A handler that minted for whatever it was handed would give a log
/// reader back the arbitrary authenticated request — `POST /rpc` included —
/// that binding the ticket to a path exists to remove.
#[tokio::test]
async fn a_ticket_cannot_be_minted_for_a_non_sse_path() {
    for path in [
        "/rpc",
        "/sessions",
        "/sessions/s1",
        "/sessions/s1/transcript",
        "/sessions//events",
        "/agents",
        "/health",
        "/sessions/s1/events/extra",
        "",
    ] {
        let resp = guarded_router()
            .await
            .oneshot(
                authed_request()
                    .method("POST")
                    .uri(format!(
                        "{}?path={}",
                        super::SSE_TICKET_PATH,
                        urlencode(path)
                    ))
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("router response");
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "a ticket must not be mintable for {path:?}"
        );
    }
}

/// Percent-encode the few characters these test paths carry — enough for a
/// query value, not a general encoder.
fn urlencode(raw: &str) -> String {
    raw.chars()
        .map(|c| match c {
            '/' => "%2F".to_string(),
            '&' | '=' | '?' | '#' | ' ' => format!("%{:02X}", c as u8),
            other => other.to_string(),
        })
        .collect()
}

/// The browser path end to end: mint a ticket over the header-authenticated
/// surface, open the SSE stream with it, and prove the SAME ticket cannot open
/// a second one.
///
/// Why the replay assertion matters: a ticket rides in a URL, so it lands in
/// access logs and tracing spans. Single use is what makes that acceptable.
#[tokio::test]
async fn an_sse_ticket_opens_one_stream_then_is_spent() {
    let (router, sessions, workstreams) = router_and_sessions().await;
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let app = build_axum_router(router, sessions, workstreams, test_binding(), test_auth());

    let stream_path = format!("/sessions/{}/events", session.id);
    let minted = app
        .clone()
        .oneshot(
            authed_request()
                .method("POST")
                .uri(format!(
                    "{}?path={}",
                    super::SSE_TICKET_PATH,
                    urlencode(&stream_path)
                ))
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router response");
    assert_eq!(minted.status(), StatusCode::OK);
    let ticket = body_json(minted).await["ticket"]
        .as_str()
        .expect("ticket is a string")
        .to_string();

    let uri = format!(
        "{stream_path}?{}={ticket}",
        trusty_common::server::bearer_auth::TICKET_QUERY_PARAM
    );
    let opened = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&uri)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router response");
    assert_eq!(
        opened.status(),
        StatusCode::OK,
        "ticket must open the stream"
    );

    let replayed = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&uri)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router response");
    assert_eq!(
        replayed.status(),
        StatusCode::UNAUTHORIZED,
        "a spent ticket must not open a second stream"
    );
}

/// A ticket minted for one stream must not open a DIFFERENT route.
///
/// This is the arm that fails if the path binding is dropped: before it, a
/// ticket read from a trace log within its 30s life bought one arbitrary
/// authenticated request, `POST /rpc` — the whole JSON-RPC method surface —
/// included.
#[tokio::test]
async fn a_ticket_is_bound_to_the_stream_it_was_minted_for() {
    let (router, sessions, workstreams) = router_and_sessions().await;
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let app = build_axum_router(router, sessions, workstreams, test_binding(), test_auth());

    let minted = app
        .clone()
        .oneshot(
            authed_request()
                .method("POST")
                .uri(format!(
                    "{}?path={}",
                    super::SSE_TICKET_PATH,
                    urlencode(&format!("/sessions/{}/events", session.id))
                ))
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router response");
    let ticket = body_json(minted).await["ticket"]
        .as_str()
        .expect("ticket is a string")
        .to_string();

    let param = trusty_common::server::bearer_auth::TICKET_QUERY_PARAM;
    for (method, uri) in [
        ("POST", format!("/rpc?{param}={ticket}")),
        ("GET", format!("/sessions?{param}={ticket}")),
        (
            "GET",
            format!("/sessions/{}/transcript?{param}={ticket}", session.id),
        ),
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(&uri)
                    .header("content-type", "application/json")
                    .body(rpc_create_body())
                    .expect("build request"),
            )
            .await
            .expect("router response");
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "an SSE ticket must not authenticate {method} {uri}"
        );
    }
}

/// A fabricated ticket must not open a stream — the guard redeems from its own
/// table, it does not merely check that the parameter is present.
#[tokio::test]
async fn a_fabricated_ticket_is_rejected() {
    let (router, sessions, workstreams) = router_and_sessions().await;
    let session = sessions.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let app = build_axum_router(
        router,
        sessions,
        test_workstreams_store().await,
        test_binding(),
        test_auth(),
    );
    drop(workstreams);

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/sessions/{}/events?{}={TEST_TOKEN}",
                    session.id,
                    trusty_common::server::bearer_auth::TICKET_QUERY_PARAM
                ))
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router response");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
