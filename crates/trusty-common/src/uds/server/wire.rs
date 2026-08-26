//! The JSON-RPC 2.0 envelope [`super::RpcRouter`] reads and writes.
//!
//! Why: [`crate::uds::rpc`] is framing-only on purpose — `Req` and `Resp` are
//! whatever the caller names, and no envelope is imposed. That is right for a
//! client helper and wrong for a server that must answer an unknown method by
//! name rather than hanging up. `webhook_relay` proved the shape a server needs
//! (a `jsonrpc` version check, a method check, a coded error frame), but it
//! spells that shape out for exactly one method and binds it to a durable-
//! delivery contract. These are the same fields with the single-method
//! assumption removed.
//!
//! What: [`RpcRequest`] in, [`RpcResponse`] out, [`RpcError`] for the failure
//! half, and the five JSON-RPC codes a dispatcher can produce. The `params` and
//! `result` payloads stay `serde_json::Value` here; the caller's own request and
//! response types are decoded one layer up, in [`super::typed_method`].
//!
//! [`RpcStreamFrame`] is the multi-frame half (#6286): the envelope a streaming
//! response is written in, and the only shape that carries a `stream`
//! discriminant. A non-streaming exchange never produces one, which is what
//! keeps the two protocols distinguishable on the same socket.
//!
//! Test: `super::tests` — `dispatch_*` for the codes,
//! `stream_frames_carry_the_phase_discriminant` for the streaming shapes.

use serde::{Deserialize, Serialize};

/// The only `jsonrpc` version this server answers.
pub const JSONRPC_VERSION: &str = "2.0";

/// JSON-RPC code for a frame that is not valid JSON.
pub const CODE_PARSE_ERROR: i64 = -32700;

/// JSON-RPC code for a frame whose envelope is wrong (unsupported `jsonrpc`).
pub const CODE_INVALID_REQUEST: i64 = -32600;

/// JSON-RPC code for a method no handler is registered for.
///
/// Mirrors `webhook_relay::serve::CODE_METHOD_NOT_FOUND` deliberately: a client
/// that drifts must see the same code from every UDS server in this workspace.
pub const CODE_METHOD_NOT_FOUND: i64 = -32601;

/// JSON-RPC code for a frame whose `params` do not decode into the handler's
/// request type.
pub const CODE_INVALID_PARAMS: i64 = -32602;

/// JSON-RPC code for a handler that failed for its own reasons.
pub const CODE_INTERNAL_ERROR: i64 = -32603;

/// A request asked for a stream from a method that does not produce one (#6286).
///
/// In JSON-RPC's implementation-defined server range (`-32099..=-32000`), so it
/// cannot collide with the reserved codes above. Reported in the `error` half of
/// a terminal [`RpcStreamFrame`], because a caller that asked for a stream reads
/// stream frames and would not recognise a plain [`RpcResponse`].
pub const CODE_STREAM_UNSUPPORTED: i64 = -32010;

/// A request reached a streaming method without asking for a stream (#6286).
///
/// Reported in an ordinary [`RpcResponse`] — the caller writes one frame and
/// reads one frame, so the refusal has to arrive in the shape it can read. This
/// is what stops an old client hanging on a method that would otherwise answer
/// in frames it cannot parse.
pub const CODE_STREAM_REQUIRED: i64 = -32011;

