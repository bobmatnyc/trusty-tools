//! The two SSE surfaces, served as typed JSON-RPC streams (#6285 slice 5).
//!
//! Why: slices 2–4 moved every route that answers in ONE body. These two answer
//! in many, over a connection that stays open — `GET /status/stream` and
//! `GET /indexes/{id}/reindex/stream`. HTTP is untouched and still serves both;
//! the socket is an ADDITIONAL way to reach the same event sequence until the
//! retire slice.
//!
//! What: the method-to-route table below, the two producers, and [`register`].
//!
//! ## Method → route
//!
//! | Method | HTTP route | Lane |
//! |---|---|---|
//! | `search.status.stream` | `GET /status/stream` | free |
//! | `search.index.reindex.stream` | `GET /indexes/{id}/reindex/stream` | free |
//!
//! Both HTTP routes sit in `service::server::build_router_on`'s `free` group —
//! no admission limiter, no query deadline — and both methods copy that. It is
//! not only parity: a stream runs for as long as its reindex does, so holding an
//! admission permit for its lifetime would let two dashboards exhaust the
//! semaphore that `/health` and every query share (#41).
//!
//! ## The streaming mechanism is [`trusty_common::uds::server`]'s, not this
//! crate's
//!
//! `RpcRouter::typed_stream` and `write_stream` shipped with #6286 for
//! trusty-memory's chat tokens (trusty-common 0.44.2, PR #6296). This module
//! registers producers against them and encodes nothing: a request carrying
//! `"stream": true` gets zero or more `"stream":"item"` frames and then EXACTLY
//! one terminal `"stream":"end"` or `"stream":"error"` frame. A second framing
//! implementation here is the duplication the common-entry-point rule exists to
//! prevent, and the SSE `data: …\n\n` encoding is deliberately NOT ported.
//!
//! ## What one item carries
//!
//! One item is the JSON value one SSE `data:` line carries — the same document,
//! parsed rather than prefixed. `{"type":"connected"}`, a `DaemonEvent`, a
//! reindex progress event, and the `{"type":"lag","skipped":N}` frame a lagged
//! broadcast subscriber gets are all identical on both transports.
//!
//! Two SSE-only frames have no socket counterpart, both because the reason for
//! them is HTTP's:
//!
//! - the `: heartbeat\n\n` comment frame `reindex_stream_handler` emits every
//!   20 s. It exists so an idle TCP body is not torn down by the OS or an
//!   intermediate proxy while the embedder sidecar stalls between batches. A
//!   Unix socket has neither, and `trusty_common::uds`'s stream client applies
//!   NO read budget between frames, so an idle stream here is not a stream at
//!   risk. A heartbeat would be an item a consumer has to learn to discard.
//! - the `data:` prefix and the blank-line separator, which the frame protocol
//!   replaces.
//!
//! ## The 8 MiB frame budget applies PER ITEM
//!
//! `RpcServeOptions::max_frame_bytes` bounds each streamed frame separately, not
//! the stream's total — so an hour-long reindex is never near it. A single
//! progress event is a few hundred bytes and a `DaemonEvent` is smaller, so
//! neither surface can reach the budget the way slice 4's `search.graph.ingest`
//! request can.
//!
//! ## A dropped client stops the producer
//!
//! Each producer runs on its own task and writes into an `mpsc::Sender`. The
//! server learns of a disconnect at its NEXT WRITE and not before: the stream
//! client half-closes its write side once the request frame is out
//! (`uds::rpc::dial_and_send`), so read-EOF on the server means "the request is
//! complete", not "the caller left". `write_stream` therefore parks in
//! `recv()` until an item exists to fail on.
//!
//! What follows from that, and what this module does about it:
//!
//! - the failing write drops the receiver, so the next `send` here returns
//!   `Err`. Every loop treats that as the end; discarding it is what would leak
//!   a task per abandoned dashboard, so no `send` here is discarded.
//! - each wait ALSO selects on `Sender::closed()`, so a producer parked in
//!   `broadcast::recv()` ends as soon as the receiver goes rather than one event
//!   later. Without it a disconnect would cost two events to clear instead of
//!   one.
//! - one event still has to arrive, and how long that takes differs per stream.
//!   `search.status.stream` is bounded: the daemon's status ticker emits every
//!   ~2 s whatever else is happening, so an abandoned producer clears within a
//!   tick. `search.index.reindex.stream` is bounded only while the reindex is
//!   PROGRESSING — it emits per batch, and a run that stalls between batches
//!   emits nothing, so its abandoned producer stays parked for the stall's
//!   duration. A finished reindex never parks at all; that arm returns after the
//!   replay rather than subscribing. A run that COMPLETES while a producer is
//!   parked ends it when the progress record is garbage-collected — 60 s after
//!   the terminal event (`service::reindex::stages`'s `REINDEX_PROGRESS_TTL_SECS`)
//!   — because dropping the record drops the sender the producer is waiting on.
//!   `a_client_that_drops_mid_stream_leaves_no_producer_subscribed` pins that
//!   sequence over a real socket for the status stream; the stalled-reindex case
//!   is documented rather than tested, since forcing a stall needs a seam the
//!   reindex runner does not have.
//!
//! The pre-existing HTTP SSE route parks on the same broadcast with the same
//! characteristic: its 20 s heartbeat keeps the TCP connection alive but does not
//! unstick a stalled reindex either. So a parked producer costs one task and one
//! broadcast subscriber slot. Neither stream holds an admission permit for its
//! lifetime, so an abandoned one costs the semaphore nothing even while its task
//! is still parked.
//!
//! Test: `streams_tests.rs`.
//!
//! [`register`]: crate::service::rpc::streams::register

