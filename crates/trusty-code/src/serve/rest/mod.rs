//! REST resource gateway over the JSON-RPC method registry (#2983, #587,
//! Slice 1: bridge only — no routes).
//!
//! Why: the vision spec (#587) wants `tcode serve --http` to expose a
//! conventional REST resource vocabulary (`GET /projects`, `POST
//! /sessions`, …) alongside the existing JSON-RPC `POST /rpc` endpoint, but
//! every one of those resources is already backed by a registered JSON-RPC
//! method (`session.create`, `task.run`, …). Re-implementing the same
//! business logic a second time as bespoke axum handlers would fork the two
//! surfaces the moment either one changed — exactly the drift `trusty-code`'s
//! `Router` was built to prevent between the STDIO and HTTP JSON-RPC
//! transports (see `crate::jsonrpc` module docs). This module is
//! `trusty-memory`'s "Pattern C" (`crates/trusty-memory/src/web/rpc.rs`:
//! one shared dispatch seam, many transports) applied one layer further:
//! a REST route handler will call [`call`] instead of building its own
//! `Request`, so a REST resource and its JSON-RPC method twin always run
//! the exact same handler.
//!
//! What: [`call`] synthesizes a JSON-RPC [`Request`] from a `method` +
//! `params` pair and drives it through [`Router::dispatch`], unwrapping the
//! `Response` envelope back into a plain `Result<Value, RpcError>` — the
//! shape a REST handler wants (`?` onto an HTTP error), rather than the
//! always-200 envelope `POST /rpc` returns. [`rpc_error_to_status`] maps an
//! `RpcError` onto the real HTTP status a REST response should carry,
//! deliberately diverging from `POST /rpc`'s "always 200, error in the
//! envelope" convention — matching how `GET /sessions/{id}/events`
//! (`crate::serve::http::session_events_sse`) already returns a real `404`
//! for `session_not_found` rather than a 200-wrapped error.
//!
//! This slice wires no axum routes — [`crate::serve::mod`] only declares
//! `mod rest;` so the bridge compiles and is unit-testable on its own.
//! Concrete resource routes (`GET /projects`, `POST /sessions`, …) land in
//! S2-S6, each calling [`call`] + [`rpc_error_to_status`] instead of talking
//! to the `Router` directly.
//!
//! Test: `tests::*` — a success round-trip, an error round-trip, and one
//! assertion per `rpc_error_to_status` mapping.

use serde_json::Value;
use trusty_common::mcp::{Request, error_codes};

use crate::jsonrpc::{ConnectionContext, Router, RpcError};

/// Call a registered JSON-RPC method from a REST handler, in-process.
///
/// Why: the single seam every future REST resource handler goes through, so
/// a REST route and its JSON-RPC method twin are provably the same code
/// path — never two handwritten implementations of the same business logic
/// (see module docs).
/// What: synthesizes a `Request { jsonrpc: "2.0", id: Some(0), method,
/// params }` — the `id` is a fixed synthetic value because a REST caller
/// never observes it; `dispatch` never suppresses a response as long as
/// `id` is `Some(_)` (see `Router::dispatch`'s notification rule) — calls
/// `router.dispatch(req, ctx)`, and unwraps the `Response` envelope: `Ok`
/// when `result` is set, `Err` reconstructing the `RpcError` from
/// `Response::error` otherwise. If `dispatch` ever returned a `Response`
/// with neither `result` nor `error` set — a `Router::dispatch` bug, not a
/// caller-triggerable condition, and provably unreachable through this
/// bridge since `call` always sets a non-`None` `id`, so `dispatch` never
/// returns `Response::suppressed()` — this falls back to `Ok(Value::Null)`
/// rather than panicking (no `unwrap`/`expect` in library code). That would
/// surface as a silently-wrong 200 response, not a panic.
/// Test: `call_success_returns_handler_result`, `call_error_returns_rpc_error`.
pub async fn call(
    router: &Router,
    method: &str,
    params: Value,
    ctx: &ConnectionContext,
) -> Result<Value, RpcError> {
    let req = Request {
        jsonrpc: Some("2.0".to_string()),
        id: Some(Value::from(0)),
        method: method.to_string(),
        params: Some(params),
    };
    let resp = router.dispatch(req, ctx).await;
    if let Some(err) = resp.error {
        let mut rpc_err = RpcError::new(err.code, err.message);
        rpc_err.data = err.data;
        return Err(rpc_err);
    }
    Ok(resp.result.unwrap_or(Value::Null))
}

