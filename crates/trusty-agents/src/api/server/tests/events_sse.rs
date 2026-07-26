//! Tests for the `/api/events` SSE delivery filter (#192 Phase B, #3760).
//!
//! Why: The Slack conversation mirror leaked across clients — `SlackMessageReceived`
//! / `SlackReplySent` carry no `session_id`, so the pre-#3760 filter read them as
//! "always include" and every connected `/api/events` subscriber received every
//! channel's message text. These cases lock the new `channel` scope AND, just as
//! importantly, lock the compatibility case: a client that sends no filters must
//! still receive everything, because that is exactly what the shipped
//! single-operator GUI does.
//! What: Exercises `events_sse::event_passes`, the pure predicate the stream body
//! calls per bus event, rather than driving a live SSE connection — the filtering
//! decision is the whole contract and it is directly observable here.
//! Test: this module IS the test.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::api::server::events_sse::event_passes;
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;
use crate::events::Event;

fn inbound(channel: &str) -> Event {
    Event::SlackMessageReceived {
        channel: channel.into(),
        user_display: "Masa".into(),
        text: "deploy status?".into(),
        tier: "all".into(),
    }
}

fn reply(channel: &str) -> Event {
    Event::SlackReplySent {
        channel: channel.into(),
        text: "green".into(),
        identity: crate::slack::BOT_IDENTITY.into(),
    }
}

fn listener_event() -> Event {
    Event::ListenerEventReceived {
        listener_id: "slack".into(),
        provider: "slack".into(),
        event_type: "message".into(),
        summary: "Masa: deploy status?".into(),
        included: true,
    }
}

fn task_event(session_id: &str) -> Event {
    Event::PmThinking {
        session_id: session_id.into(),
        text: "planning".into(),
    }
}

// -- compatibility: no filters means no filtering -----------------------------

#[test]
fn event_passes_without_filters() {
    // The shipped GUI opens `/api/events` with no query params at all. Every
    // event kind must still reach it — #3760 must not narrow the default.
    for ev in [
        inbound("C0123ABC"),
        reply("C0123ABC"),
        listener_event(),
        task_event("sess-1"),
        Event::Ping,
    ] {
        assert!(
            event_passes(&ev, None, None),
            "unfiltered subscriber must receive {ev:?}"
        );
    }
}

// -- #3760: channel scoping ---------------------------------------------------

#[test]
fn event_passes_scopes_slack_by_channel() {
    // A client scoped to one channel sees that channel's BOTH halves...
    assert!(event_passes(&inbound("C0123ABC"), None, Some("C0123ABC")));
    assert!(event_passes(&reply("C0123ABC"), None, Some("C0123ABC")));
    // ...and none of another channel's — this is the leak #3760 reported.
    assert!(!event_passes(&inbound("C_OTHER"), None, Some("C0123ABC")));
    assert!(!event_passes(&reply("C_OTHER"), None, Some("C0123ABC")));
}

#[test]
fn event_passes_leaves_non_slack_events_alone() {
    // Events that carry no channel always pass a channel filter. Suppressing
    // keepalives would kill the connection; suppressing task telemetry or the
    // provider-generic listener stream would silently blank unrelated panes.
    // `ListenerEventReceived` is deliberately NOT channel-scoped (see the doc
    // comment on `Event::slack_channel`): it has no channel to scope by.
    assert!(event_passes(&Event::Ping, None, Some("C0123ABC")));
    assert!(event_passes(&task_event("sess-1"), None, Some("C0123ABC")));
    assert!(event_passes(&listener_event(), None, Some("C0123ABC")));
}

// -- pre-existing session scoping still holds ---------------------------------

#[test]
fn event_passes_still_scopes_by_session() {
    assert!(event_passes(&task_event("sess-1"), Some("sess-1"), None));
    assert!(!event_passes(&task_event("sess-2"), Some("sess-1"), None));
    // A session filter must never suppress keepalives.
    assert!(event_passes(&Event::Ping, Some("sess-1"), None));
    // ...nor the Slack mirror, which has no session to match against: the two
    // filter dimensions are independent, so a session-scoped client that wants
    // no Slack traffic scopes the channel too.
    assert!(event_passes(&inbound("C0123ABC"), Some("sess-1"), None));
}

// -- #3760: a mistyped filter must fail LOUD, never fail open ----------------

/// Open `/api/events` with the given query string and return the status.
///
/// The SSE handler streams forever once it is entered, so we only ever assert
/// on the response STATUS here — reaching a 200 means the `Query<EventsQuery>`
/// extractor accepted the parameters, which is the whole contract under test.
async fn get_events(query: &str) -> StatusCode {
    let app = build_router(AppState::default());
    let req = Request::builder()
        .uri(format!("/api/events{query}"))
        .body(Body::empty())
        .unwrap();
    app.oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn events_query_rejects_unknown_parameter() {
    // Every field on `EventsQuery` is a security filter whose ABSENT form fails
    // open (unfiltered stream). Without `deny_unknown_fields` a typo'd
    // `?channel_id=` would silently hand back the full firehose the caller was
    // trying to narrow — the #3760 leak, reintroduced by a spelling mistake and
    // invisible at both ends.
    assert!(
        get_events("?channel_id=C0123ABC").await.is_client_error(),
        "a typo'd filter parameter must be rejected, not silently ignored"
    );
    assert!(get_events("?sessionId=sess-1").await.is_client_error());
    assert!(
        get_events("?channel=C0123ABC&bogus=1")
            .await
            .is_client_error()
    );
}

#[tokio::test]
async fn events_query_accepts_known_parameters() {
    // The compatibility surface: no params, each param alone, and both
    // together must all be accepted. No shipped client sends anything else
    // (transport.ts sends `session_id` or nothing; sse_bridge.rs sends nothing).
    for q in [
        "",
        "?session_id=sess-1",
        "?channel=C0123ABC",
        "?session_id=sess-1&channel=C0123ABC",
    ] {
        assert_eq!(
            get_events(q).await,
            StatusCode::OK,
            "query {q:?} must be accepted"
        );
    }
}

#[test]
fn event_passes_applies_both_filters_independently() {
    // Both set: each event is judged only by the key it actually carries.
    assert!(event_passes(
        &task_event("sess-1"),
        Some("sess-1"),
        Some("C0123ABC")
    ));
    assert!(!event_passes(
        &task_event("sess-2"),
        Some("sess-1"),
        Some("C0123ABC")
    ));
    assert!(event_passes(
        &reply("C0123ABC"),
        Some("sess-1"),
        Some("C0123ABC")
    ));
    assert!(!event_passes(
        &reply("C_OTHER"),
        Some("sess-1"),
        Some("C0123ABC")
    ));
}
