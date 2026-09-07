//! One framed JSON-RPC stream, rendered as the Server-Sent Events a browser
//! reads (#6155).
//!
//! Why: `search_uds` built this bridge for trusty-search and `memory_uds` needs
//! exactly the same thing for trusty-memory — the frame-to-`data:` encoding, the
//! keep-alive comment, the cancel-safe reader task, and the response head are
//! identical for both, because none of them is about which daemon is on the
//! other end. A second copy is how one bridge starts closing a failed stream
//! silently while the other reports it.
//!
//! What is NOT here: anything a service owns. Which method is streaming, which
//! JSON-RPC code becomes which HTTP status, and what the peek before the
//! response head means all stay in the per-service module, because the two
//! daemons answer with different code tables.
//!
//! ## The contract this preserves
//!
//! One stream ITEM is exactly the JSON document one SSE `data:` line carried,
//! parsed rather than prefixed. So this re-prefixes it and adds nothing: every
//! event reaches the browser byte-identical to what the daemon's own SSE route
//! wrote before ADR-0032 retired it.
//!
//! Two things those SSE routes emitted that an RPC stream does not, and what
//! happens to them here:
//!
//! - the `: heartbeat\n\n` comment every 20 s. It exists so an idle TCP body is
//!   not torn down, and the browser hop is still TCP — so this emits it, on the
//!   same interval.
//! - the terminal `data:` framing of a failure. A mid-stream failure becomes one
//!   `{"type":"error","message":…}` event before the body closes, because a
//!   consumer reads a closed stream as a COMPLETED operation and a silent close
//!   would report a broken one as finished.
//!
//! Test: `sse_data_is_one_line_per_event` below, plus
//! `tests/search_uds_bridge.rs` and `tests/memory_uds_bridge.rs`, which drive
//! whole streams through the real router.

use axum::body::{Body, Bytes};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt as _;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::warn;
use trusty_common::uds::UdsRpcError;
use trusty_common::uds::stream_client::FramedStream;

/// How often an open stream emits an SSE keep-alive comment.
///
/// The same 20 s the daemons' own SSE routes used, so an idle browser
/// connection sees the byte sequence it saw before the migration.
const SSE_HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);

/// How many stream items may buffer between the socket reader and the browser.
///
/// Matches the daemon-side producer buffer, so neither side is the first to
/// accumulate behind a slow reader.
const SSE_BUFFER: usize = 64;

/// Build the `200 text/event-stream` response for an already-opened stream.
///
/// Why the caller passes `first` separately: a streaming method can refuse, and
/// that refusal arrives as the stream's FIRST frame after the dial has already
/// succeeded. The caller peeks it so a refusal becomes an HTTP status rather
/// than an empty `200`; the peeked item then has to lead the body, which is what
/// this takes it for. `None` is a stream that ended with no items — a
/// well-formed empty answer, and an immediately-closed event stream.
/// What: the peeked item, then [`sse_tail`], under the three headers a browser
/// and any reverse proxy in front of the console need.
/// Test: `tests/memory_uds_bridge.rs`'s `a_stream_reaches_the_browser_frame_for_frame`.
pub(crate) fn sse_response(
    first: Option<Value>,
    stream: FramedStream<Value>,
    method: &'static str,
) -> Response {
    let head = futures_util::stream::iter(
        first
            .into_iter()
            .map(|item| Ok::<Bytes, std::convert::Infallible>(sse_data(&item))),
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        // The same header the daemons' SSE routes set, so a reverse proxy in
        // front of the console does not buffer the stream into uselessness.
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(head.chain(sse_tail(stream, method))))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// The rest of an open stream, as SSE frames plus keep-alive comments.
///
/// Why the reader runs in its own task rather than inside the `select!`:
/// `FramedStream::next_frame` reads a line off a `BufReader`, and cancelling
/// that mid-line — which a heartbeat tick would do — discards the bytes already
/// read. Moving the read behind an `mpsc` makes both arms of the select
/// cancel-safe, since `Receiver::recv` and `Interval::tick` both are.
///
/// The task also carries the disconnect signal: when the browser goes, axum
/// drops this body and the receiver drops. The read itself selects on
/// `Sender::closed()` so the task notices immediately rather than at the next
/// frame — a status stream can be silent for minutes, and waiting for a frame
/// that will never come would hold the socket, and the daemon's producer behind
/// it, open for exactly that long. Either way the task returns, dropping the
/// `FramedStream` and closing the socket, which is what ends the producer.
///
/// Test: `tests/search_uds_bridge.rs`'s `a_mid_stream_failure_becomes_an_error_event`
/// and `a_browser_disconnect_releases_the_daemon_socket`.
fn sse_tail(
    mut stream: FramedStream<Value>,
    method: &'static str,
) -> impl futures_util::Stream<Item = Result<Bytes, std::convert::Infallible>> {
    let (tx, rx) = mpsc::channel::<Result<Value, UdsRpcError>>(SSE_BUFFER);
    tokio::spawn(async move {
        loop {
            // Cancelling `next_frame` mid-line discards the bytes already read,
            // which only matters to a reader that resumes. This arm never
            // resumes: it returns, and the `FramedStream` is dropped with it.
            let item = tokio::select! {
                biased;
                () = tx.closed() => return,
                item = stream.next_frame() => item,
            };
            let Some(item) = item else { return };
            let terminal = item.is_err();
            if tx.send(item).await.is_err() || terminal {
                return;
            }
        }
    });

    let heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + SSE_HEARTBEAT_INTERVAL,
        SSE_HEARTBEAT_INTERVAL,
    );

    futures_util::stream::unfold(Some((rx, heartbeat)), move |state| async move {
        let (mut rx, mut heartbeat) = state?;
        tokio::select! {
            biased;
            item = rx.recv() => match item {
                Some(Ok(value)) => Some((Ok(sse_data(&value)), Some((rx, heartbeat)))),
                Some(Err(e)) => {
                    // #6285: never a silent close. See the module docs.
                    warn!("uds_sse: {method} failed mid-stream: {e}");
                    let event = json!({ "type": "error", "message": e.to_string() });
                    Some((Ok(sse_data(&event)), None))
                }
                None => None,
            },
            _ = heartbeat.tick() => Some((
                Ok(Bytes::from_static(b": heartbeat\n\n")),
                Some((rx, heartbeat)),
            )),
        }
    })
}

/// Encode one stream item as an SSE `data:` frame.
///
/// Why `to_string` on a `Value` rather than passing the raw line through: the
/// stream carries parsed JSON, and re-serialising is what puts it back on one
/// line — an embedded newline would split one event into two.
/// Test: `sse_data_is_one_line_per_event`.
fn sse_data(value: &Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: an event carrying a newline inside a string would split into two SSE
    /// events and neither would parse.
    /// Test: this is the test.
    #[test]
    fn sse_data_is_one_line_per_event() {
        let framed = sse_data(&json!({ "message": "a\nb" }));
        let text = String::from_utf8(framed.to_vec()).expect("utf-8");
        assert_eq!(text, "data: {\"message\":\"a\\nb\"}\n\n");
        assert_eq!(text.matches("\n\n").count(), 1);
    }
}
