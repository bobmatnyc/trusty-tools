//! `/api/events` stream authentication (#5052).
//!
//! Why: the SSE stream carries conversation content — `PmThinking.text`,
//! `AgentMessage.text`, `PmDelegating.task_preview` — and was exempt from auth
//! outright, so any page in the operator's browser could read it cross-origin
//! from `127.0.0.1` with no credential. These tests pin BOTH halves of the fix:
//! the route now refuses an uncredentialed caller, and the ticket that lets a
//! browser `EventSource` authenticate can only be minted by a same-origin
//! (and, when a token is configured, token-bearing) caller.
//!
//! `event_stream_without_credential_is_rejected` is the regression test: it
//! returns 200 against the pre-fix code and 401 after.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use tower::ServiceExt;

use super::super::event_tickets::{EventStreamAuth, EventTicketStore};
use super::super::routes::{build_router, build_router_with_config};
use super::super::state::AppState;

/// Router with no operator token — the SHIPPED DEFAULT (Tauri sidecar on
/// `127.0.0.1:7654`), and the exact configuration the disclosure targeted.
fn default_router() -> Router {
    build_router(AppState::default())
}

fn token_router(token: &str) -> Router {
    build_router_with_config(AppState::default(), Some(token.to_string()))
}

async fn status_of(app: &Router, req: Request<Body>) -> StatusCode {
    app.clone()
        .oneshot(req)
        .await
        .expect("router responds")
        .status()
}

fn get_events(query: &str) -> Request<Body> {
    Request::builder()
        .uri(format!("/api/events{query}"))
        .body(Body::empty())
        .expect("request builds")
}

/// Mint a ticket through the live route and return it.
async fn mint_ticket(app: &Router, bearer: Option<&str>) -> String {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/api/events/ticket");
    if let Some(t) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::empty()).expect("request builds"))
        .await
        .expect("router responds");
    assert_eq!(resp.status(), StatusCode::OK, "mint should succeed");
    let bytes = axum::body::to_bytes(resp.into_body(), 4096)
        .await
        .expect("body reads");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    assert_eq!(v["expires_in_secs"], 300);
    v["ticket"]
        .as_str()
        .expect("ticket is a string")
        .to_string()
}

// -- the regression: an uncredentialed stream request is refused --

/// Why: THE defect. Pre-#5052 `auth_middleware` exempted `/api/events` before
/// comparing anything, and on the default (token-less) config the middleware was
/// not even installed — so this request streamed live conversation content to
/// anyone who could open a socket, including a cross-origin `EventSource` in the
/// operator's own browser. This test returns 200 against the pre-fix commit.
/// Test: this test.
#[tokio::test]
async fn event_stream_without_credential_is_rejected() {
    assert_eq!(
        status_of(&default_router(), get_events("")).await,
        StatusCode::UNAUTHORIZED,
    );
}

/// Why: the same request against a token-guarded server — the case #5052 was
/// filed for. A configured token must actually cover the stream.
/// Test: this test.
#[tokio::test]
async fn event_stream_without_credential_is_rejected_with_token_configured() {
    assert_eq!(
        status_of(&token_router("secret"), get_events("")).await,
        StatusCode::UNAUTHORIZED,
    );
}

/// Why: an attacker who guesses at the parameter must not get in, and the 401
/// must carry the same body shape as `auth_middleware`'s so clients can treat
/// the two identically.
/// Test: this test.
#[tokio::test]
async fn event_stream_with_unknown_ticket_is_rejected() {
    let app = default_router();
    let resp = app
        .clone()
        .oneshot(get_events("?ticket=not-a-real-ticket"))
        .await
        .expect("router responds");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024)
        .await
        .expect("body reads");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    assert_eq!(v["error"], "unauthorized");
}

// -- the mechanism: a minted ticket opens the stream --

/// Why: the fix must not break the shipped UI. A browser mints a ticket and
/// opens `EventSource` with it; that path must reach a real `text/event-stream`.
/// Test: this test.
#[tokio::test]
async fn minted_ticket_opens_event_stream() {
    let app = default_router();
    let ticket = mint_ticket(&app, None).await;

    let resp = app
        .clone()
        .oneshot(get_events(&format!("?ticket={ticket}")))
        .await
        .expect("router responds");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream"),
    );
    // The body is an endless stream — never read it, just drop the connection.
    drop(resp);
}

/// Why: `EventSource` reconnects to the SAME url after a dropped connection, so
/// a single-use ticket would turn any transient blip into a permanently dead
/// stream. Redemption must be repeatable.
/// Test: this test.
#[tokio::test]
async fn minted_ticket_is_redeemable_more_than_once() {
    let app = default_router();
    let ticket = mint_ticket(&app, None).await;
    let query = format!("?ticket={ticket}");
    for attempt in 0..3 {
        assert_eq!(
            status_of(&app, get_events(&query)).await,
            StatusCode::OK,
            "reconnect {attempt} must succeed"
        );
    }
}