use std::sync::Arc;

use axum::http::StatusCode;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use trusty_common::uds::server::{RpcError, RpcRouter, RpcStreamItems};

use crate::core::registry::IndexId;
use crate::service::reindex::ReindexStatus;
use crate::service::server::SearchAppState;
use crate::service::socket::NoParams;

use super::error::rpc_error_from_http;
use super::reads::IndexRef;

#[cfg(test)]
#[path = "streams_tests.rs"]
mod tests;

/// `GET /status/stream` — live daemon stats, one item per `DaemonEvent`.
pub const METHOD_STATUS_STREAM: &str = "search.status.stream";
/// `GET /indexes/{id}/reindex/stream` — one index's reindex progress events.
pub const METHOD_INDEX_REINDEX_STREAM: &str = "search.index.reindex.stream";

/// Every method this slice registers, in registration order.
///
/// Why: the same contract `reads::METHODS`, `queries::METHODS` and
/// `writes::METHODS` carry — `service::socket::METHODS` splices these in by
/// reference rather than restating the literals, so a rename is a compile error
/// there rather than a drift only a running consumer would find.
/// Test: `rpc_router_registers_every_documented_method` and
/// `every_family_method_is_spliced_into_the_socket_method_list` in
/// `socket_tests.rs`.
pub const METHODS: &[&str] = &[METHOD_STATUS_STREAM, METHOD_INDEX_REINDEX_STREAM];

/// How many items a producer may run ahead of the connection writer.
///
/// Why bounded at all: an unbounded channel lets a fast producer — a reindex
/// emitting a batch event per second — accumulate without limit behind a client
/// that has stopped reading but not yet disconnected. Why this figure: the
/// broadcast channel each producer reads has a capacity of 256, so a buffer near
/// it neither becomes the first thing to overflow nor hides a lag the broadcast
/// side is about to report anyway.
const STREAM_BUFFER: usize = 64;

/// One SSE `data:` payload, as the JSON value it is.
///
/// Why not the raw string: the HTTP handlers write these lines verbatim after a
/// `data: ` prefix, and every producer in this crate writes JSON there. Parsing
/// makes an item on this transport the SAME document a `data:` line carries,
/// which is what the parity tests compare.
/// What: a line that does not parse is emitted as a JSON string rather than
/// dropped or turned into a terminal error. That is a producer bug, and the
/// stream HTTP would have delivered must not die of it — the bytes stay
/// recoverable by the consumer.
/// Test: `reindex_progress_events_match_the_sse_body_frame_for_frame`.
fn item(line: String) -> serde_json::Value {
    serde_json::from_str(&line).unwrap_or(serde_json::Value::String(line))
}

/// The `{"type":"lag","skipped":N}` frame both transports send a lagged reader.
fn lag(skipped: u64) -> serde_json::Value {
    serde_json::json!({ "type": "lag", "skipped": skipped })
}

/// `search.status.stream` — the item sequence `GET /status/stream` writes.
///
/// Why infallible: subscribing to the daemon's broadcast sender cannot fail, so
/// unlike the reindex stream there is nothing to refuse before the first frame.
/// What: `{"type":"connected"}` first — so a consumer knows the subscription is
/// live before any event exists — then every `DaemonEvent` as it is emitted, and
/// a lag frame when the subscriber falls behind. The stream ends only when the
/// daemon's sender is dropped, which is process shutdown, or when the client
/// disconnects.
/// Test: `the_status_stream_opens_with_the_same_connected_frame_sse_writes`,
/// `a_daemon_event_reaches_both_the_sse_body_and_the_socket_stream`.
fn status_stream(state: &Arc<SearchAppState>) -> RpcStreamItems {
    let mut events = state.events.subscribe();
    let (tx, items) = mpsc::channel(STREAM_BUFFER);

    tokio::spawn(async move {
        if tx
            .send(Ok(serde_json::json!({ "type": "connected" })))
            .await
            .is_err()
        {
            return;
        }
        loop {
            let received = tokio::select! {
                biased;
                () = tx.closed() => return,
                received = events.recv() => received,
            };
            let value = match received {
                Ok(event) => serde_json::to_value(&event).unwrap_or_else(
                    |e| serde_json::json!({ "type": "error", "message": e.to_string() }),
                ),
                Err(RecvError::Lagged(skipped)) => lag(skipped),
                // The daemon's sender is gone: nothing more can be emitted, so
                // this is an END rather than an error.
                Err(RecvError::Closed) => return,
            };
            if tx.send(Ok(value)).await.is_err() {
                return;
            }
        }
    });

    items
}