/// Map an [`RpcError`] onto the HTTP status a REST response should carry.
///
/// Why: `POST /rpc` always returns HTTP 200 with the JSON-RPC error carried
/// in the envelope — appropriate for a JSON-RPC transport, but REST callers
/// (and REST tooling: curl, browsers, API gateways) expect the status line
/// itself to say "not found" / "forbidden" / etc. This mirrors the mapping
/// `crate::serve::http::session_events_sse` already applies by hand for
/// `session_not_found` -> 404, generalised to every domain error code
/// `crate::jsonrpc::error::RpcError` currently defines (vision spec §13.2).
/// What: matches on the numeric JSON-RPC error code: `-32002 not_found` ->
/// 404, `-32001 permission_denied` -> 403, `-32003 invalid_argument` and
/// `-32602 Invalid params` -> 400, `-32007 session_not_found` -> 404,
/// `-32603 Internal error` -> 500. Any other/future code (including
/// `-32601`/`-32600`, which a REST handler should never see because it
/// calls `call` with a hardcoded, known-good method name) falls back to 500
/// — an unmapped domain error is a bug to fix by adding a mapping, not a
/// client-facing 4xx.
/// Test: `status_mapping_*` — one case per mapped code, plus the unknown ->
/// 500 fallback.
pub fn rpc_error_to_status(err: &RpcError) -> axum::http::StatusCode {
    use axum::http::StatusCode;

    match err.code {
        -32002 => StatusCode::NOT_FOUND,   // not_found
        -32007 => StatusCode::NOT_FOUND,   // session_not_found
        -32001 => StatusCode::FORBIDDEN,   // permission_denied
        -32003 => StatusCode::BAD_REQUEST, // invalid_argument
        c if c == error_codes::INVALID_PARAMS => StatusCode::BAD_REQUEST,
        c if c == error_codes::INTERNAL_ERROR => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::mpsc;

    fn test_ctx() -> ConnectionContext {
        let (tx, _rx) = mpsc::unbounded_channel();
        ConnectionContext::new(tx)
    }

    fn echo_router() -> Router {
        let mut router = Router::new();
        router.register(
            "echo",
            |params: Value, _ctx: ConnectionContext| async move { Ok(params) },
        );
        router.register(
            "boom.not_found",
            |_params: Value, _ctx: ConnectionContext| async move {
                Err(RpcError::not_found("thing not found: x"))
            },
        );
        router
    }

    /// `call` on a registered method must return `Ok` with the handler's
    /// result, unwrapped from the `Response` envelope.
    #[tokio::test]
    async fn call_success_returns_handler_result() {
        let router = echo_router();
        let result = call(&router, "echo", json!({"a": 1}), &test_ctx())
            .await
            .expect("echo must succeed");
        assert_eq!(result, json!({"a": 1}));
    }

    /// `call` on a handler that returns an `RpcError` must surface that
    /// exact error (code + message + data) as `Err`, not swallow it into
    /// the `Ok` branch.
    #[tokio::test]
    async fn call_error_returns_rpc_error() {
        let router = echo_router();
        let err = call(&router, "boom.not_found", Value::Null, &test_ctx())
            .await
            .expect_err("boom.not_found must fail");
        assert_eq!(err.code, -32002);
        assert!(err.message.contains("thing not found"));
        assert_eq!(err.data, Some(json!({"error_type": "not_found"})));
    }

    /// `call` on an unknown method must surface `-32601 Method not found`
    /// like any other `Router::dispatch` caller — `call` does not swallow
    /// or re-map dispatcher-level errors, only handler-level ones.
    #[tokio::test]
    async fn call_unknown_method_returns_method_not_found() {
        let router = echo_router();
        let err = call(&router, "does.not.exist", Value::Null, &test_ctx())
            .await
            .expect_err("unknown method must fail");
        assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
    }

    /// Every currently-mapped `RpcError` code must resolve to its documented
    /// HTTP status.
    #[test]
    fn status_mapping_not_found_is_404() {
        assert_eq!(
            rpc_error_to_status(&RpcError::not_found("x")),
            axum::http::StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn status_mapping_session_not_found_is_404() {
        assert_eq!(
            rpc_error_to_status(&RpcError::session_not_found("s-1")),
            axum::http::StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn status_mapping_permission_denied_is_403() {
        assert_eq!(
            rpc_error_to_status(&RpcError::permission_denied("x")),
            axum::http::StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn status_mapping_invalid_argument_is_400() {
        assert_eq!(
            rpc_error_to_status(&RpcError::invalid_argument("x")),
            axum::http::StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn status_mapping_invalid_params_is_400() {
        assert_eq!(
            rpc_error_to_status(&RpcError::invalid_params("x")),
            axum::http::StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn status_mapping_internal_error_is_500() {
        assert_eq!(
            rpc_error_to_status(&RpcError::internal("x")),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn status_mapping_unknown_code_falls_back_to_500() {
        assert_eq!(
            rpc_error_to_status(&RpcError::new(-1, "custom")),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
