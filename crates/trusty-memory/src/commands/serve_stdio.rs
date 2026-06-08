//! Direct stdio JSON-RPC MCP server (issue #914, Part B).
//!
//! Why: Claude Code launches MCP servers as subprocesses and communicates over
//! stdio using line-delimited JSON-RPC.  The historic `serve --stdio` mode was
//! removed in issue #150 because it deadlocked on the redb exclusive write lock
//! when a long-lived HTTP daemon was already running.  This module reinstates
//! it as a first-class code path that:
//!
//!   1. Builds its own `AppState` (no shared redb lock with a running HTTP
//!      daemon — palaces opened read-only via the snapshot fallback when the
//!      write lock is held elsewhere).
//!   2. Dispatches every request through the EXISTING
//!      `transport::rpc::dispatch` so tool parity with the HTTP/UDS path is
//!      automatic.
//!   3. NEVER binds axum, a TCP socket, or a UDS listener — stdout is the
//!      JSON-RPC channel and must remain pure protocol bytes.
//!   4. Enforces the never-hang invariant: every request resolves within a
//!      wall-clock deadline (success or explicit JSON-RPC error).  The
//!      `readiness_check()` preflights on every embedder-touching tool handler
//!      are the primary guard; the embedder `OnceCell` timeout (180 s) is the
//!      backstop.
//!
//! What: `run_stdio` builds `AppState`, optionally kicks a background
//! embedder warm-up, applies the `--palace` default, and then delegates to
//! `trusty_common::mcp::run_stdio_loop` with a closure that adapts the
//! `trusty_common::mcp::{Request,Response}` envelope to the
//! `transport::rpc::{JsonRpcRequest,JsonRpcResponse}` types used by
//! `dispatch`.
//!
//! Test: `tests/serve_stdio_e2e.rs` spawns a real child process, sends
//! `initialize`, `tools/list`, `memory_remember`, `memory_recall`, and
//! `memory_recall_all`, and asserts each response arrives within a
//! wall-clock deadline.

use anyhow::Result;
use std::path::PathBuf;

use crate::transport::rpc::{dispatch, JsonRpcRequest};
use crate::AppState;

/// Run a direct stdio JSON-RPC MCP server.
///
/// Why: reinstates `serve --stdio` as a safe, deadlock-free code path (issue
/// #914 Part B).  The `run_serve` path in `main.rs` binds axum and registers
/// startup tasks that emit banners and SSE messages to stdout/stderr; this
/// path deliberately omits all of that so stdout remains a clean JSON-RPC
/// channel.
/// What: resolves the palace data root (same logic as `run_serve`), applies
/// optional `--palace` default, optionally kicks a background embedder
/// warm-up, then enters `run_stdio_loop` dispatching every request through
/// the shared `transport::rpc::dispatch`.
/// Test: `tests/serve_stdio_e2e.rs` exercises the full round-trip from a
/// spawned child process; `tools::tests::recall_all_returns_warming_error_*`
/// covers the readiness preflight guard.
pub async fn run_stdio(data_root: PathBuf, palace: Option<String>) -> Result<()> {
    // Build state rooted at the resolved palace registry dir.
    // We do NOT call spawn_startup_tasks (no HTTP addr file, no pin-scan
    // banner, no update-check eprintln — stdout must stay clean).
    let state = AppState::new(data_root).with_default_palace(palace);

    // Optionally warm up the embedder in the background so the first recall is
    // fast.  Failures stay at WARN — the readiness preflight in each tool
    // handler catches the Warming state and returns a bounded error.
    let warmup_state = state.clone();
    tokio::spawn(async move {
        match trusty_common::memory_core::retrieval::shared_embedder().await {
            Ok(_) => warmup_state.set_ready(),
            Err(e) => tracing::warn!(
                "stdio serve: background embedder warm-up failed \
                 (memory ops will return a bounded error on first request): {e:#}"
            ),
        }
    });

    // Wrap `state` in an Arc so the closure can clone it cheaply for each
    // dispatched request.
    let state = std::sync::Arc::new(state);

    trusty_common::mcp::run_stdio_loop(move |req| {
        let state = state.clone();
        async move {
            let rpc_req = rpc_request_from_mcp(req);
            let rpc_resp = dispatch(&state, rpc_req).await;
            mcp_response_from_rpc(rpc_resp)
        }
    })
    .await
}

