//! MCP dispatch as a JSON-RPC method on the daemon socket (#6288 step 2a).
//!
//! Why: `tm serve --stdio` and `tm sessions disk` reach the daemon's MCP tool
//! surface through `POST /rpc`, and nothing else serves it. Step 2c removes the
//! TCP bind, so the socket needs this method before either client moves; the
//! 2c install risk depends on it.
//!
//! What: `mpm.mcp.dispatch` takes the whole MCP JSON-RPC envelope `/rpc` takes
//! (`{jsonrpc, id, method, params}`) as its params, and answers with the whole
//! MCP response envelope `/rpc` returns, as its result. Both transports call
//! [`dispatch_op`](crate::daemon::rpc::mcp::dispatch_op), so the two cannot drift. A notification answers the same
//! `{"jsonrpc":"2.0"}` body HTTP returns for a suppressed response.
//!
//! Auth: HTTP gates `/rpc` on a loopback peer address. The socket admits only
//! a peer whose uid is this process's, checked by `serve_until` before a byte
//! is read — a strict subset of loopback callers (see
//! `rpc::managed::control::CallerTrust`). A params object that is not an MCP
//! envelope is refused with `CODE_INVALID_PARAMS` and no tool runs, as HTTP
//! refuses an undecodable body before the handler.
//!
//! Test: `mcp_tests.rs`.

use std::sync::Arc;

use trusty_common::uds::server::{RpcError, RpcRouter};
use trusty_mcp::{Request as McpRequest, Response as McpResponse};

use crate::daemon::mcp_backend::StateBackend;
use crate::daemon::state::DaemonState;

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;

/// The one method this module registers.
pub const METHOD: &str = "mpm.mcp.dispatch";

/// Route one MCP request through the daemon's tool surface.
///
/// Why: the one body `POST /rpc` and [`METHOD`] share.
/// What: wraps `state` in a [`StateBackend`] and calls [`crate::mcp::dispatch`].
/// Test: `parity_mcp_dispatch_agrees_across_transports`.
pub async fn dispatch_op(state: &Arc<DaemonState>, req: McpRequest) -> McpResponse {
    let backend = StateBackend::new(Arc::clone(state));
    crate::mcp::dispatch(&backend, req).await
}

/// Mount [`METHOD`] on `router`.
///
/// Test: `mcp_dispatch_is_registered_on_the_daemon_socket`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let held = Arc::clone(state);
    router.typed::<McpRequest, McpResponse, _, _>(METHOD, move |req| {
        let state = Arc::clone(&held);
        async move { Ok::<_, RpcError>(dispatch_op(&state, req).await) }
    })
}
