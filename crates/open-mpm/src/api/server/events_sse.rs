//! Server-Sent Events stream for real-time telemetry (#192 Phase B).
//!
//! Why: Replaces the 2-second `setInterval` poll the UI used to hit
//! `/api/tasks`. SSE keeps a single long-lived HTTP connection open and pushes
//! events the instant the back-end emits them, cutting perceived latency from
//! "up to 2 s" to "≤ network RTT" while reducing request load.
//! What: Subscribes to the process-global `events::bus`, optionally filters to
//! a single `session_id`, and yields each event as an SSE frame with periodic
//! keepalive pings.
//! Test: After `cargo run -- --api --port 7654 &`, `curl -N
//! http://localhost:7654/api/events` streams events as tasks execute.

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::Query,
    response::sse::{Event as SseEvent, KeepAlive, Sse},
};
use serde::Deserialize;
use tokio_stream::Stream;

use crate::events;

/// Query string for `GET /api/events`. (#192 Phase B)
///
/// Why: Lets a single-task UI subscribe only to events for its session
/// without filtering N other concurrent tasks client-side. When omitted,
/// the stream emits every event the server's bus produces.
/// What: Optional `session_id` filter.
/// Test: Driven via the live `/api/events` stream (integration).
#[derive(Debug, Deserialize)]
pub(super) struct EventsQuery {
    session_id: Option<String>,
}

/// `GET /api/events?session_id=<optional>` — Server-Sent Events stream of
/// real-time PM/agent/workflow telemetry. (#192 Phase B)
///
/// Why: Replaces the 2-second `setInterval` poll the UI used to hit
/// `/api/tasks` with. SSE keeps a single long-lived HTTP connection open and
/// pushes events the instant the back-end emits them, cutting perceived
/// latency from "up to 2 s" to "≤ network RTT" while reducing request load.
/// What: Subscribes to the process-global `events::bus`, optionally filters
/// to a single `session_id`, and yields each event as `event: event\ndata:
/// <json>\n\n`. Emits `event: ping\ndata: {}` every 15 s as a keepalive so
/// reverse proxies and mobile networks don't reap idle connections. On
/// `RecvError::Lagged(n)` (slow subscriber), yields one `event: lag\ndata:
/// {"skipped":<n>}` notice and resumes — never silently drops the stream.
/// Test: After `cargo run -- --api --port 7654 &`, run
/// `curl -N http://localhost:7654/api/events` and watch events stream as
/// tasks execute. The connection is exempt from Bearer auth (see
/// `auth_middleware`).
pub(super) async fn events_handler(
    Query(params): Query<EventsQuery>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let mut rx = events::subscribe();
    let filter = params.session_id;

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
                            // Filter by session_id when the client requested one.
                            // `Ping` (session_id == None) always passes — keepalives
                            // must reach every subscriber.
                            if let Some(ref sid) = filter
                                && let Some(ev_sid) = event.session_id()
                                && ev_sid != sid.as_str()
                            {
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
