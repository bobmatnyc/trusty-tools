//! The mock socket daemon every MCP bridge test drives (#9168).
//!
//! Why: the bridge reaches the daemon only over its Unix socket, so its tests
//! need a socket peer rather than an axum listener on a loopback port. The
//! library's `daemon_client_tests::mock_daemon` (twin of the binary's
//! `commands::mock_socket`) already binds one under a `TempDir`; this module
//! adds what the bridge tests share on top of it.
//! What: [`unreachable_server`] for a socket nothing serves, [`refusal`] to
//! render an HTTP-shaped `(status, body)` the way the daemon itself does, and
//! [`recording_daemon`] to capture every `(method, params)` a tool sends.
//! Nothing here resolves or dials the live daemon's socket.
//! Test: every `tests_*.rs` dispatch test in this directory.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use trusty_common::uds::server::RpcError;

use super::McpServer;
pub(crate) use crate::service::daemon_client::tests::{mock_daemon, MockDaemon};
use crate::service::daemon_client::DaemonClient;

/// Every `(method, params)` pair a mock daemon received, in order.
pub(crate) type Calls = Arc<Mutex<Vec<(String, Value)>>>;

impl MockDaemon {
    /// A dispatcher on this mock's socket. Keep the mock alive while it runs.
    pub(crate) fn server(&self) -> McpServer {
        McpServer::new(self.client.clone())
    }
}

/// A dispatcher whose socket nothing serves, so any daemon call fails as
/// unreachable.
pub(crate) fn unreachable_server() -> McpServer {
    McpServer::new(DaemonClient::at(
        "/nonexistent/trusty-search-mcp-test/ts.sock",
    ))
}

/// The refusal the daemon sends for an HTTP-shaped `(status, body)`.
///
/// Why: the daemon builds every socket refusal with `rpc_error_from_http`;
/// a mock that used the same function cannot drift from the real code or the
/// real `data` member.
pub(crate) fn refusal(status: u16, body: &Value) -> RpcError {
    let status = axum::http::StatusCode::from_u16(status).expect("a valid status");
    crate::service::rpc::error::rpc_error_from_http(status, body)
}

/// A mock daemon that records every call and answers through `answer`.
pub(crate) async fn recording_daemon(
    answer: impl Fn(&str, &Value) -> Result<Value, RpcError> + Send + Sync + 'static,
) -> (MockDaemon, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let daemon = mock_daemon(move |method, params| {
        let reply = answer(method, &params);
        seen.lock()
            .expect("the call log")
            .push((method.to_string(), params));
        reply
    })
    .await;
    (daemon, calls)
}

/// The params of the last call to `method`, if any.
pub(crate) fn last_params(calls: &Calls, method: &str) -> Option<Value> {
    calls
        .lock()
        .expect("the call log")
        .iter()
        .rev()
        .find(|(m, _)| m == method)
        .map(|(_, p)| p.clone())
}

/// Every method called, in order.
pub(crate) fn methods(calls: &Calls) -> Vec<String> {
    calls
        .lock()
        .expect("the call log")
        .iter()
        .map(|(m, _)| m.clone())
        .collect()
}
