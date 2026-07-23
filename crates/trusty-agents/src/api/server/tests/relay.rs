//! Tests for the internal Slack-mirror relay endpoint (#3752).
//!
//! Why: `POST /api/internal/relay-event` injects events onto the GUI bus from
//! the separate `tagent --slack` process. Its contract is narrow and
//! security-relevant: it is the load-bearing honesty control for the mirror
//! pane, so it must FAIL CLOSED without the shared secret, reject a wrong/
//! missing token, accept only with a matching token, then accept only the two
//! `Slack*` kinds with a known RBAC tier. These cases lock that contract.
//! What: oneshots the endpoint on a default (loopback-only) router — the origin
//! guard fails open on the absent `Origin` header (the real server-to-server
//! posture), so the shared-secret gate is what actually protects the route.
//! `TAGENT_RELAY_TOKEN` is a process-global env var, so every test that sets it
//! holds `RELAY_ENV_LOCK` for its whole body (the parent `tests` module allows
//! `await_holding_lock`), and only relay tests read this var.
//! Test: this module IS the test.

use std::sync::Mutex;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

use crate::api::server::relay::{relay_authorized, tier_is_known};
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;
use crate::events;

/// Serializes tests that mutate `TAGENT_RELAY_TOKEN` (process-global env).
static RELAY_ENV_LOCK: Mutex<()> = Mutex::new(());

const RELAY_TOKEN_ENV: &str = "TAGENT_RELAY_TOKEN";
const RELAY_TOKEN_HEADER: &str = "x-relay-token";

/// Set (`Some`) or clear (`None`) the server-side relay secret.
///
/// SAFETY: every caller holds `RELAY_ENV_LOCK` for its whole body and only
/// relay tests read this var, so no concurrent get/set can race.
fn set_relay_env(val: Option<&str>) {
    unsafe {
        match val {
            Some(v) => std::env::set_var(RELAY_TOKEN_ENV, v),
            None => std::env::remove_var(RELAY_TOKEN_ENV),
        }
    }
}

/// POST a raw JSON body to the relay endpoint (optionally with an
/// `x-relay-token` header) and return the response status.
async fn post_relay(json_body: &str, token_header: Option<&str>) -> StatusCode {
    let app = build_router(AppState::default());
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/api/internal/relay-event")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token_header {
        builder = builder.header(RELAY_TOKEN_HEADER, t);
    }
    let req = builder.body(Body::from(json_body.to_string())).unwrap();
    app.oneshot(req).await.unwrap().status()
}

fn inbound_body(tier: &str) -> String {
    serde_json::to_string(&events::Event::SlackMessageReceived {
        channel: "C0123ABC".into(),
        user_display: "Masa".into(),
        text: "deploy status?".into(),
        tier: tier.into(),
    })
    .unwrap()
}

// -- shared-secret gate (fail closed) ---------------------------------------

#[tokio::test]
async fn relay_rejects_when_token_unset_server_side() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(None); // secret not configured → reject everything.
    // Even a well-formed event carrying *some* token is rejected.
    let status = post_relay(&inbound_body("all"), Some("anything")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn relay_rejects_missing_token() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));
    let status = post_relay(&inbound_body("all"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    set_relay_env(None);
}

#[tokio::test]
async fn relay_rejects_wrong_token() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));
    let status = post_relay(&inbound_body("all"), Some("nope")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    set_relay_env(None);
}

// -- accept path + payload validation (with a matching token) ---------------

#[tokio::test]
async fn relay_accepts_with_matching_token() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));

    // Subscribe BEFORE the request to confirm the handler actually published.
    let mut rx = events::subscribe();
    let unique_channel = "C_RELAY_ACCEPT_TEST";
    let body = serde_json::to_string(&events::Event::SlackMessageReceived {
        channel: unique_channel.into(),
        user_display: "Masa".into(),
        text: "deploy status?".into(),
        tier: "all".into(),
    })
    .unwrap();

    assert_eq!(
        post_relay(&body, Some("s3cret")).await,
        StatusCode::ACCEPTED
    );

    // The bus is process-global, so drain and find our unique event.
    let mut found = false;
    while let Ok(ev) = rx.try_recv() {
        if let events::Event::SlackMessageReceived { channel, tier, .. } = ev
            && channel == unique_channel
        {
            assert_eq!(tier, "all");
            found = true;
        }
    }
    assert!(
        found,
        "relay did not publish the injected event onto the bus"
    );
    set_relay_env(None);
}

#[tokio::test]
async fn relay_rejects_non_slack_kind() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));
    // A well-formed, non-Slack event kind is rejected by the whitelist so the
    // GUI can't be fed forged task/agent telemetry through this surface.
    let body = serde_json::to_string(&events::Event::Ping).unwrap();
    assert_eq!(
        post_relay(&body, Some("s3cret")).await,
        StatusCode::BAD_REQUEST
    );
    set_relay_env(None);
}

#[tokio::test]
async fn relay_rejects_unknown_tier() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));
    // A tier outside the closed ServiceTier set must not become a badge.
    let status = post_relay(&inbound_body("root"), Some("s3cret")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    set_relay_env(None);
}

#[tokio::test]
async fn relay_rejects_malformed_body() {
    let _g = RELAY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_relay_env(Some("s3cret"));
    // An unknown `type` tag never deserializes to `Event`; axum's `Json`
    // extractor rejects it (422) before the handler runs. Either way it is a
    // client error, never a 2xx.
    let status = post_relay(r#"{"type":"not_a_real_kind","x":1}"#, Some("s3cret")).await;
    assert!(status.is_client_error(), "expected 4xx, got {status}");
    set_relay_env(None);
}

// -- pure helpers -----------------------------------------------------------

#[test]
fn relay_authorized_fails_closed_when_unset() {
    // No secret configured server-side → deny, regardless of what's provided.
    assert!(!relay_authorized(None, Some("anything")));
    assert!(!relay_authorized(None, None));
    assert!(!relay_authorized(Some(""), Some("")));
}

#[test]
fn relay_authorized_requires_exact_match() {
    assert!(relay_authorized(Some("abc"), Some("abc")));
    assert!(!relay_authorized(Some("abc"), Some("abcd")));
    assert!(!relay_authorized(Some("abc"), None));
}

#[test]
fn tier_is_known_accepts_the_closed_set() {
    assert!(tier_is_known("all"));
    assert!(tier_is_known("analytics"));
    assert!(tier_is_known("read_only"));
}

#[test]
fn tier_is_known_rejects_unknown() {
    assert!(!tier_is_known("root"));
    assert!(!tier_is_known(""));
    assert!(!tier_is_known("ALL"));
}