/// Why: programmatic clients (`curl`, a reverse proxy, the desktop shell's Rust
/// SSE bridge) CAN set headers, so a configured bearer token must open the
/// stream directly without a ticket round-trip.
/// Test: this test.
#[tokio::test]
async fn bearer_token_opens_event_stream() {
    let app = token_router("secret");
    let req = Request::builder()
        .uri("/api/events")
        .header(header::AUTHORIZATION, "Bearer secret")
        .body(Body::empty())
        .expect("request builds");
    assert_eq!(status_of(&app, req).await, StatusCode::OK);
}

/// Why: a ticket carries only stream-read authority; it must never be usable as
/// a substitute for the bearer token on the write surface.
/// Test: this test.
#[tokio::test]
async fn ticket_does_not_unlock_the_rest_of_the_api() {
    let app = token_router("secret");
    let ticket = mint_ticket(&app, Some("secret")).await;
    let req = Request::builder()
        .uri(format!("/api/tasks?ticket={ticket}"))
        .body(Body::empty())
        .expect("request builds");
    assert_eq!(status_of(&app, req).await, StatusCode::UNAUTHORIZED);
}

// -- the ticket path on a TOKEN-CONFIGURED server (#5052, critic HIGH) --
//
// Every success case above uses `default_router()`, which has no token — so
// none of them exercised `auth_middleware`, which axum layers OUTSIDE the
// handler. `bearer_token_opens_event_stream` passed because the HEADER cleared
// that middleware, not because the ticket path worked. It did not: the server
// minted tickets it would then always 401. And since `serve_with_config`
// refuses a non-loopback bind without a token, a token-configured server is the
// ONLY way to run off-loopback — the deployment #5052 was filed about, where
// the browser stream was therefore dead. These are the cases that were missing.

/// Why: THE critic HIGH. A browser holding a valid ticket and no
/// `Authorization` header — the only thing `EventSource` can do — must reach
/// the stream on a token-configured server. Fails with 401 against the
/// pre-fix head, where `auth_middleware` rejected it before `EventStreamAuth`
/// ever ran.
/// Test: this test.
#[tokio::test]
async fn ticket_opens_event_stream_on_a_token_configured_server() {
    let app = token_router("secret");
    // The ticket is minted WITH the bearer (the UI holds the token); the stream
    // request deliberately carries no header at all.
    let ticket = mint_ticket(&app, Some("secret")).await;
    assert_eq!(
        status_of(&app, get_events(&format!("?ticket={ticket}"))).await,
        StatusCode::OK,
    );
}

/// Why: the exemption that makes the ticket reachable must not weaken the
/// negative case — a bogus ticket and no header is still 401 with a token set.
/// Test: this test.
#[tokio::test]
async fn bogus_ticket_is_rejected_on_a_token_configured_server() {
    assert_eq!(
        status_of(&token_router("secret"), get_events("?ticket=forged")).await,
        StatusCode::UNAUTHORIZED,
    );
}

/// Why: an EXPIRED ticket must not survive the exemption either. Exercised
/// against `EventStreamAuth` directly — the router hides its ticket store in a
/// request extension, and the decision under test is the auth one, not routing.
/// Test: this test.
#[test]
fn expired_ticket_is_rejected_by_stream_auth_with_a_token_configured() {
    let auth = EventStreamAuth::new(Some("secret".to_string()));
    let ticket = auth.mint().ticket;
    let no_headers = HeaderMap::new();

    assert!(auth.authorize(&no_headers, Some(&ticket)), "fresh ticket");
    auth.store().expire_all_for_test();
    assert!(
        !auth.authorize(&no_headers, Some(&ticket)),
        "expired ticket must be refused even though a token is configured"
    );

    // The bearer path is unaffected by ticket expiry.
    let mut bearer = HeaderMap::new();
    bearer.insert(
        header::AUTHORIZATION,
        "Bearer secret".parse().expect("header value"),
    );
    assert!(auth.authorize(&bearer, None));
    // And a wrong bearer with no ticket is still refused.
    let mut wrong = HeaderMap::new();
    wrong.insert(
        header::AUTHORIZATION,
        "Bearer wrong!".parse().expect("header value"),
    );
    assert!(!auth.authorize(&wrong, None));
}

// -- the mint endpoint's own gates --

/// Why: with a token configured, minting must require it — otherwise the ticket
/// endpoint is a credential-laundering bypass of `auth_middleware`.
/// Test: this test.
#[tokio::test]
async fn mint_route_requires_bearer_token_when_configured() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/events/ticket")
        .body(Body::empty())
        .expect("request builds");
    assert_eq!(
        status_of(&token_router("secret"), req).await,
        StatusCode::UNAUTHORIZED,
    );
}

