//! Server-Sent Events stream for real-time telemetry (#192 Phase B).
//!
//! Why: Replaces the 2-second `setInterval` poll the UI used to hit
//! `/api/tasks`. SSE keeps a single long-lived HTTP connection open and pushes
//! events the instant the back-end emits them, cutting perceived latency from
//! "up to 2 s" to "≤ network RTT" while reducing request load.
//! What: Subscribes to the process-global `events::bus`, optionally filters to
//! a single `session_id` and/or a single Slack `channel`, and yields each event
//! as an SSE frame with periodic keepalive pings.
//! Test: After `cargo run -- --api --port 7654 &`, `curl -N
//! http://localhost:7654/api/events` streams events as tasks execute; the pure
//! filter predicate is covered by `super::tests::events_sse`.

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    Extension, Json,
    extract::Query,
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event as SseEvent, KeepAlive, Sse},
    },
};
use serde::Deserialize;
use tokio_stream::Stream;

use super::event_tickets::EventStreamAuth;
use crate::events::{self, Event};

/// Query string for `GET /api/events`. (#192 Phase B, #3760)
///
/// Why: Lets a single-task UI subscribe only to events for its session
/// without filtering N other concurrent tasks client-side. When omitted,
/// the stream emits every event the server's bus produces.
/// What: Optional `session_id` filter and optional Slack `channel` filter
/// (#3760). Both are independent and both default to "no filter", so an
/// existing client that passes neither keeps receiving every event.
///
/// `deny_unknown_fields` (#3760): every field here is a SECURITY filter whose
/// absent form fails OPEN (unfiltered stream). Without it a client that typo'd
/// `?channel_id=C123` would silently get the full firehose it was trying to
/// narrow — the exact leak this parameter exists to close, reintroduced by a
/// spelling mistake and invisible at both ends. Rejecting the request instead
/// makes the mistake loud. Safe to add: no shipped client sends any other
/// parameter (`ui/src/lib/transport.ts` sends `session_id` or nothing;
/// `ui/src-tauri/src/sse_bridge.rs` sends nothing).
/// Test: `event_passes_*` in `super::tests::events_sse`;
/// `events_query_rejects_unknown_parameter`,
/// `events_query_accepts_known_parameters`; end-to-end via the live
/// `/api/events` stream (integration).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EventsQuery {
    session_id: Option<String>,
    /// #3760: Slack channel id to scope the Slack conversation mirror to.
    channel: Option<String>,
    /// #5052: the stream credential a browser `EventSource` can carry, since it
    /// cannot set an `Authorization` header. Minted at `POST
    /// /api/events/ticket`. Programmatic clients send a bearer header instead
    /// and leave this absent.
    ticket: Option<String>,
}

/// #3760: Whether one bus event should be delivered to a subscriber holding
/// the given filters.
///
/// Why: The Slack-mirror events carry no `session_id`, so the pre-existing
/// session filter read them as "always include" and every connected
/// `/api/events` client received every channel's message text — a leak the
/// moment more than one viewer is attached. Splitting the predicate out of the
/// stream body makes both filter dimensions directly testable instead of only
/// reachable through a live SSE connection.
/// What: Each filter is applied only to events that expose the corresponding
/// key. An event whose key is `None` (a `Ping`, or any non-Slack event under a
/// `channel` filter) always passes — keepalives and task telemetry must not be
/// suppressed by a Slack-scoping request. Both filters absent ⇒ everything
/// passes, which is the pre-#3760 behaviour and what the single-operator GUI
/// relies on.
/// Test: `event_passes_without_filters`, `event_passes_scopes_slack_by_channel`,
/// `event_passes_leaves_non_slack_events_alone`,
/// `event_passes_still_scopes_by_session`.
pub(super) fn event_passes(
    event: &Event,
    session_filter: Option<&str>,
    channel_filter: Option<&str>,
) -> bool {
    if let Some(sid) = session_filter
        && let Some(ev_sid) = event.session_id()
        && ev_sid != sid
    {
        return false;
    }
    if let Some(chan) = channel_filter
        && let Some(ev_chan) = event.slack_channel()
        && ev_chan != chan
    {
        return false;
    }
    true
}

