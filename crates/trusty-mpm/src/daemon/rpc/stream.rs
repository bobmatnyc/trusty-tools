//! The two hook-event SSE routes as streaming JSON-RPC methods (#6288 slice 6).
//!
//! Why: slices 2–5 moved every request/response family onto the socket, which
//! left `GET /events` and `GET /sessions/{id}/events` as the last routes with no
//! socket form. Both answer with an open-ended `text/event-stream`, so a unary
//! method could only return a snapshot — a different route, which
//! `mpm.events.poll` and `mpm.sessions.events_poll` already are. These two are
//! registered with [`RpcRouter::typed_stream`] instead, so a caller sends
//! `"stream": true` and reads one frame per event.
//!
//! What: the name-to-route table below, `METHODS` as an assertable array of it,
//! and [`register`], which mounts both names on the router `daemon::socket`
//! builds. [`event_mentions_session`] is the per-session predicate, shared with
//! the HTTP handler rather than restated, so the two transports cannot drift
//! into filtering differently.
//!
//! ## Method → route
//!
//! | Method | HTTP route |
//! |---|---|
//! | `mpm.events.stream` | `GET /events` (SSE) |
//! | `mpm.sessions.events_stream` | `GET /sessions/{id}/events` (SSE) |
//!
//! The `_stream` suffix pairs each name with the polled leg slice 3 registered:
//! `mpm.events.poll` / `mpm.events.stream`, and `mpm.sessions.events_poll` /
//! `mpm.sessions.events_stream`. A bare `mpm.sessions.events` would read as a
//! getter beside its own `_poll` sibling.
//!
//! ## What a frame carries, and what ends a stream
//!
//! One frame per event, in publication order, each carrying the SAME
//! `serde_json::Value` the SSE `data:` line carries — the `data:` line is that
//! value's `to_string()`, so a UDS subscriber and a concurrent `EventSource`
//! observe identical sequences. `stream_matches_the_sse_sequence` and
//! `session_stream_matches_the_sse_sequence` are the proof.
//!
//! **A lagged subscriber SKIPS the dropped events and keeps streaming.** That
//! mirrors the HTTP handlers exactly: both `api::stream_events` and
//! `api::stream_session_events` map a `BroadcastStream` error to `None` inside
//! a `filter_map`, which drops that item and continues. No lag marker is emitted
//! on either transport, because the SSE routes emit none and this slice invents
//! no semantics HTTP does not already have. The broadcast channel intentionally
//! sheds load rather than blocking its publishers
//! (`DaemonState::push_hook_event` is best-effort by construction), so a
//! subscriber that falls more than `EVENT_CHANNEL_CAPACITY` behind loses frames
//! on both transports alike.
//!
//! **The stream ends when the daemon's channel closes**, which happens when the
//! state is dropped at shutdown. The forwarder then drops its sender and
//! `write_stream` writes the terminal `end` frame.
//!
//! ## No receiver leak
//!
//! Each open stream holds one `broadcast::Receiver` inside its forwarder task.
//! A client that hangs up makes the next `tx.send` fail once `write_stream`
//! stops draining, which breaks the loop and drops the receiver — so the
//! daemon's receiver count returns to its baseline rather than accumulating one
//! per disconnected subscriber.
//! Test: `disconnecting_a_socket_client_releases_the_broadcast_receiver`.
//!
//! ## What guards these methods
//!
//! Nothing beyond the transport, and nothing is dropped by moving across. The
//! HTTP routes are `GET`s under the router-wide `guard_write_origin` layer,
//! which inspects `Origin` on writes and authenticates nobody; the socket runs
//! `ensure_peer_is_self` on every accepted connection before a byte is read,
//! over a `0600` socket in a `0700` directory. The peer-uid check is the
//! stronger of the two. See `super::sessions_legacy`'s header for the full
//! argument — these routes read the same event log those methods write.
//!
//! Test: `stream_tests.rs`.
//!
//! [`RpcRouter::typed_stream`]: trusty_common::uds::server::RpcRouter::typed_stream

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use trusty_common::uds::server::{RpcRouter, RpcStreamItems};

use super::core::NoParams;
use super::sessions_legacy::SessionIdParams;
use crate::daemon::state::DaemonState;

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;

