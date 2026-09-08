//! `workstream.events` — the streaming method a UDS client observes one
//! workstream through (#6637).
//!
//! Why: `crate::workstreams::sse::routes` serves the same fan-out over HTTP,
//! and the TUI consumed it as SSE. The socket transport has no long-lived GET,
//! so the fan-out is registered as a
//! [`trusty_common::uds::server::RpcRouter::typed_stream`] method instead. The
//! fan-out itself is NOT reimplemented — [`crate::workstreams::sse::aggregate_live`]
//! stays the single definition of which events belong to a workstream, and
//! this module is the bridge from its `Stream` onto the router's channel.
//!
//! What: [`open`] validates the id and the workstream's existence exactly as
//! `workstream_events_sse` does, then forwards
//! [`crate::workstreams::WorkstreamEventEnvelope`]s until the client
//! disconnects.
//!
//! **There is no `after_seq` here, and there cannot be.** `aggregate_live` is
//! live-only by construction: a workstream has no ring buffer of its own, only
//! the per-session rings its bound sessions each hold, and those carry
//! per-session `seq` values with no workstream-wide ordering to resume from. A
//! client that reconnects re-subscribes bare and observes from that moment —
//! the same contract the HTTP route always had. `session.events`'s resume is
//! not portable to this method.
//!
//! Test: `crate::serve::uds_tests::stream_names_win_over_the_fallback` pins
//! the registration; the fan-out rules themselves are covered by `sse_tests`.

use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_stream::StreamExt as _;
use trusty_common::uds::server::{RpcError, RpcStreamItems};
use uuid::Uuid;

use super::activation::SharedWorkstreamStore;
use super::model::WorkstreamId;
use super::store::StoreError;

/// The method name this stream is registered under.
pub const METHOD: &str = "workstream.events";

/// How many event frames may sit unwritten before the producer waits — see
/// `crate::session::events_stream`'s constant of the same name.
const CHANNEL_CAPACITY: usize = 256;

/// `workstream.events`' params.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkstreamEventsParams {
    /// Workstream to observe. A UUID; anything else is `invalid_params`.
    pub workstream_id: String,
}

/// Open one `workstream.events` stream.
///
/// Why/What: see module docs.
///
/// # Errors
///
/// `invalid_params` for an id that is not a UUID, and `not_found` for one that
/// names no workstream — the `400` and `404` `workstream_events_sse` answers.
/// A CLOSED workstream is observable, not an error (DOC-48 §4.4).
///
/// Test: `crate::serve::uds_tests::workstream_events_refuses_an_unknown_id`.
pub async fn open(
    store: SharedWorkstreamStore,
    params: WorkstreamEventsParams,
) -> Result<RpcStreamItems, RpcError> {
    let id = Uuid::parse_str(&params.workstream_id)
        .map(WorkstreamId::from)
        .map_err(|e| RpcError::invalid_params(format!("invalid workstream id: {e}")))?;

    {
        let mut guard = store.lock().await;
        guard.get(id).await.map_err(map_store_err)?;
    }

    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    let stream = super::sse::aggregate_live(id, store);
    tokio::spawn(async move {
        tokio::pin!(stream);
        while let Some(envelope) = stream.next().await {
            let frame = match serde_json::to_value(&envelope) {
                Ok(value) => Ok(value),
                Err(e) => Err(RpcError::internal(format!(
                    "serialize workstream event: {e}"
                ))),
            };
            let failed = frame.is_err();
            if tx.send(frame).await.is_err() || failed {
                return;
            }
        }
    });
    Ok(rx)
}

/// Map a [`StoreError`] onto the transport's error type.
///
/// Delegates to [`crate::workstreams::sse::map_store_err`] rather than
/// restating the taxonomy — the HTTP route and this stream must refuse an
/// unknown workstream with the same code.
fn map_store_err(err: StoreError) -> RpcError {
    super::sse::map_store_err(err).into()
}
