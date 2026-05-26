//! JSON-RPC 2.0 envelope types for the embed daemon.
//!
//! Why: Both the daemon and the in-process `EmbedClient` (in `trusty-common`)
//! need to agree on the wire format. Centralising the shapes here keeps the
//! protocol versioned in one place and lets the client crate copy the wire
//! shape without taking a dependency on this binary.
//!
//! What: pure serde types describing the `embed` request, the success result,
//! and the JSON-RPC error envelope. Numeric error codes follow the JSON-RPC
//! 2.0 spec where applicable (-32600 invalid request, -32601 method not
//! found, -32700 parse error, -32603 internal error).
//!
//! Test: round-trip serde tests live in this module; integration coverage in
//! `tests/embed_daemon.rs`.

use serde::{Deserialize, Serialize};

/// JSON-RPC 2.0 method name handled by the daemon.
///
/// Why: keep the method-string literal in one place so both the daemon's
/// dispatch arm and the client cannot drift.
/// What: a `&'static str` constant exposed to the rest of the crate.
/// Test: covered by the integration test which sends this exact method name.
pub const METHOD_EMBED: &str = "embed";

/// JSON-RPC version string. The protocol mandates this exact value.
pub const JSONRPC_VERSION: &str = "2.0";

// ── Error codes ────────────────────────────────────────────────────────────

/// JSON-RPC: malformed request envelope (e.g. missing required fields).
pub const ERR_INVALID_REQUEST: i32 = -32600;

/// JSON-RPC: requested method does not exist.
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;

/// JSON-RPC: payload could not be parsed as JSON at all.
pub const ERR_PARSE: i32 = -32700;

/// JSON-RPC: server-side failure while executing the method.
pub const ERR_INTERNAL: i32 = -32603;

/// Inbound JSON-RPC request envelope.
///
/// Why: the daemon reads one of these per newline-framed payload. Using a
/// dedicated struct lets serde reject malformed messages with a precise error
/// rather than parsing into a free-form `Value` and post-validating.
/// What: standard JSON-RPC 2.0 request fields. `id` is stored as
/// `serde_json::Value` so the daemon can echo the caller's id verbatim
/// (clients sometimes use strings, numbers, or `null`).
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default)]
    pub id: Option<serde_json::Value>,
}

/// Parameters for the `embed` method.
///
/// Why: typed params make decoding explicit — a missing or non-array `texts`
/// field is rejected at parse time rather than at the call boundary.
/// What: a single field — `texts: Vec<String>`. An empty vec is allowed and
/// returns an empty `embeddings` array.
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedParams {
    pub texts: Vec<String>,
}

/// Successful result for the `embed` method.
///
/// Why: callers receive one `Vec<f32>` per input text, in input order. Wrapping
/// the array in a named struct gives us forward-compat room (e.g. a `cached`
/// boolean flag per slot in a future revision).
/// What: `embeddings[i]` is the vector for `params.texts[i]`. All vectors have
/// length `trusty_common::embedder::EMBED_DIM` (384 for all-MiniLM-L6-v2).
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedResult {
    pub embeddings: Vec<Vec<f32>>,
}

/// JSON-RPC 2.0 error object.
///
/// Why: structured errors let clients distinguish "method not found" from
/// "internal failure" without string-matching on `message`.
/// What: `code` follows the JSON-RPC 2.0 numeric scheme (see the `ERR_*`
/// constants). `message` is a short human-readable description.
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

/// Outbound JSON-RPC response envelope.
///
/// Why: exactly one of `result` / `error` is populated, mirroring the JSON-RPC
/// 2.0 spec. Using `skip_serializing_if` keeps the wire output clean.
/// What: `id` echoes the request id verbatim. On parse failure (no id
/// recoverable) we emit `id = null`.
/// Test: serde round-trip in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    pub id: serde_json::Value,
}

impl RpcResponse {
    /// Build a success response.
    ///
    /// Why: keeps the boilerplate of "jsonrpc=2.0, error=None" off every call
    /// site so dispatch code reads as a straight value translation.
    /// What: wraps `result` as `Some(serde_json::Value)` and the supplied id.
    /// Test: covered by the integration test's success path.
    pub fn ok(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }

    /// Build an error response.
    ///
    /// Why: same DRY motivation as [`Self::ok`].
    /// What: wraps `error` as `Some(RpcError)` with the supplied code/message
    /// and the caller's id (or `null` when no id could be recovered).
    /// Test: covered by the integration test's error paths.
    pub fn err(id: serde_json::Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
            id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_request_round_trips() {
        let raw =
            r#"{"jsonrpc":"2.0","method":"embed","params":{"texts":["hi","there"]},"id":"abc"}"#;
        let parsed: RpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.method, "embed");
        assert_eq!(parsed.jsonrpc, "2.0");
        let params: EmbedParams = serde_json::from_value(parsed.params.unwrap()).unwrap();
        assert_eq!(params.texts, vec!["hi".to_string(), "there".to_string()]);
    }

    #[test]
    fn ok_response_serialises_without_error_field() {
        let resp = RpcResponse::ok(
            serde_json::json!("abc"),
            serde_json::json!({"embeddings":[[0.1_f32, 0.2_f32]]}),
        );
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"result\""));
        assert!(!s.contains("\"error\""));
        assert!(s.contains("\"id\":\"abc\""));
    }

    #[test]
    fn err_response_serialises_without_result_field() {
        let resp = RpcResponse::err(serde_json::Value::Null, ERR_INTERNAL, "boom");
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains("\"error\""));
        assert!(!s.contains("\"result\""));
        assert!(s.contains("\"code\":-32603"));
    }

    #[test]
    fn missing_params_decodes_as_none() {
        let raw = r#"{"jsonrpc":"2.0","method":"embed","id":1}"#;
        let parsed: RpcRequest = serde_json::from_str(raw).unwrap();
        assert!(parsed.params.is_none());
    }
}
