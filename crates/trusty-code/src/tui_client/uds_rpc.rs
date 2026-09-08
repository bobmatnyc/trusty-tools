//! [`UdsRpcClient`]: the JSON-RPC client `tcode tui` drives the daemon with,
//! over the daemon's hardened Unix socket (#6637).
//!
//! Why: this replaces `RpcHttpClient`, which spoke `POST {base}/rpc` and
//! carried the #5439 bearer token because a loopback TCP port is reachable by
//! any process on the machine. The socket is `0600` inside a `0700` directory
//! and every accepted connection runs a peer-uid check, so there is no
//! credential to resolve, no `Authorization` header to attach, and no
//! `TCODE_DAEMON_URL` to discover — the path is
//! `trusty_common::daemon_addr::daemon_socket_path`, which is the same one
//! `crate::serve::uds` binds.
//!
//! What: [`UdsRpcClient::call`] keeps the exact `(method, params) ->
//! Result<Value, EngineError>` signature `engine.rs` and `engine_state.rs`
//! already call, so the transport swap is invisible above this file.
//! [`UdsRpcClient::open_stream`] is the streaming half that replaces the SSE
//! reader: one dial per stream, then frames until the server writes its
//! terminal one.
//!
//! **Every call is its own connection**, unlike the pooled `reqwest::Client`
//! this replaces. That is the transport's shape, not a regression: a UDS
//! connect is a couple of syscalls against a local inode, with no handshake to
//! amortise, and `trusty_common::uds::server`'s wire contract is one request
//! frame per connection.
//!
//! Test: `engine_tests::*` for the parts that need no daemon;
//! `tests/tui_client_engine.rs` drives this against a real socket.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use trusty_common::uds::{FramedStream, send_framed_request, send_framed_stream_request};
use trusty_mcp::Response;

use super::error::EngineError;

/// How long one unary call waits for its response frame.
///
/// Every method this client calls (`session.*`, `task.run`, `workstream.*`) is
/// a fast, synchronous handler — `task.run` reserves its execution slot and
/// returns rather than blocking on the model — so a hang here means a real
/// daemon-side regression, not a slow but legitimate call.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a stream may go without a frame before the reader gives up.
///
/// Why fifteen minutes rather than the forty-five seconds the SSE reader used:
/// axum sent a keep-alive comment roughly every fifteen seconds, so silence on
/// the wire genuinely meant a dead connection. This transport sends nothing
/// between frames, so a quiet session — one long tool call, a workstream with
/// no activity — would be cut by a short budget. A socket whose peer is gone
/// reports a read failure immediately rather than timing out, so this bounds
/// one thing: a daemon that accepted the connection and then wedged.
///
/// It is what keeps #3494's property — a hung daemon must not hang the client
/// forever — true on this transport. The HTTP shape of that bug (accepted, but
/// response headers never sent) has no dial-time analogue here: a UDS connect
/// against a bound socket either completes at once or fails at once. What
/// remains is silence after the connection is up, which this bounds and
/// `session_events_tests::session_stream_silence_is_bounded_not_infinite`
/// asserts. Timing out is cheap on both streams: `session.events` reconnects
/// with `after_seq` and loses nothing, `workstream.events` re-subscribes bare.
pub const STREAM_FRAME_TIMEOUT: Duration = Duration::from_secs(900);

/// See module docs.
pub struct UdsRpcClient {
    socket: PathBuf,
    next_id: AtomicI64,
    stream_frame_timeout: Duration,
}

impl UdsRpcClient {
    /// A client dialling `socket`.
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            next_id: AtomicI64::new(1),
            stream_frame_timeout: STREAM_FRAME_TIMEOUT,
        }
    }

    /// Override the per-frame stream budget.
    ///
    /// Why: [`STREAM_FRAME_TIMEOUT`] is fifteen minutes, and
    /// `session_events_tests::session_stream_silence_is_bounded_not_infinite`
    /// has to observe the budget expire. Spending fifteen real minutes to
    /// prove a bound exists is not a test; a paused clock cannot help either,
    /// because the reader is parked on a socket read rather than on a timer.
    pub fn with_stream_frame_timeout(mut self, timeout: Duration) -> Self {
        self.stream_frame_timeout = timeout;
        self
    }

    /// The socket this client dials — read by callers building an error
    /// message or a reconnect, never to bypass this client.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Call `method` with `params`, waiting (bounded by
    /// [`DEFAULT_CALL_TIMEOUT`]) for the one response frame.
    ///
    /// Why: the single call every `session.*`/`task.*`/`workstream.*` RPC in
    /// this engine goes through.
    /// What: mints a monotonic id, writes one JSON-RPC frame, and returns
    /// `Ok(result)` or maps the JSON-RPC error object onto
    /// [`EngineError::Rpc`]. A dial, write, read or decode failure maps onto
    /// [`EngineError::Transport`].
    ///
    /// # Errors
    ///
    /// [`EngineError::Transport`] for a transport failure, [`EngineError::Rpc`]
    /// for the daemon's own error envelope, [`EngineError::Malformed`] for a
    /// response carrying neither `result` nor `error`.
    ///
    /// Test: `tests/tui_client_engine.rs::tui_engine_end_to_end_with_http_port_closed`.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, EngineError> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": self.next_id.fetch_add(1, Ordering::Relaxed),
            "method": method,
            "params": params,
        });
        let response: Response = send_framed_request(&self.socket, &request, DEFAULT_CALL_TIMEOUT)
            .await
            .map_err(|source| EngineError::Transport {
                socket: self.socket.clone(),
                source: Box::new(source),
            })?;
        match (response.result, response.error) {
            (Some(result), _) => Ok(result),
            (None, Some(error)) => Err(EngineError::Rpc {
                code: error.code,
                message: error.message,
                data: error.data,
            }),
            (None, None) => Err(EngineError::Malformed(format!(
                "{method}: response carried neither result nor error"
            ))),
        }
    }

    /// Open a streaming call and return its frame reader.
    ///
    /// What: sets the `"stream": true` flag
    /// `trusty_common::uds::server`'s wire contract negotiates on, and hands
    /// back a [`FramedStream`] whose per-frame budget is
    /// [`STREAM_FRAME_TIMEOUT`]. A method that does not stream answers one
    /// ordinary response frame, which the reader reports as
    /// `UdsRpcError::NotAStream` rather than decoding as an item.
    ///
    /// # Errors
    ///
    /// [`EngineError::Transport`] for a dial or write failure. Everything after
    /// the stream opens is reported per frame, by the reader.
    ///
    /// Test: `tests/tui_client_engine.rs::tui_engine_end_to_end_with_http_port_closed`.
    pub async fn open_stream<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<FramedStream<T>, EngineError> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": self.next_id.fetch_add(1, Ordering::Relaxed),
            "method": method,
            "params": params,
            "stream": true,
        });
        send_framed_stream_request(&self.socket, &request, self.stream_frame_timeout)
            .await
            .map_err(|source| EngineError::Transport {
                socket: self.socket.clone(),
                source: Box::new(source),
            })
    }
}
