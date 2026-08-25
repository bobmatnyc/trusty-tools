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
//! Test: `super::tests` — `dispatch_*` for the codes, `wire_*` for the shapes.

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
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}
