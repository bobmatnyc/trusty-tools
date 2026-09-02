//! `GET /api/console/machine-status/history` and `…/stream` (#6641).
//!
//! Why: `GET /api/console/machine-status` answers with one point in time, which
//! draws a number but not a graph. Phase 3 of epic #6516 needs the last 10
//! minutes of host samples and the moments each service changed state, arriving
//! live while the dashboard is open. These two routes are the pull and the push
//! halves of that: the history endpoint seeds a page load, the stream keeps it
//! moving.
//!
//! Why history resets on restart: the owner's Phase 3 ruling is no persistence —
//! the window is in-memory only, so a restarted console begins an empty 10-minute
//! window and the graph fills from the left. This is by design, not a bug to
//! work around: nothing here reads or writes disk, and a client that reconnects
//! after a restart correctly sees a short window rather than a stale one.
//!
//! Why history answers `200` on a cold cache while `/machine-status` answers
//! `503`: an empty window is a complete, true answer — "no samples yet" — and
//! the graph renders it as an empty axis. The point-in-time route has no such
//! answer, because there is no host snapshot to return.
//! What: [`history_handler`] serves the
//! [`HistorySnapshot`](crate::machine_history::HistorySnapshot) as JSON;
//! [`stream_handler`] serves the same snapshot as the first `history` event of a
//! `text/event-stream`, then one `sample` event per new sample, one `transition`
//! event per service change, and a `lagged` event carrying the dropped count
//! whenever a slow subscriber falls behind.
//! Test: `crate::server::tests::machine_history_route_*` and
//! `crate::machine_history::stream::tests`.

use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::machine_history::stream::event_stream;
use crate::server::AppState;

/// `GET /api/console/machine-status/history` — the bounded window as JSON.
///
/// Why: a page load needs the whole window in one request before it opens a
/// stream; polling the stream's first event would mean holding a connection
/// open just to read it.
/// What: returns the sample ring and the transition log oldest-first, plus the
/// capacities and the sample interval the graph's axes are derived from. Always
/// `200` — before the first sample the arrays are simply empty.
/// Test: `machine_history_route_cold_cache_returns_empty_200`,
/// `machine_history_route_returns_recorded_samples`.
pub async fn history_handler(State(state): State<AppState>) -> Response {
    axum::Json(state.machine_history().snapshot().await).into_response()
}

/// `GET /api/console/machine-status/stream` — the live event stream.
///
/// Why the history is sent first rather than left to the client: a browser that
/// connects mid-window would otherwise draw an empty graph until the next
/// sample, and reconciling a separately-fetched history against a stream that
/// started at an unknown moment is exactly the gap-or-duplicate problem
/// `MachineHistory::subscribe` exists to remove.
/// What: `200 text/event-stream`. The first frame is `event: history` carrying
/// the current window; every later frame is `event: sample`, `event:
/// transition`, or `event: lagged` (with the count a slow subscriber dropped),
/// each framed `data: {json}\n\n`. `X-Accel-Buffering: no` keeps a reverse proxy
/// from buffering the stream into uselessness — the same headers the #6524
/// search relay sets.
/// Test: `machine_history_stream_route_is_an_event_stream`, plus
/// `crate::machine_history::stream::tests` for the frame sequence.
pub async fn stream_handler(State(state): State<AppState>) -> Response {
    let stream = event_stream(state.machine_history()).await;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
