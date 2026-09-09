//! Multi-frame responses: the handler shape, and the writer that drains it
//! (#6286).
//!
//! Why: `trusty-memory`'s `POST /api/v1/chat` streams LLM tokens off a
//! `tokio::sync::mpsc::Receiver` wrapped in a `ReceiverStream`, and the
//! one-frame-per-connection contract [`super::handle_connection`] enforces
//! cannot carry that. #6287 deferred the multi-frame extension until a daemon
//! had a real cross-crate SSE consumer; chat is that consumer, and degrading
//! chat to a single buffered frame was ruled out. So the producer shape here is
//! the shape chat already has: a handler hands back the RECEIVER and goes on
//! filling the sender from wherever it likes.
//!
//! What, in the order a service uses them.
//!   - [`RpcStreamItems`] is what a handler returns — an `mpsc::Receiver` of
//!     per-item results. `tokio_stream::wrappers::ReceiverStream::new` takes the
//!     same value, so a caller that already thinks in `Stream`s loses nothing.
//!   - [`RpcStreamMethod`] is the object-safe handler trait, and
//!     [`typed_stream_method`] builds one from an async function over the
//!     caller's own request type.
//!   - [`RpcOutcome`] is what [`super::RpcRouter::dispatch_streaming`] decides:
//!     one response, or a stream to drain.
//!   - [`write_stream`] drains that stream onto the socket in the frame contract
//!     [`RpcStreamFrame`] documents.
//!
//! **Every stream ends in exactly one terminal frame.** `write_stream` has no
//! path that returns `Ok` without having written an `end` or an `error` frame:
//! a handler error becomes a terminal error frame, an item too large for the
//! frame budget becomes a terminal error frame, and a producer that drops its
//! sender without failing becomes a terminal `end`. A silently truncated stream
//! would be a Fail-Open branch — the client would read a partial answer as a
//! complete one — so the only way a stream ends without a terminal frame is a
//! write failure, which the client sees as a hang-up rather than as data.
//!
//! Test: `super::tests` — `stream_*`.

use std::marker::PhantomData;
use std::time::Duration;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncWrite, AsyncWriteExt, Interest};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use super::wire::{RpcError, RpcResponse, RpcStreamFrame};

/// The channel a streaming handler produces into.
///
/// An `Ok` item becomes one [`StreamPhase::Item`] frame; an `Err` becomes the
/// terminal error frame and stops the stream. Dropping the sender ends the
/// stream successfully.
///
/// [`StreamPhase::Item`]: super::StreamPhase
pub type RpcStreamItems = mpsc::Receiver<Result<serde_json::Value, RpcError>>;

/// One streaming method's implementation (#6286).
///
/// Why: object-safe for the same reason [`RpcMethod`] is — the router holds a
/// heterogeneous set behind `Arc<dyn RpcStreamMethod>`. Most callers never name
/// it; [`typed_stream_method`] builds one from an ordinary async function.
/// What: opens the stream and returns its receiver. Returning `Err` here is the
/// "could not start" case — the caller never sent an item, so it becomes a
/// terminal error frame with zero items ahead of it.
///
/// [`RpcMethod`]: super::RpcMethod
///
/// Test: `stream_round_trips_many_frames_over_a_real_socket`,
/// `stream_reports_a_handler_error_as_a_terminal_frame`.
#[async_trait]
pub trait RpcStreamMethod: Send + Sync + 'static {
    /// Open a stream for one decoded `params` payload.
    async fn call(&self, params: serde_json::Value) -> Result<RpcStreamItems, RpcError>;
}

/// Adapter behind [`typed_stream_method`]; `PhantomData<fn(Req)>` keeps the
/// struct `Send + Sync` whatever `Req` is.
struct TypedStream<Req, F> {
    call: F,
    _req: PhantomData<fn(Req)>,
}

#[async_trait]
impl<Req, F, Fut> RpcStreamMethod for TypedStream<Req, F>
where
    Req: DeserializeOwned + Send + 'static,
    F: Fn(Req) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<RpcStreamItems, RpcError>> + Send + 'static,
{
    async fn call(&self, params: serde_json::Value) -> Result<RpcStreamItems, RpcError> {
        let request: Req = serde_json::from_value(params)
            .map_err(|e| RpcError::invalid_params(format!("params do not decode: {e}")))?;
        (self.call)(request).await
    }
}

