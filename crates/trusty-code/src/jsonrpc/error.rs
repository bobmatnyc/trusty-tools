//! Typed JSON-RPC error returned by [`super::router::MethodHandler`] impls.
//!
//! Why: handlers need a lightweight, typed way to signal a specific JSON-RPC
//! error code (invalid params, internal error, …) without the router having
//! to string-sniff an `anyhow::Error`. Centralising the constructors here
//! means every handler produces spec-correct codes by construction.
//! What: `RpcError { code, message, data }` plus constructors for the
//! standard JSON-RPC 2.0 codes a handler is expected to return itself, and
//! (#2054) the vision spec's domain error taxonomy (§13.2) — an
//! *additive* layer of application-specific codes carried in the same
//! envelope, each tagged with `data.error_type` so clients can match on a
//! stable string rather than a numeric code alone. `-32601 Method not
//! found` and `-32600 Invalid Request` are produced by
//! [`super::router::Router`] before a handler is ever invoked, so they are
//! intentionally not exposed as constructors here — a handler has no
//! legitimate reason to fabricate either.
//! Test: this module's unit tests.

use serde_json::{Value, json};
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

    /// Build a domain-taxonomy error (vision spec §13.2): a code from the
    /// spec's table plus a stable `error_type` string tagged onto `data`.
    ///
    /// Why: the spec's domain errors (`session_not_found`, `invalid_argument`,
    /// `cancelled`, …) live in a numeric range distinct from the standard
    /// JSON-RPC `-326xx` codes, and every one of them carries
    /// `data.error_type` so a client can match on the string rather than
    /// memorising numbers. Centralising the shape here means every domain
    /// error is built the same way.
    /// What: sets `code`, `message`, and `data = {"error_type": error_type}`.
    /// Test: `rpc_error_domain_sets_error_type_in_data`.
    fn domain(code: i32, error_type: &'static str, message: impl Into<String>) -> Self {
        Self::new(code, message).with_data(json!({"error_type": error_type}))
    }

    /// Build a `-32007 session_not_found` error (vision spec §13.2).
    ///
    /// Why: the specific, spec-named code for "the session id in `params`
    /// does not exist in the registry" — used by every `session.*` method
    /// that takes a `session_id`.
    /// What: `message` embeds `session_id` for operator-visible logs;
    /// `data.error_type = "session_not_found"`.
    /// Test: `rpc_error_session_not_found_sets_code_and_type`.
    pub fn session_not_found(session_id: &str) -> Self {
        Self::domain(
            -32007,
            "session_not_found",
            format!("session not found: {session_id}"),
        )
    }

    /// Build a `-32002 not_found` error (vision spec §13.2).
    ///
    /// Why: the spec's general "the thing you named does not exist" code,
    /// distinct from the session-specific `-32007 session_not_found` — used by
    /// methods whose subject is not a session (e.g. `fs.list_dir` on a path
    /// that isn't there).
    /// What: `data.error_type = "not_found"`.
    /// Test: `rpc_error_not_found_sets_code_and_type`.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::domain(-32002, "not_found", message)
    }

    /// Build a `-32001 permission_denied` error (vision spec §13.2).
    ///
    /// Why: the spec's "caller lacks permission" code. tcode adds no permission
    /// layer of its own — it runs as the user with the user's entitlements — so
    /// this reports an OS-level refusal (e.g. an unreadable directory) rather
    /// than a policy decision tcode made.
    /// What: `data.error_type = "permission_denied"`.
    /// Test: `rpc_error_permission_denied_sets_code_and_type`.
    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self::domain(-32001, "permission_denied", message)
    }

    /// Build a `-32003 invalid_argument` error (vision spec §13.2).
    ///
    /// Why: the spec's domain-level "malformed request" code, distinct from
    /// the JSON-RPC protocol-level `-32602 Invalid params` — used when a
    /// method's arguments parse as JSON but fail a semantic check (e.g. an
    /// empty task description).
    /// What: `data.error_type = "invalid_argument"`.
    /// Test: `rpc_error_invalid_argument_sets_code_and_type`.
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::domain(-32003, "invalid_argument", message)
    }

    /// Build a `-32008 active_conflict` error (DOC-48 §5.2/§6.1, issue #3294).
    ///
    /// Why: `workstream.activate{id, force: false}` must fail when a
    /// DIFFERENT workstream is already active, and the caller needs the
    /// currently-active id to decide whether to retry with `force: true`
    /// (§6.1: "the error includes the ID of the currently-active
    /// workstream"). `-32008` is the next free slot after `-32007
    /// session_not_found` in this crate's domain-error range — DOC-48
    /// predates no reservation for it in the vision spec's §13.2 table, so
    /// it is allocated here rather than colliding with the reserved-but-
    /// unimplemented `-32004`/`-32005`/`-32006` (`timeout`/`cancelled`/
    /// `provider_failure`).
    /// What: `data = {"error_type": "active_conflict", "active_id": <uuid
    /// string>}` — `active_id` rides alongside the standard `error_type` tag
    /// so a client can read the conflicting id without re-parsing `message`.
    /// Test: `rpc_error_active_conflict_sets_code_and_active_id`.
    pub fn active_conflict(active_id: impl std::fmt::Display) -> Self {
        let active_id = active_id.to_string();
        Self::new(-32008, format!("another workstream is active: {active_id}"))
            .with_data(json!({"error_type": "active_conflict", "active_id": active_id}))
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

    /// `session_not_found` must use the spec's `-32007` code, embed the id
    /// in the message, and tag `error_type` in `data`.
    #[test]
    fn rpc_error_session_not_found_sets_code_and_type() {
        let e = RpcError::session_not_found("s-123");
        assert_eq!(e.code, -32007);
        assert!(e.message.contains("s-123"));
        assert_eq!(e.data, Some(json!({"error_type": "session_not_found"})));
    }

    /// `not_found` must use the spec's `-32002` code and tag `error_type`.
    #[test]
    fn rpc_error_not_found_sets_code_and_type() {
        let e = RpcError::not_found("path not found: /x");
        assert_eq!(e.code, -32002);
        assert_eq!(e.data, Some(json!({"error_type": "not_found"})));
    }

    /// `permission_denied` must use the spec's `-32001` code and tag
    /// `error_type`.
    #[test]
    fn rpc_error_permission_denied_sets_code_and_type() {
        let e = RpcError::permission_denied("permission denied: /x");
        assert_eq!(e.code, -32001);
        assert_eq!(e.data, Some(json!({"error_type": "permission_denied"})));
    }

    /// `invalid_argument` must use the spec's `-32003` code and tag
    /// `error_type` in `data`.
    #[test]
    fn rpc_error_invalid_argument_sets_code_and_type() {
        let e = RpcError::invalid_argument("task must not be empty");
        assert_eq!(e.code, -32003);
        assert_eq!(e.data, Some(json!({"error_type": "invalid_argument"})));
    }

    /// `active_conflict` must use `-32008`, tag `error_type`, and carry the
    /// conflicting `active_id` in `data` (DOC-48 §6.1).
    #[test]
    fn rpc_error_active_conflict_sets_code_and_active_id() {
        let e = RpcError::active_conflict("a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b");
        assert_eq!(e.code, -32008);
        assert!(e.message.contains("a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b"));
        assert_eq!(
            e.data,
            Some(json!({
                "error_type": "active_conflict",
                "active_id": "a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b",
            }))
        );
    }
}
