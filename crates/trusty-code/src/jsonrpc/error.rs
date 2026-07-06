//! Typed JSON-RPC error returned by [`super::router::MethodHandler`] impls.
//!
//! Why: handlers need a lightweight, typed way to signal a specific JSON-RPC
//! error code (invalid params, internal error, …) without the router having
//! to string-sniff an `anyhow::Error`. Centralising the constructors here
//! means every handler produces spec-correct codes by construction.
//! What: `RpcError { code, message, data }` plus constructors for the two
//! codes a handler is expected to return itself. `-32601 Method not found`
//! and `-32600 Invalid Request` are produced by [`super::router::Router`]
//! before a handler is ever invoked, so they are intentionally not exposed
//! as constructors here — a handler has no legitimate reason to fabricate
//! either.
//! Test: this module's unit tests.

use serde_json::Value;
use trusty_common::mcp::error_codes;

/// A JSON-RPC 2.0 error, as returned by a method handler.
///
/// Why: mirrors `trusty_common::mcp::JsonRpcError`'s shape so the router can
/// convert one into the other with a straight field copy, but stays a
/// separate type so handler code never has to construct the wire envelope
/// directly.
/// What: `code` is a JSON-RPC error code (spec-defined or application
/// range); `message` is human-readable; `data` is optional structured detail
/// attached to the wire error.
/// Test: `rpc_error_invalid_params_sets_code`, `rpc_error_internal_sets_code`.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    /// Construct an `RpcError` with an arbitrary code.
    ///
    /// Why: escape hatch for application-defined codes outside the standard
    /// JSON-RPC range (e.g. a future domain-specific error).
    /// What: sets `code`/`message`, leaves `data` unset.
    /// Test: `rpc_error_new_has_no_data_by_default`.
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Attach structured `data` to an existing error.
    ///
    /// Why: some failures carry machine-readable detail (e.g. which field
    /// failed validation) beyond the human-readable `message`.
    /// What: builder-style setter; returns `self` for chaining.
    /// Test: `rpc_error_with_data_attaches_payload`.
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// Build a `-32602 Invalid params` error.
    ///
    /// Why: the standard code a handler returns when it cannot parse or
    /// validate its `params` argument.
    /// What: `code = error_codes::INVALID_PARAMS`.
    /// Test: `rpc_error_invalid_params_sets_code`.
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(error_codes::INVALID_PARAMS, message)
    }

    /// Build a `-32603 Internal error` error.
    ///
    /// Why: the standard code for a handler-side failure that isn't the
    /// caller's fault (I/O failure, downstream panic-free error, etc.).
    /// What: `code = error_codes::INTERNAL_ERROR`.
    /// Test: `rpc_error_internal_sets_code`.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(error_codes::INTERNAL_ERROR, message)
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `invalid_params` must use the spec's `-32602` code.
    #[test]
    fn rpc_error_invalid_params_sets_code() {
        let e = RpcError::invalid_params("bad shape");
        assert_eq!(e.code, error_codes::INVALID_PARAMS);
        assert_eq!(e.message, "bad shape");
        assert!(e.data.is_none());
    }

    /// `internal` must use the spec's `-32603` code.
    #[test]
    fn rpc_error_internal_sets_code() {
        let e = RpcError::internal("boom");
        assert_eq!(e.code, error_codes::INTERNAL_ERROR);
        assert_eq!(e.message, "boom");
    }

    /// `new` alone must not attach any `data`.
    #[test]
    fn rpc_error_new_has_no_data_by_default() {
        let e = RpcError::new(-1, "custom");
        assert_eq!(e.code, -1);
        assert!(e.data.is_none());
    }

    /// `with_data` must attach the supplied payload.
    #[test]
    fn rpc_error_with_data_attaches_payload() {
        let e = RpcError::invalid_params("bad field").with_data(json!({"field": "name"}));
        assert_eq!(e.data, Some(json!({"field": "name"})));
    }

    /// `Display` must surface both the code and the message for logs.
    #[test]
    fn rpc_error_display_contains_code_and_message() {
        let e = RpcError::internal("db unreachable");
        let s = e.to_string();
        assert!(s.contains("-32603"));
        assert!(s.contains("db unreachable"));
    }
}