/// `GET /api/events?session_id=<optional>&channel=<optional>` — Server-Sent
/// Events stream of real-time PM/agent/workflow telemetry. (#192 Phase B,
/// #3760)
///
/// Why: Replaces the 2-second `setInterval` poll the UI used to hit
/// `/api/tasks` with. SSE keeps a single long-lived HTTP connection open and
/// pushes events the instant the back-end emits them, cutting perceived
/// latency from "up to 2 s" to "≤ network RTT" while reducing request load.
/// What: Subscribes to the process-global `events::bus`, optionally filters
/// to a single `session_id` and/or a single Slack `channel` (#3760 — absent
/// means "no filter", so existing single-operator clients are unchanged), and
/// yields each surviving event as `event: event\ndata:
/// <json>\n\n`. Emits `event: ping\ndata: {}` every 15 s as a keepalive so
/// reverse proxies and mobile networks don't reap idle connections. On
/// `RecvError::Lagged(n)` (slow subscriber), yields one `event: lag\ndata:
/// {"skipped":<n>}` notice and resumes — never silently drops the stream.
/// #5052: the connection is NO LONGER exempt from auth. A caller must present
/// either a matching `Authorization: Bearer <token>` (programmatic clients) or a
/// live `?ticket=` minted at `POST /api/events/ticket` (browsers, whose
/// `EventSource` cannot set headers); anything else gets 401. This holds even
/// when no bearer token is configured, because the shipped default — the Tauri
/// sidecar on `127.0.0.1:7654` with no token — is precisely the configuration
/// the cross-origin `EventSource` disclosure targeted.
/// Test: `cargo run -- --api --port 7654 &`, then
/// `T=$(curl -sX POST localhost:7654/api/events/ticket | jq -r .ticket)` and
/// `curl -N "http://localhost:7654/api/events?ticket=$T"`; the same `curl -N`
/// without a ticket returns 401. Unit coverage:
/// `super::tests::event_tickets`.
pub(super) async fn events_handler(
    Extension(auth): Extension<EventStreamAuth>,
    headers: HeaderMap,
    Query(params): Query<EventsQuery>,
) -> Response {
    // #5052: authenticate before subscribing to the bus, so an unauthorized
    // caller never holds a receiver on a channel carrying conversation content.
    if !auth.authorize(&headers, params.ticket.as_deref()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response();
    }
    event_stream(params).into_response()
}

/// `POST /api/events/ticket` — issue a short-lived credential for the SSE
/// stream. (#5052)
///
/// Why: the browser needs a credential it can put in an `EventSource` URL, and
/// this is the authenticated endpoint that hands one out. It is a POST, and
/// therefore already behind the router-wide same-origin write guard
/// (`trusty_common::server::guard_write_origin`) — that is what closes the
/// default, token-less configuration: a page on `https://evil.example` gets a
/// 403 here and so can never obtain a ticket, whatever it does with
/// `EventSource` afterwards. When a bearer token IS configured, `auth_middleware`
/// gates this route on top, since `/api/events/ticket` is not an exempt path.
/// What: mints a ticket and returns `{"ticket": "<opaque>", "expires_in_secs":
/// 300}`.
/// Test: `event_tickets::mint_route_requires_bearer_token_when_configured`,
/// `event_tickets::mint_route_rejects_cross_origin_caller`.
pub(super) async fn mint_event_ticket(Extension(auth): Extension<EventStreamAuth>) -> Response {
    Json(auth.mint()).into_response()
}

/// The SSE body itself, split out so [`events_handler`] can return either it or
/// a 401 from one function without the stream type leaking into that signature.
fn event_stream(params: EventsQuery) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let mut rx = events::subscribe();
    let session_filter = params.session_id;
    // #3760: optional Slack-channel scope for the conversation mirror.
    let channel_filter = params.channel;

    // `async_stream::stream!` lets us write linear-looking code that yields
    // SSE events; the macro lowers it to a poll-based `Stream`.
    let s = async_stream::stream! {
        let mut keepalive = tokio::time::interval(Duration::from_secs(15));
        // Skip the immediate first tick — `interval` fires once at t=0.
        keepalive.tick().await;
        loop {
            tokio::select! {
                _ = keepalive.tick() => {
                    yield Ok(SseEvent::default().event("ping").data("{}"));
                }
                result = rx.recv() => {
                    match result {
                        Ok(event) => {
                            // Filter by session_id and/or Slack channel when the
                            // client requested them. `Ping` (no session, no channel)
                            // always passes — keepalives must reach every subscriber.
                            // #3760: the channel dimension is what keeps a Slack
                            // mirror from fanning out to every connected viewer.
                            if !event_passes(
                                &event,
                                session_filter.as_deref(),
                                channel_filter.as_deref(),
                            ) {
                                continue;
                            }
                            let data = serde_json::to_string(&event)
                                .unwrap_or_else(|_| "{}".to_string());
                            yield Ok(SseEvent::default().event("event").data(data));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            yield Ok(SseEvent::default()
                                .event("lag")
                                .data(format!("{{\"skipped\":{n}}}")));
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };

    // Axum's KeepAlive layer is redundant with our explicit ping but harmless
    // — it sends a comment line if no traffic flows for the default 15 s,
    // giving us double-protection against idle-connection reaping.
    Sse::new(s).keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
}