/// `search.index.reindex.stream` — the item sequence
/// `GET /indexes/{id}/reindex/stream` writes.
///
/// What: look the index's progress up and refuse when there is none, then open
/// the stream through [`ReindexProgress::subscribe_with_replay`], which hands
/// back the replay buffer, the status, and the live subscription together.
///
/// #6386: that ONE call is what makes the open exactly-once. It used to be three
/// statements here — snapshot, read status, subscribe — copied by hand from the
/// SSE handler, and an event whose buffer write and broadcast straddled them
/// could arrive twice or not at all. The SSE route now opens through the same
/// method, so parity holds by construction rather than by two hand-kept copies;
/// the argument for why nothing can straddle it is on that method.
///
/// A progress record whose status is no longer `Running` yields its replay and
/// ends. Without that arm the stream would deliver the buffered `complete` event
/// and then idle forever on a broadcast nothing will send to again.
///
/// # Errors
///
/// [`CODE_NOT_FOUND`] when no reindex has been recorded for the index, which is
/// the `404` the HTTP route answers. HTTP sends that status with an empty body,
/// so the message here is this transport's own rather than a copied one.
///
/// [`CODE_NOT_FOUND`]: super::error::CODE_NOT_FOUND
///
/// Test: `reindex_progress_events_match_the_sse_body_frame_for_frame`,
/// `a_live_reindex_stream_delivers_events_in_order_as_they_are_emitted`,
/// `an_event_either_side_of_the_stream_opening_is_delivered_exactly_once`,
/// `a_stream_opening_against_live_pushes_loses_and_repeats_nothing`,
/// `an_unknown_index_is_refused_before_the_reindex_stream_opens`.
async fn reindex_stream(
    state: &Arc<SearchAppState>,
    index_id: &str,
) -> Result<RpcStreamItems, RpcError> {
    let id = IndexId::new(index_id.to_string());
    let progress = state
        .reindex_progress
        .get(&id)
        .map(|entry| Arc::clone(entry.value()))
        .ok_or_else(|| {
            rpc_error_from_http(
                StatusCode::NOT_FOUND,
                &serde_json::json!({
                    "error": format!("no reindex progress for index {}", id.0),
                }),
            )
        })?;

    // #6386: one atomic open — no event can straddle the snapshot and the subscribe.
    let (replay, status, mut events) = progress.subscribe_with_replay().await;
    let live = status == ReindexStatus::Running;

    let (tx, items) = mpsc::channel(STREAM_BUFFER);
    tokio::spawn(async move {
        for line in replay {
            if tx.send(Ok(item(line))).await.is_err() {
                return;
            }
        }
        if !live {
            return;
        }
        loop {
            let received = tokio::select! {
                biased;
                () = tx.closed() => return,
                received = events.recv() => received,
            };
            let value = match received {
                Ok(line) => item(line),
                Err(RecvError::Lagged(skipped)) => lag(skipped),
                // The reindex task dropped its sender: the run is over.
                Err(RecvError::Closed) => return,
            };
            if tx.send(Ok(value)).await.is_err() {
                return;
            }
        }
    });

    Ok(items)
}

/// Map this slice's two methods onto their producers.
///
/// Neither is wrapped in an admission or deadline guard, because neither HTTP
/// route is — see the lane table in this module's header, and
/// `every_socket_method_takes_the_admission_lane_its_http_route_takes` in
/// `lanes_tests.rs`, which asserts that for all of them at once.
///
/// Test: `rpc_router_registers_the_two_streams_as_streams`.
pub fn register(router: RpcRouter, state: &Arc<SearchAppState>) -> RpcRouter {
    let held = Arc::clone(state);
    let router = router.typed_stream::<NoParams, _, _>(METHOD_STATUS_STREAM, move |_params| {
        let state = Arc::clone(&held);
        async move { Ok(status_stream(&state)) }
    });

    let held = Arc::clone(state);
    router.typed_stream::<IndexRef, _, _>(METHOD_INDEX_REINDEX_STREAM, move |params| {
        let state = Arc::clone(&held);
        async move { reindex_stream(&state, &params.index_id).await }
    })
}