/// Build an [`RpcStreamMethod`] from an async function over the caller's own
/// request type.
///
/// Why: the streaming counterpart of [`typed_method`] — the caller names `Req`
/// and never touches `serde_json::Value` on the way in. Items stay `Value`
/// deliberately: a token stream is heterogeneous in practice (a text delta, a
/// usage record, a finish reason), and forcing one `Resp` type on every frame
/// would push that union into every service.
/// What: deserialises `params` into `Req` before `call` runs, reporting a decode
/// failure as [`RpcError::invalid_params`] exactly as the unary path does.
///
/// [`typed_method`]: super::typed_method
///
/// Test: `stream_reports_invalid_params_before_opening_the_stream`.
pub fn typed_stream_method<Req, F, Fut>(call: F) -> impl RpcStreamMethod
where
    Req: DeserializeOwned + Send + 'static,
    F: Fn(Req) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<RpcStreamItems, RpcError>> + Send + 'static,
{
    TypedStream::<Req, F> {
        call,
        _req: PhantomData,
    }
}

/// What one request frame turned out to need (#6286).
///
/// Why: [`super::RpcRouter::dispatch`] answers with a value, which is right for
/// a unary call and cannot express "many frames follow". This is the wider
/// return the connection handler needs, kept out of `dispatch` so every existing
/// caller of that function is untouched.
/// What: either the single response frame to write, or the id to stamp on each
/// frame plus the receiver to drain.
///
/// `#[non_exhaustive]`: a third outcome (a bidirectional exchange, say) would
/// otherwise be a breaking change.
///
/// Test: `stream_round_trips_many_frames_over_a_real_socket`,
/// `dispatch_streaming_answers_a_unary_request_unchanged`.
#[non_exhaustive]
pub enum RpcOutcome {
    /// One response frame, exactly as a non-streaming exchange produces.
    Single(RpcResponse),
    /// A stream to drain onto the connection.
    Stream {
        /// The request id, echoed on every frame of the stream.
        id: serde_json::Value,
        /// The handler's items.
        items: RpcStreamItems,
    },
}

impl RpcOutcome {
    /// A stream that carries nothing but its terminal error frame.
    ///
    /// Why: a streaming request the router refuses — an unstreamable method, or
    /// a handler that failed before producing an item — must still answer in the
    /// shape the caller is reading, and must still end in exactly one terminal
    /// frame. Expressing the refusal as a one-item stream routes it through
    /// [`write_stream`] rather than adding a second place that decides what
    /// terminates a stream.
    ///
    /// Test: `stream_request_for_a_non_streaming_method_is_refused`,
    /// `stream_reports_an_open_failure_as_a_terminal_frame`.
    pub fn refused(id: serde_json::Value, error: RpcError) -> Self {
        let (tx, items) = mpsc::channel(1);
        // A fresh channel with capacity 1 and no other sender cannot be full or
        // closed, so this send cannot fail.
        let _ = tx.try_send(Err(error));
        drop(tx);
        Self::Stream { id, items }
    }
}

impl std::fmt::Debug for RpcOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Single(response) => f.debug_tuple("Single").field(response).finish(),
            // The receiver's contents are not inspectable without consuming
            // them, so the id is the whole of what can honestly be shown.
            Self::Stream { id, .. } => f.debug_struct("Stream").field("id", id).finish(),
        }
    }
}