/// Why: SECURITY — this is what closes the DEFAULT (token-less) config. Minting
/// is a POST, so the router-wide same-origin write guard rejects a cross-origin
/// caller; a page on `https://evil.example` therefore cannot obtain a ticket,
/// and without one its `EventSource` is refused by the stream route.
/// Test: this test.
#[tokio::test]
async fn mint_route_rejects_cross_origin_caller() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/events/ticket")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::empty())
        .expect("request builds");
    assert_eq!(
        status_of(&default_router(), req).await,
        StatusCode::FORBIDDEN,
    );
}

/// Why: the daemon's own SPA and a `pnpm dev` Vite server are loopback origins
/// and must keep minting successfully.
/// Test: this test.
#[tokio::test]
async fn mint_route_allows_loopback_origin() {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/events/ticket")
        .header(header::ORIGIN, "http://localhost:5173")
        .body(Body::empty())
        .expect("request builds");
    assert_eq!(status_of(&default_router(), req).await, StatusCode::OK);
}

// -- the CORS half of the fix --

/// Why: SECURITY (#5052) — even holding a ticket, a page on a remote origin must
/// not be able to READ the response, because the browser only exposes a
/// cross-origin body when the server reflects the origin. `allow_origin(Any)`
/// did reflect it; the same-origin policy does not.
/// Test: this test.
#[tokio::test]
async fn cross_origin_request_gets_no_cors_reflection() {
    let app = default_router();
    let req = Request::builder()
        .uri("/api/tasks")
        .header(header::ORIGIN, "https://evil.example")
        .body(Body::empty())
        .expect("request builds");
    let resp = app.clone().oneshot(req).await.expect("router responds");
    assert!(
        resp.headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "a remote origin must not be reflected"
    );
}

/// Why: the tightened policy must not break the local browser surfaces — the
/// daemon's own SPA on `127.0.0.1` and the Vite dev server on `localhost:5173`.
/// Test: this test.
#[tokio::test]
async fn loopback_origin_is_cors_reflected() {
    let app = default_router();
    for origin in ["http://localhost:5173", "http://127.0.0.1:7654"] {
        let req = Request::builder()
            .uri("/api/tasks")
            .header(header::ORIGIN, origin)
            .body(Body::empty())
            .expect("request builds");
        let resp = app.clone().oneshot(req).await.expect("router responds");
        assert_eq!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some(origin),
            "{origin} must be reflected"
        );
    }
}

// -- the store, directly --

/// Why: a guessable ticket is no credential at all.
/// Test: this test.
#[test]
fn mint_returns_distinct_unguessable_tickets() {
    let store = EventTicketStore::default();
    let a = store.mint().ticket;
    let b = store.mint().ticket;
    assert_ne!(a, b);
    // 32 random bytes, URL-safe base64 without padding.
    assert_eq!(a.len(), 43);
    assert!(
        a.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "ticket must survive a query string unescaped: {a}"
    );
}

/// Why: an expired ticket recovered from an access log must be dead.
/// Test: this test.
#[test]
fn redeem_rejects_expired_ticket() {
    let store = EventTicketStore::default();
    let ticket = store.mint().ticket;
    assert!(store.redeem(&ticket));
    store.expire_all_for_test();
    assert!(!store.redeem(&ticket));
    assert_eq!(store.len(), 0, "expired tickets are pruned");
}

/// Why: a client minting in a loop must not grow the store without bound.
/// Test: this test.
#[test]
fn store_is_capacity_bounded() {
    let store = EventTicketStore::default();
    for _ in 0..200 {
        let _ = store.mint();
    }
    assert!(store.len() <= 32, "live tickets: {}", store.len());
}

/// Why: SECURITY (#5052, critic MEDIUM-1) — the sliding window must not be able
/// to renew a ticket past its absolute lifetime, or a ticket recovered from an
/// access log and polled every few minutes becomes a permanent credential.
/// Test: this test.
#[test]
fn sliding_ttl_cannot_outlive_the_absolute_cap() {
    let store = EventTicketStore::default();
    let ticket = store.mint().ticket;
    // Redeem once (which slides the idle window), then push only the ABSOLUTE
    // deadline into the past while leaving the idle window wide open.
    assert!(store.redeem(&ticket));
    store.expire_hard_deadline_for_test();
    assert!(
        !store.redeem(&ticket),
        "a refreshed idle window must not outlive MAX_TICKET_LIFETIME"
    );
    assert_eq!(store.len(), 0, "hard-expired tickets are pruned");
}

/// Why: the obvious negative case — an unrelated string is not a ticket.
/// Test: this test.
#[test]
fn redeem_rejects_unknown_ticket() {
    let store = EventTicketStore::default();
    let ticket = store.mint().ticket;
    assert!(!store.redeem(""));
    assert!(!store.redeem("nope"));
    // A one-character truncation must not match either.
    assert!(!store.redeem(&ticket[..ticket.len() - 1]));
}
