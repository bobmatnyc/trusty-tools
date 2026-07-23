//! Tests for the internal Slack-mirror relay endpoint (#3752).
//!
//! Why: `POST /api/internal/relay-event` injects events onto the GUI bus from
//! the separate `tagent --slack` process. Its contract is narrow and
//! security-relevant: accept only the two `Slack*` mirror kinds, publish them
//! so `/api/events` subscribers see them, and reject everything else (a forged
//! task/agent event, or a malformed body). These cases lock that contract.
//! What: oneshots the endpoint on a default (loopback-only) router — the guard
//! fails open on the absent `Origin` header, mirroring the real server-to-server
//! POST from the loopback Slack gateway.
//! Test: this module IS the test.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use tower::ServiceExt;

use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;
use crate::events;

/// POST a raw JSON body to the relay endpoint and return the status.
async fn post_relay(json_body: &str) -> StatusCode {
    let app = build_router(AppState::default());
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/internal/relay-event")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json_body.to_string()))
        .unwrap();
    app.oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn relay_accepts_slack_event() {
    // Subscribe BEFORE the request so we can confirm the handler actually
    // published the event onto the process bus (not merely returned 202).
    let mut rx = events::subscribe();

    // The bus is process-global and other tests publish onto it concurrently,
    // so key on a unique channel id and drain to find our injected event
    // rather than assuming it is the very next message.
    let unique_channel = "C_RELAY_ACCEPT_TEST";
    let body = serde_json::to_string(&events::Event::SlackMessageReceived {
        channel: unique_channel.into(),
        user_display: "Masa".into(),
        text: "deploy status?".into(),
        tier: "all".into(),
    })
    .unwrap();

    assert_eq!(post_relay(&body).await, StatusCode::ACCEPTED);

    // Publish is synchronous, so by the time `oneshot` returned our event is
    // already queued. Drain everything currently buffered and confirm ours is
    // present with the honest tier badge intact.
    let mut found = false;
    while let Ok(ev) = rx.try_recv() {
        if let events::Event::SlackMessageReceived {
            channel,
            user_display,
            tier,
            ..
        } = ev
            && channel == unique_channel
        {
            assert_eq!(user_display, "Masa");
            assert_eq!(tier, "all");
            found = true;
        }
    }
    assert!(
        found,
        "relay did not publish the injected event onto the bus"
    );
}

#[tokio::test]
async fn relay_rejects_non_slack_kind() {
    // A well-formed, non-Slack event kind is rejected by the whitelist so the
    // GUI can't be fed forged task/agent telemetry through this surface.
    let body = serde_json::to_string(&events::Event::Ping).unwrap();
    assert_eq!(post_relay(&body).await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn relay_rejects_malformed_body() {
    // An unknown `type` tag never deserializes to `Event`; axum's `Json`
    // extractor rejects it before the handler runs (422). Either way it is a
    // client error, never a 2xx.
    let status = post_relay(r#"{"type":"not_a_real_kind","x":1}"#).await;
    assert!(status.is_client_error(), "expected 4xx, got {status}");
}

#[tokio::test]
async fn relay_rejects_non_json_body() {
    let status = post_relay("this is not json").await;
    assert!(status.is_client_error(), "expected 4xx, got {status}");
}