/// Drain `items` onto `writer` as newline-terminated stream frames.
///
/// Why: the write half of the contract [`RpcStreamFrame`] states, in one place,
/// so "what terminates a stream" is decided once rather than per service.
/// What: one [`StreamPhase::Item`] frame per `Ok` item, then exactly one
/// terminal frame — `end` when the producer finishes, `error` when it fails or
/// when an item does not fit `max_frame_bytes`. Returns whether the terminal
/// frame carried an error.
///
/// **`max_frame_bytes` is per frame, not per stream.** An item that would not
/// fit is refused rather than truncated, because the client applies the same
/// budget to each frame it reads and a frame it cannot buffer would desynchronise
/// the rest of the connection.
///
/// The writer is flushed after every frame. Batching would be cheaper and would
/// also defeat the point: a token stream the client sees only at completion is
/// the buffered response streaming exists to replace.
///
/// **A departed peer ends the drain, whatever the producer is doing (#7217).**
/// Waiting on the producer alone left a handler parked for as long as its
/// stream stayed quiet, so a client that hung up kept the connection — and
/// [`super::serve_until`]'s shutdown drain — alive until its whole budget was
/// spent. [`peer_departed`] watches the socket alongside the channel; see it for
/// why a half-closed client is not mistaken for a departed one.
///
/// # Errors
///
/// Only the underlying write error, of which a departed peer is one: it returns
/// [`std::io::ErrorKind::BrokenPipe`] with no terminal frame written, exactly as
/// a failed write would. A handler failure is data, not an error here — it
/// leaves as the terminal error frame.
///
/// [`StreamPhase::Item`]: super::StreamPhase
///
/// Test: `stream_round_trips_many_frames_over_a_real_socket`,
/// `stream_reports_a_handler_error_as_a_terminal_frame`,
/// `stream_refuses_an_item_larger_than_the_frame_budget`,
/// `stream_survives_a_client_that_disconnects_mid_stream`,
/// `stream_ends_when_the_peer_departs_with_its_producer_still_open`.
pub async fn write_stream(
    writer: &mut UnixStream,
    id: serde_json::Value,
    mut items: RpcStreamItems,
    max_frame_bytes: u64,
) -> std::io::Result<bool> {
    loop {
        // #7217: the peer is watched alongside the producer, so a departed
        // client ends the handler instead of parking it on a quiet stream.
        let next = tokio::select! {
            item = items.recv() => item,
            () = peer_departed(writer) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "the stream's peer closed the socket",
                ));
            }
        };
        let Some(item) = next else { break };

        let value = match item {
            Ok(value) => value,
            Err(error) => {
                write_frame(writer, &RpcStreamFrame::error(id.clone(), error)).await?;
                return Ok(true);
            }
        };

        let frame = encode(&RpcStreamFrame::item(id.clone(), value))?;
        if frame.len() as u64 > max_frame_bytes {
            // Fail closed: the client's reader refuses a frame over its own
            // budget, so writing this one would strand the connection
            // mid-stream with no terminal frame in it.
            let refusal = RpcError::internal(format!(
                "stream item of {} bytes exceeds the {max_frame_bytes}-byte frame budget",
                frame.len()
            ));
            write_frame(writer, &RpcStreamFrame::error(id.clone(), refusal)).await?;
            return Ok(true);
        }
        writer.write_all(&frame).await?;
        writer.flush().await?;
    }

    write_frame(writer, &RpcStreamFrame::end(id)).await?;
    Ok(false)
}

/// How often an otherwise quiet stream re-reads whether its peer is still there.
///
/// Why a poll rather than an await on the event: the socket is writable for
/// essentially a stream's whole life, so `ready(Interest::WRITABLE)` returns at
/// once and awaiting it in a loop would spin. A quarter second bounds how long a
/// departed client delays a shutdown drain, at four wakeups per second per open
/// stream.
const PEER_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Resolve once `socket`'s peer can no longer receive anything (#7217).
///
/// Why: [`write_stream`] needs "the client left" as a signal it can select on,
/// and read-EOF is not that signal. `uds::rpc::dial_and_send` half-closes the
/// client's write side the moment the request frame is out, so every
/// well-behaved streaming client sits at read-EOF for the whole stream.
/// What: `is_write_closed` is the bit that separates the two. A peer that closed
/// both halves leaves this socket's SEND side shut down — `EPOLLHUP` on Linux,
/// `EV_EOF` on the write filter on macOS — which a half-close by a client that
/// is still reading never sets. A readiness error is treated as departure for
/// the same reason a write error is: the socket is finished either way.
///
/// Test: `stream_ends_when_the_peer_departs_with_its_producer_still_open`,
/// `stream_round_trips_many_frames_over_a_real_socket` (the half-close that must
/// NOT count as departure).
async fn peer_departed(socket: &UnixStream) {
    loop {
        match socket.ready(Interest::WRITABLE).await {
            Ok(ready) if ready.is_write_closed() => return,
            Err(_) => return,
            Ok(_) => tokio::time::sleep(PEER_POLL_INTERVAL).await,
        }
    }
}

/// [`crate::uds::encode_frame`] with its serde failure mapped to an io error,
/// so `write_stream` has one error type rather than two.
fn encode(frame: &RpcStreamFrame) -> std::io::Result<Vec<u8>> {
    crate::uds::encode_frame(frame)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Encode one frame and push it, flushed, so the client sees it immediately.
async fn write_frame<W>(writer: &mut W, frame: &RpcStreamFrame) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let bytes = encode(frame)?;
    writer.write_all(&bytes).await?;
    writer.flush().await
}
