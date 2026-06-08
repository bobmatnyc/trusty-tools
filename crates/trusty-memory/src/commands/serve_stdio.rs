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

/// Returns true if the request is a JSON-RPC notification.
///
/// Why: the MCP spec (§4.1) forbids sending any response for a notification.
/// Suppression must be decided from the REQUEST, not the response value — if
/// dispatch's sentinel ever drifts the channel would be corrupted.  This
/// predicate is the single canonical check, driven by the spec rule: a
/// notification has no `id` field, and/or its method begins with
/// `notifications/`.
/// What: returns true when `req.id` is `None` (absent in the wire JSON) or
/// the method starts with `"notifications/"`.
/// Test: `notification_requests_are_detected` unit test; transitively by
/// `serve_stdio_e2e`.
fn is_notification(req: &trusty_common::mcp::Request) -> bool {
    req.id.is_none() || req.method.starts_with("notifications/")
}

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
    // The JoinHandle is stored and aborted when the stdio loop exits so the
    // warm-up task is cancelled on shutdown rather than outliving the process.
    let warmup_state = state.clone();
    let warmup_handle = tokio::spawn(async move {
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

    let result = trusty_common::mcp::run_stdio_loop(move |req| {
        let state = state.clone();
        async move {
            // Decide suppression from the REQUEST before touching dispatch.
            // JSON-RPC spec §4.1: a notification has no id — the server MUST
            // NOT reply.  Checking here (not from the response value) means
            // the guarantee holds even if dispatch's sentinel drifts.
            if is_notification(&req) {
                return trusty_common::mcp::Response::suppressed();
            }
            let rpc_req = rpc_request_from_mcp(req);
            let rpc_resp = dispatch(&state, rpc_req).await;
            mcp_response_from_rpc(rpc_resp)
        }
    })
    .await;

    // Cancel the warm-up task so it does not outlive the stdio loop.
    warmup_handle.abort();

    result
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
/// coupling either type to the other.  Note: notification suppression is now
/// decided at the REQUEST layer (`is_notification`), not from the response
/// value, so this function never needs to inspect `id == Null` as a sentinel.
/// What: maps `id`, `result`, `error` directly from the `JsonRpcResponse` into
/// the `mcp::Response` envelope.
/// Test: covered transitively by `serve_stdio_e2e`.
fn mcp_response_from_rpc(
    resp: crate::transport::rpc::JsonRpcResponse,
) -> trusty_common::mcp::Response {
    use serde_json::Value;

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

    /// Why: `is_notification` must accurately classify requests so the
    /// stdio loop never emits a reply for a notification.  This is the
    /// spec-compliance guard that prevents stdout corruption if dispatch's
    /// sentinel ever drifts.
    /// What: exercises all three cases:
    ///   1. id present → not a notification (must return false)
    ///   2. id absent (None) → notification (must return true)
    ///   3. method starts with "notifications/" even if id is present
    ///      (belt-and-suspenders; MCP clients may include an id on
    ///      notifications/initialized even though spec says not to)
    ///
    /// Test: this test.
    #[test]
    fn notification_requests_are_detected() {
        // Normal request with id — not a notification.
        let normal = trusty_common::mcp::Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };
        assert!(
            !is_notification(&normal),
            "request with id must not be classified as notification"
        );

        // No id field → notification.
        let notif_no_id = trusty_common::mcp::Request {
            jsonrpc: Some("2.0".to_string()),
            id: None,
            method: "notifications/initialized".to_string(),
            params: None,
        };
        assert!(
            is_notification(&notif_no_id),
            "request with no id must be classified as notification"
        );

        // notifications/ prefix with no id — doubly a notification.
        let notif_prefix = trusty_common::mcp::Request {
            jsonrpc: Some("2.0".to_string()),
            id: None,
            method: "notifications/cancelled".to_string(),
            params: None,
        };
        assert!(
            is_notification(&notif_prefix),
            "notifications/* method with no id must be classified as notification"
        );

        // Edge: notifications/ prefix but id present (non-spec client).
        let notif_prefix_with_id = trusty_common::mcp::Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(99)),
            method: "notifications/initialized".to_string(),
            params: None,
        };
        assert!(
            is_notification(&notif_prefix_with_id),
            "notifications/* method must be classified as notification even with id"
        );
    }

    /// Why: a real tool response (id present, non-notification method) must NOT
    /// be suppressed — it must reach the client.
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
