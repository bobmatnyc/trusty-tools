//! `session.events` — the streaming method a UDS client tails a session
//! through (#6637).
//!
//! Why: `session.attach` is the one production handler that pushes on
//! `crate::jsonrpc::ConnectionContext::notify`, and a UDS connection served by
//! [`trusty_common::uds::server`] reads one request frame and writes one
//! response frame, so it is not a push channel. HTTP solved the same problem
//! with a dedicated SSE route (`crate::serve::http::session_events_sse`);
//! this is that route's transport-native twin, registered with
//! [`trusty_common::uds::server::RpcRouter::typed_stream`] so the answer is a
//! sequence of frames instead of one.
//!
//! What: [`open`](crate::session::events_stream::open) snapshots the session's
//! ring buffer, then forwards the
//! daemon-global event bus filtered to that session, into the
//! [`trusty_common::uds::server::RpcStreamItems`] channel the router writes
//! frames from.
//! [`SessionEventsParams::after_seq`](crate::session::events_stream::SessionEventsParams::after_seq)
//! is what HTTP has no
//! equivalent of: a client whose connection was cut names the last `seq` it
//! confirmed, and the replay resumes above it instead of repeating the whole
//! ring. The subscribe happens BEFORE the replay snapshot so an event
//! published between the two is delivered rather than dropped, and every
//! forwarded envelope's `seq` is compared against the highest already sent, so
//! the overlap that ordering creates never reaches the client twice.
//!
//! **A lagged bus ends the stream.** The `tokio::sync::broadcast` bus drops
//! the oldest envelopes for a receiver that falls behind, so continuing would
//! deliver a tail with a hole in it that the client could not see. The stream
//! ends with an error frame instead; the client reconnects with `after_seq`
//! and the ring buffer refills the gap. `session_events_sse` skips a lag
//! silently, which is the behaviour this deliberately does not copy.
//!
//! Test: `events_stream_tests`.

use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use trusty_common::uds::server::{RpcError, RpcStreamItems};

use crate::events::SessionEventEnvelope;
use crate::session::SessionRegistry;

/// The method name this stream is registered under.
pub const METHOD: &str = "session.events";

/// How many event frames may sit unwritten before the producer waits.
///
/// Why: back-pressure rather than an unbounded queue — the consumer is one
/// socket, and a client that stops reading must slow the producer down instead
/// of growing memory without limit. Sized to the same order as the session
/// ring buffer so a burst of a whole replay does not block.
const CHANNEL_CAPACITY: usize = 256;

/// `session.events`' params.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionEventsParams {
    /// Session whose events to tail.
    pub session_id: String,
    /// Resume above this `seq`, skipping everything already confirmed.
    ///
    /// Absent means "replay the whole ring buffer", which is what a first
    /// attach wants and what HTTP's SSE route always does.
    #[serde(default)]
    pub after_seq: Option<u64>,
}

/// Open one `session.events` stream.
///
/// Why/What: see module docs.
///
/// # Errors
///
/// [`crate::jsonrpc::RpcError::session_not_found`], converted onto the
/// transport's error type, when `session_id` names no live session — the same
/// refusal `session_events_sse` answers as a `404`.
///
/// Test: `events_stream_tests::session_events_replays_ring_buffer_then_live`,
/// `events_stream_tests::session_events_after_seq_skips_replayed_events`,
/// `events_stream_tests::session_events_unknown_session_is_refused`.
pub async fn open(
    sessions: Arc<SessionRegistry>,
    params: SessionEventsParams,
) -> Result<RpcStreamItems, RpcError> {
    // Subscribed BEFORE the snapshot: an envelope published between the two
    // would otherwise be in neither half. The overlap this creates is removed
    // by the `seq` floor in `forward`.
    let live = crate::events::subscribe();
    let replay = sessions.replay(&params.session_id)?;
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    tokio::spawn(forward(
        params.session_id,
        params.after_seq,
        replay,
        live,
        tx,
    ));
    Ok(rx)
}

/// Replay, then tail, until the client goes away or the bus lags.
///
/// `delivered` is the highest `seq` already written. It starts at the caller's
/// `after_seq`, rises through the replay, and then filters the live half — one
/// rule covering both the client's resume point and the replay/subscribe
/// overlap.
async fn forward(
    session_id: String,
    after_seq: Option<u64>,
    replay: Vec<SessionEventEnvelope>,
    live: broadcast::Receiver<SessionEventEnvelope>,
    tx: mpsc::Sender<Result<Value, RpcError>>,
) {
    let mut delivered = after_seq;
    for envelope in replay {
        if delivered.is_some_and(|floor| envelope.seq <= floor) {
            continue;
        }
        delivered = Some(envelope.seq);
        if !send(&tx, &envelope).await {
            return;
        }
    }

    let mut live = BroadcastStream::new(live);
    while let Some(item) = live.next().await {
        match item {
            Ok(envelope) => {
                if envelope.session_id != session_id {
                    continue;
                }
                if delivered.is_some_and(|floor| envelope.seq <= floor) {
                    continue;
                }
                delivered = Some(envelope.seq);
                if !send(&tx, &envelope).await {
                    return;
                }
            }
            Err(BroadcastStreamRecvError::Lagged(missed)) => {
                let resume = delivered
                    .map(|seq| format!("after_seq {seq}"))
                    .unwrap_or_else(|| "no after_seq".to_string());
                let _ = tx
                    .send(Err(RpcError::internal(format!(
                        "event bus lagged and dropped {missed} events for session \
                         {session_id}; reconnect with {resume}"
                    ))))
                    .await;
                return;
            }
        }
    }
}

/// Write one envelope, reporting whether the stream may continue.
///
/// `false` means the receiver is gone (the client disconnected) or the
/// envelope would not serialise — the second is a bug on this side of the
/// wire, so it is reported as a terminal error rather than skipped.
async fn send(tx: &mpsc::Sender<Result<Value, RpcError>>, envelope: &SessionEventEnvelope) -> bool {
    match serde_json::to_value(envelope) {
        Ok(value) => tx.send(Ok(value)).await.is_ok(),
        Err(e) => {
            let _ = tx
                .send(Err(RpcError::internal(format!(
                    "serialize session event: {e}"
                ))))
                .await;
            false
        }
    }
}

#[cfg(test)]
#[path = "events_stream_tests.rs"]
mod events_stream_tests;