/// One request frame, as it arrives on the wire.
///
/// Every field carries `#[serde(default)]` except `method`, so a frame missing
/// its `jsonrpc` version reaches the version check and is refused there with a
/// message naming the problem, rather than dying in serde with a message about
/// a missing field.
#[derive(Debug, Clone, Deserialize)]
pub struct RpcRequest {
    /// Protocol version. Anything but [`JSONRPC_VERSION`] is refused.
    #[serde(default)]
    pub jsonrpc: String,
    /// Correlation id, echoed verbatim on the response.
    #[serde(default)]
    pub id: serde_json::Value,
    /// Method name, matched against the router's registered handlers.
    pub method: String,
    /// Handler payload, decoded by the handler rather than here.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// One response frame.
///
/// `result` and `error` are mutually exclusive and both skipped when absent, so
/// the serialised frame matches what a JSON-RPC client expects rather than
/// carrying a `"error": null` alongside a result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    /// Always [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// The request's id, echoed back. `null` when the frame was too broken to
    /// read an id out of.
    pub id: serde_json::Value,
    /// Present when the call succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Present when the call failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    /// A successful response carrying `result`.
    pub fn success(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// A failed response carrying `error`.
    pub fn failure(id: serde_json::Value, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }

    /// Whether this frame reports a failure.
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// Which of the three things a streaming response frame is (#6286).
///
/// Why: one connection carries many frames, so a reader needs to know from the
/// frame itself whether more are coming. Encoding that as a discriminant rather
/// than as "a frame with no `result` means the end" keeps a handler free to
/// stream `null` items.
/// What: serialises as the lowercase string a `"stream"` field holds.
/// Test: `stream_frames_carry_the_phase_discriminant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamPhase {
    /// One payload; more frames follow.
    Item,
    /// The stream completed. Terminal — nothing follows on this connection.
    End,
    /// The stream failed. Terminal, and carries the reason.
    Error,
}

/// One frame of a streaming response (#6286).
///
/// Why: `POST /api/v1/chat` in `trusty-memory` streams LLM tokens, and the
/// one-frame-per-connection contract [`RpcResponse`] states cannot carry that.
/// #6287 deferred the multi-frame shape until a daemon had a real cross-crate
/// consumer; chat is that consumer.
///
/// What: the same `jsonrpc` / `id` envelope [`RpcResponse`] carries, plus a
/// [`StreamPhase`] discriminant that a plain response never has. The wire
/// contract in full:
///
/// - Zero or more `{"jsonrpc":"2.0","id":<id>,"stream":"item","result":<value>}`
/// - Then EXACTLY ONE terminal frame: `…,"stream":"end"` on success, or
///   `…,"stream":"error","error":{…}` on failure.
///
/// A stream that reaches EOF without a terminal frame is a protocol violation,
/// not an empty success — the client reports it rather than returning what it
/// happened to receive. That is the Fail-Open branch this shape exists to close:
/// a truncated token stream must never read as a complete answer.
///
/// `#[non_exhaustive]`: build one through [`RpcStreamFrame::item`],
/// [`RpcStreamFrame::end`] or [`RpcStreamFrame::error`], so a later envelope
/// field is not a breaking change.
///
/// Test: `stream_frames_carry_the_phase_discriminant`,
/// `stream_round_trips_many_frames_over_a_real_socket`,
/// `stream_reports_a_truncated_stream_rather_than_an_empty_success`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RpcStreamFrame {
    /// Always [`JSONRPC_VERSION`].
    pub jsonrpc: String,
    /// The request's id, echoed on every frame of the stream.
    pub id: serde_json::Value,
    /// Which phase this frame is. Absent from a non-streaming [`RpcResponse`],
    /// which is what lets a reader tell the two apart.
    pub stream: StreamPhase,
    /// The payload, on a [`StreamPhase::Item`] frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// The reason, on a [`StreamPhase::Error`] frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcStreamFrame {
    /// One payload frame. More frames follow.
    pub fn item(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            stream: StreamPhase::Item,
            result: Some(result),
            error: None,
        }
    }

    /// The terminal success frame.
    pub fn end(id: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            stream: StreamPhase::End,
            result: None,
            error: None,
        }
    }

    /// The terminal failure frame, carrying `error` verbatim.
    pub fn error(id: serde_json::Value, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            stream: StreamPhase::Error,
            result: None,
            error: Some(error),
        }
    }
}

/// The error half of a response frame, and what a handler returns to refuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    /// JSON-RPC error code.
    pub code: i64,
    /// Human-readable reason, reported to the caller verbatim.
    pub message: String,
}

impl RpcError {
    /// An error with a caller-chosen code.
    ///
    /// Application-defined codes belong in the JSON-RPC implementation-defined
    /// range (`-32099..=-32000`); the reserved codes above have constants.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// [`CODE_INVALID_PARAMS`] — the frame was well-formed but its `params`
    /// were not usable.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(CODE_INVALID_PARAMS, message)
    }

    /// [`CODE_INTERNAL_ERROR`] — the handler ran and failed.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(CODE_INTERNAL_ERROR, message)
    }

    /// [`CODE_METHOD_NOT_FOUND`], naming what this server does serve.
    ///
    /// Listing the registered methods is the difference between a client author
    /// seeing a typo and seeing an opaque refusal.
    pub fn method_not_found(method: &str, known: &[&str]) -> Self {
        Self::new(
            CODE_METHOD_NOT_FOUND,
            format!("unknown method {method:?}; this listener serves {known:?}"),
        )
    }

    /// [`CODE_STREAM_UNSUPPORTED`], naming what this server does stream (#6286).
    ///
    /// Listing the streaming methods is what makes this refusal actionable: it
    /// covers both "that name streams nothing" and "that name does not exist",
    /// and the caller reads which from the list.
    ///
    /// Test: `stream_request_for_a_non_streaming_method_is_refused`.
    pub fn stream_unsupported(method: &str, streaming: &[&str]) -> Self {
        Self::new(
            CODE_STREAM_UNSUPPORTED,
            format!("method {method:?} does not stream; this listener streams {streaming:?}"),
        )
    }

    /// [`CODE_STREAM_REQUIRED`] — this method answers only in a stream (#6286).
    ///
    /// Test: `unary_request_for_a_streaming_method_is_refused_in_one_frame`.
    pub fn stream_required(method: &str) -> Self {
        Self::new(
            CODE_STREAM_REQUIRED,
            format!(
                "method {method:?} answers only as a stream; resend with \"stream\": true \
                 and read frames until a terminal one"
            ),
        )
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}
