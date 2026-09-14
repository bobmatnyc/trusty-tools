//! `GET /api/console/events/stream/sse` — the console event bus, fanned out
//! (issue #6851, DOC-73 §4.4).
//!
//! Why this handler is thin: everything that can silently break lives in
//! [`super::stream`], which a test drives without HTTP. What is left here is
//! the part only HTTP can express — three distinguishable answers.
//!
//! | Bus state | Answer |
//! |---|---|
//! | Running, no events yet | `200 text/event-stream`, a `ready` frame, then heartbeats |
//! | Not running | `503 application/json`, `{"error":"event_bus_unavailable",…}` |
//! | Running, unusable `Last-Event-ID` | `400 application/json`, `{"error":"bad_last_event_id",…}` |
//!
//! Why 503 and not an empty 200: DOC-73 §8.5 requires a viewer to tell "nothing
//! is happening" from "cannot see", and an event stream that opens and stays
//! quiet looks identical to a healthy idle one. The spec names the requirement
//! and not the status code; 503 is this crate's existing answer for a surface
//! whose backing data is not there (`GET /api/console/machine-status` on a cold
//! cache), so the dashboard's existing error handling already covers it.
//!
//! Test: `super::tests::a_dead_bus_answers_503_json`,
//! `super::tests::a_healthy_empty_bus_answers_200_event_stream`,
//! `super::tests::an_unparseable_last_event_id_is_rejected`.

use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use super::frames::parse_last_event_id;
use super::stream::{HEARTBEAT_INTERVAL, event_stream};
use crate::server::AppState;

/// The path this handler is mounted at (`server::router`).
pub(crate) const SSE_PATH: &str = "/api/console/events/stream/sse";

/// The `Last-Event-ID` request header, as an SSE client sends it on reconnect.
const LAST_EVENT_ID: &str = "last-event-id";

/// Serve the live event stream, resuming from `Last-Event-ID` when present.
///
/// Why the header rather than a query parameter: a browser's `EventSource`
/// replays the last `id:` it saw automatically, on its own reconnect, with no
/// application code involved — that automatic resume is the reason DOC-73 §4.4
/// chose SSE over a WebSocket. `since_seq` on the sibling polling route
/// (`/api/console/events/stream`, #6850) is the same cursor for a reader that
/// is not an `EventSource`.
/// What: 503 when no bus is wired, 400 when the header is present but not a
/// decimal seq, otherwise `200 text/event-stream` carrying [`event_stream`].
/// `X-Accel-Buffering: no` keeps a reverse proxy from buffering the stream into
/// uselessness — the same headers the machine-status stream sets.
/// Test: `super::tests::a_dead_bus_answers_503_json`,
/// `super::tests::a_healthy_empty_bus_answers_200_event_stream`.
pub(crate) async fn sse_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    let Some(bus) = state.event_bus() else {
        // #6851: a console whose event-bus ingest never bound has no bus to
        // fan out. Distinct from an idle one — see this module's table.
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "error": "event_bus_unavailable",
                "detail": "the console event bus is not running; no events can be \
                           observed until its ingest socket binds",
            })),
        )
            .into_response();
    };

    // A non-ASCII header value is unusable for the same reason a non-numeric
    // one is: this stream never issued it. Both take the 400 arm below.
    let raw = headers
        .get(LAST_EVENT_ID)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
    let since_seq = match raw.as_deref().map(parse_last_event_id) {
        None => None,
        Some(Ok(seq)) => Some(seq),
        Some(Err(e)) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(serde_json::json!({
                    "error": "bad_last_event_id",
                    "detail": "Last-Event-ID must be a decimal event seq",
                    "value": e.value,
                })),
            )
                .into_response();
        }
    };

    let stream = event_stream(bus, since_seq, HEARTBEAT_INTERVAL);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