/// Convert a `trusty_common::mcp::Request` into a `transport::rpc::JsonRpcRequest`.
///
/// Why: the shared `run_stdio_loop` works with the common `mcp::Request`
/// envelope; the existing dispatcher speaks `rpc::JsonRpcRequest`.  A thin
/// adapter here avoids duplicating either the loop or the dispatcher.
/// What: maps fields 1-to-1; the types are structurally identical so the
/// conversion is infallible.
/// Test: covered transitively by `serve_stdio_e2e` which drives the full
/// request pipeline.
fn rpc_request_from_mcp(req: trusty_common::mcp::Request) -> JsonRpcRequest {
    JsonRpcRequest {
        jsonrpc: req.jsonrpc,
        id: req.id,
        method: req.method,
        params: req.params,
    }
}

/// Convert a `transport::rpc::JsonRpcResponse` into a `trusty_common::mcp::Response`.
///
/// Why: `run_stdio_loop` expects the common `mcp::Response` on the write path;
/// `dispatch` returns `rpc::JsonRpcResponse`.  A thin adapter here avoids
/// coupling either type to the other.
/// What: maps `id`, `result`, `error` directly.  Notification responses
/// (`id == null && result == null && error == null`) are marked `suppress =
/// true` so `run_stdio_loop` drops them without writing to stdout — matches
/// the MCP spec requirement that notifications never receive a reply.
/// Test: covered transitively by `serve_stdio_e2e`.
fn mcp_response_from_rpc(
    resp: crate::transport::rpc::JsonRpcResponse,
) -> trusty_common::mcp::Response {
    use serde_json::Value;

    // Suppress notifications: a response with id=Null, result=Null, and
    // error=None was generated by the `notifications/*` arm in dispatch;
    // per MCP spec §4.1 we MUST NOT send anything back.
    let is_notification_ack = resp.id == Value::Null
        && resp.result.as_ref() == Some(&Value::Null)
        && resp.error.is_none();
    if is_notification_ack {
        return trusty_common::mcp::Response::suppressed();
    }

    let id = if resp.id == Value::Null {
        None
    } else {
        Some(resp.id)
    };

    match (resp.result, resp.error) {
        (Some(result), _) => trusty_common::mcp::Response::ok(id, result),
        (None, Some(err)) => trusty_common::mcp::Response::err(id, err.code, err.message),
        (None, None) => {
            // Should not happen in practice; return an internal error rather
            // than silently dropping the response.
            trusty_common::mcp::Response::err(
                id,
                trusty_common::mcp::error_codes::INTERNAL_ERROR,
                "dispatch returned a response with no result and no error",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Why: `rpc_request_from_mcp` must preserve all fields so the dispatcher
    /// sees the same method, id, and params that the stdio loop parsed.
    /// What: round-trip a request through the adapter and assert field equality.
    /// Test: this test.
    #[test]
    fn rpc_request_adapter_preserves_fields() {
        let mcp_req = trusty_common::mcp::Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(42)),
            method: "palace_list".to_string(),
            params: Some(json!({"palace": "test"})),
        };
        let rpc_req = rpc_request_from_mcp(mcp_req);
        assert_eq!(rpc_req.id, Some(json!(42)));
        assert_eq!(rpc_req.method, "palace_list");
        assert_eq!(rpc_req.params, Some(json!({"palace": "test"})));
    }

    /// Why: notification responses (notifications/initialized) must be
    /// suppressed so `run_stdio_loop` never writes a reply to stdout — MCP
    /// spec §4.1.  This test drives `mcp_response_from_rpc` with the sentinel
    /// envelope that `dispatch` emits for notifications and asserts the
    /// `suppress` flag is set.
    /// Test: this test (spec compliance guard).
    #[test]
    fn notification_ack_is_suppressed() {
        let rpc_resp = crate::transport::rpc::JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: serde_json::Value::Null,
            result: Some(serde_json::Value::Null),
            error: None,
        };
        let mcp_resp = mcp_response_from_rpc(rpc_resp);
        assert!(
            mcp_resp.suppress,
            "notification ack must be suppressed so no reply is written to stdout"
        );
    }

    /// Why: a real tool response (id present, result present) must NOT be
    /// suppressed — it must reach the client.
    /// Test: this test.
    #[test]
    fn normal_response_is_not_suppressed() {
        let rpc_resp = crate::transport::rpc::JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: json!(7),
            result: Some(json!({"tools": []})),
            error: None,
        };
        let mcp_resp = mcp_response_from_rpc(rpc_resp);
        assert!(
            !mcp_resp.suppress,
            "non-notification response must not be suppressed"
        );
        assert_eq!(mcp_resp.id, Some(json!(7)));
    }
}
