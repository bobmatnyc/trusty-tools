//! The `events.subscribe` request frame, and the errors decoding one reports.
//!
//! Why: a reader opens a subscription with one JSON-RPC request line on the
//!      `trusty-events` socket (ADR-0065 decision 3), and a stub daemon in a
//!      client test, or the bus's own first-line handling, must read that line
//!      back. Decoding it in each place would let the two sides disagree about
//!      what a valid subscription is. A malformed request must fail with a
//!      named reason, never fall back to a default subscription.
//! What: [`SubscribeRequest`] builds and decodes the frame
//!       `{"jsonrpc":"2.0","id":..,"method":"events.subscribe","stream":true,
//!       "params":{..}}`; [`SubscribeFrameError`] names each way a frame is
//!       refused. Gated on `unix` + `uds` like `push_client`, because the
//!       envelope is `uds::server`'s and the error type needs `thiserror`.
//! Test: `super::subscribe_tests::frame`.

// #9075: new module, PR1 of epic #9074.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::subscribe::{METHOD_EVENTS_SUBSCRIBE, SubscribeParams};
use crate::uds::server::JSONRPC_VERSION;

/// Why an `events.subscribe` request frame was refused.
///
/// Why: each arm is a distinct client bug with a distinct fix, and a server
/// answering it needs the reason; collapsing them would turn a misspelt filter
/// into "bad request".
/// What: one arm per check, in the order [`SubscribeRequest::decode`] runs
/// them, which is the order `uds::server::RpcRouter` runs its own.
/// Test: `decode_rejects_a_frame_that_is_not_json`,
/// `decode_rejects_a_wrong_jsonrpc_version`, `decode_rejects_another_method`,
/// `decode_rejects_a_unary_subscribe`, `decode_rejects_an_unknown_filter_field`,
/// `decode_rejects_missing_params`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SubscribeFrameError {
    /// Not a JSON object with a string `method`.
    #[error("events.subscribe frame is not a JSON-RPC request: {0}")]
    Malformed(#[source] serde_json::Error),
    /// `jsonrpc` is not `"2.0"`.
    #[error("events.subscribe frame has jsonrpc {found:?}; expected \"2.0\"")]
    UnsupportedVersion {
        /// The version the frame carried.
        found: String,
    },
    /// The frame calls a different method.
    #[error("frame calls {found:?}, not \"events.subscribe\"")]
    WrongMethod {
        /// The method the frame named.
        found: String,
    },
    /// `"stream": true` is absent, so the server would answer once and close.
    #[error("events.subscribe answers only as a stream; send \"stream\": true")]
    NotStreaming,
    /// `params` is absent or does not decode, including an unknown filter field.
    #[error("events.subscribe params rejected: {0}")]
    InvalidParams(#[source] serde_json::Error),
}

/// One `events.subscribe` request, as written on the bus socket.
///
/// Why: the client writes it and a test stub or the daemon reads it; one type
/// for both directions keeps the golden frame the only definition.
/// What: serialises with `jsonrpc`, `method` and `stream` fixed to the values
/// the contract requires; only `id` and `params` vary. [`Self::decode`]
/// validates a received line and rebuilds the same value.
/// Test: `subscribe_request_golden_roundtrip`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SubscribeRequest {
    jsonrpc: &'static str,
    /// Correlation id, echoed on every frame of the stream.
    pub id: Value,
    method: &'static str,
    stream: bool,
    /// What to replay and which events to deliver.
    pub params: SubscribeParams,
}

/// The fields a request line is read through before it is validated.
///
/// Every field but `method` defaults, so a missing `jsonrpc` or `stream`
/// reaches its own check and is refused by name.
#[derive(Deserialize)]
struct WireRequest {
    #[serde(default)]
    jsonrpc: String,
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    params: Value,
}

impl SubscribeRequest {
    /// A streaming `events.subscribe` request with this `id` and `params`.
    pub fn new(id: impl Into<Value>, params: SubscribeParams) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            id: id.into(),
            method: METHOD_EVENTS_SUBSCRIBE,
            stream: true,
            params,
        }
    }

    /// Decode and validate one request line.
    ///
    /// Why: a stub daemon in a client test, and the bus itself, must refuse a
    /// bad subscription by name instead of serving a default one.
    /// What: parses `frame`, then checks `jsonrpc`, `method`, the stream flag
    /// and `params`, returning the first failure. Absent `params` is refused,
    /// as `RpcRouter::typed_stream` refuses it; send `{}` to subscribe to
    /// everything live.
    ///
    /// # Errors
    ///
    /// The [`SubscribeFrameError`] arm for the first check that fails.
    ///
    /// Test: `subscribe_request_golden_roundtrip` and the `decode_rejects_*`
    /// tests in `super::subscribe_tests::frame`.
    pub fn decode(frame: &[u8]) -> Result<Self, SubscribeFrameError> {
        let wire: WireRequest =
            serde_json::from_slice(frame).map_err(SubscribeFrameError::Malformed)?;
        if wire.jsonrpc != JSONRPC_VERSION {
            return Err(SubscribeFrameError::UnsupportedVersion {
                found: wire.jsonrpc,
            });
        }
        if wire.method != METHOD_EVENTS_SUBSCRIBE {
            return Err(SubscribeFrameError::WrongMethod { found: wire.method });
        }
        if !wire.stream {
            return Err(SubscribeFrameError::NotStreaming);
        }
        let params =
            serde_json::from_value(wire.params).map_err(SubscribeFrameError::InvalidParams)?;
        Ok(Self::new(wire.id, params))
    }
}