/// Every method this slice registers, in registration order.
///
/// Why: the slice-7 client swap will dial these names by literal, from code with
/// no compile-time link to this table — the same reason `sessions_legacy`
/// carries one. A rename then surfaces as a failing assertion rather than as a
/// consumer that silently reports `method_not_found`.
///
/// These are STREAMING names, so a coverage test compares them against
/// `RpcRouter::stream_names`, never `method_names`: the router keeps the two
/// tables separate so one name is one or the other.
///
/// Test: `rpc_router_registers_every_documented_stream_method`.
pub const METHODS: &[&str] = &["mpm.events.stream", "mpm.sessions.events_stream"];

/// How many events one open stream buffers ahead of its socket writer.
///
/// The same figure `mpm.control.connect` uses. It bounds only the hand-off
/// between the forwarder task and `write_stream`; the real backlog budget is the
/// broadcast channel's `EVENT_CHANNEL_CAPACITY`, past which the daemon sheds
/// events for this subscriber on both transports.
const ITEM_CHANNEL_CAPACITY: usize = 64;

/// Whether a broadcast event belongs to session `id`.
///
/// Why it is shared rather than restated: this slice's acceptance bar is that a
/// UDS subscriber and a concurrent HTTP `EventSource` see the same sequence, and
/// two copies of a filter rule are exactly how that stops being true. HTTP's
/// `stream_session_events` calls this function, so a change to the rule reaches
/// both transports or neither.
///
/// What: the substring match `api::stream_session_events` has always used,
/// carried across unchanged. Every [`HookEventRecord`] serialises its
/// `SessionId` as the UUID string, so a substring test over the serialised event
/// answers the question without a typed parse per frame. It is deliberately
/// loose — an id appearing anywhere in the payload matches — and narrowing it
/// would change HTTP's behaviour, which this slice does not do.
///
/// [`HookEventRecord`]: crate::core::hook::HookEventRecord
///
/// Test: `session_stream_matches_the_sse_sequence`,
/// `event_mentions_session_matches_the_serialized_id`.
pub fn event_mentions_session(event: &serde_json::Value, id: &str) -> bool {
    event.to_string().contains(id)
}

/// Mount every method in [`METHODS`] onto `router`.
///
/// Why a free function rather than a builder method: `daemon::socket` names one
/// `register` call per family, so a family arrives without the accept loop, the
/// framing, or the peer check being touched.
///
/// Test: `rpc_router_registers_every_documented_stream_method`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let all = Arc::clone(state);
    let per_session = Arc::clone(state);
    router
        // #6288 slice 6: `GET /events`, unfiltered.
        .typed_stream("mpm.events.stream", move |_params: NoParams| {
            let state = Arc::clone(&all);
            async move { Ok(forward_events(&state, None)) }
        })
        // #6288 slice 6: `GET /sessions/{id}/events`, filtered to one session.
        .typed_stream(
            "mpm.sessions.events_stream",
            move |params: SessionIdParams| {
                let state = Arc::clone(&per_session);
                async move { Ok(forward_events(&state, Some(params.id))) }
            },
        )
}

/// Subscribe to the daemon's hook-event broadcast and forward it as stream
/// items, optionally filtered to one session.
///
/// Why one body for both methods: the unfiltered route is the filtered one with
/// the predicate omitted, and writing them separately is how the two would drift
/// in their lag handling.
/// What: subscribes BEFORE returning, so an event published between the caller's
/// request and the first `recv` is not lost, then forwards on a background task.
/// A lagged read is skipped and the loop continues — the `filter_map(… Err(_) =>
/// None)` the SSE handlers do, in imperative form. A `send` failure means the
/// consumer is gone, which ends the task and releases the receiver.
///
/// Test: `stream_matches_the_sse_sequence`,
/// `stream_skips_lagged_events_and_keeps_streaming`,
/// `disconnecting_a_socket_client_releases_the_broadcast_receiver`.
fn forward_events(state: &Arc<DaemonState>, session: Option<String>) -> RpcStreamItems {
    let rx = state.event_subscribe();
    let (tx, items) = mpsc::channel(ITEM_CHANNEL_CAPACITY);
    tokio::spawn(async move {
        let mut stream = BroadcastStream::new(rx);
        while let Some(item) = stream.next().await {
            // A lag drops frames and keeps the stream open, exactly as the SSE
            // `filter_map` does. Ending here instead would be a semantic this
            // slice invented; see the module header.
            let Ok(event) = item else { continue };
            if let Some(id) = session.as_deref()
                && !event_mentions_session(&event, id)
            {
                continue;
            }
            if tx.send(Ok(event)).await.is_err() {
                // The consumer hung up. Dropping `stream` here is what keeps the
                // daemon's receiver count from growing one per dead subscriber.
                break;
            }
        }
    });
    items
}
