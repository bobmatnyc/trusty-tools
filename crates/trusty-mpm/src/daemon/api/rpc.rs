//! Loopback-only JSON-RPC dispatch endpoint (`POST /rpc`, #1221).
//!
//! Why: the `trusty-mpm serve --stdio` bridge (mirroring `trusty-memory`) needs
//! an internal channel to forward MCP JSON-RPC envelopes to the durable daemon.
//! HTTP `POST /rpc` is the simplest, most consistent option (resolved owner
//! decision, RFC §6 Q1). But this endpoint executes arbitrary tool calls
//! (including session spawn/decommission), so it MUST never be reachable off
//! loopback — even if the daemon is later rebound to `0.0.0.0` (e.g. inside a
//! container). This is the hard acceptance criterion from the #1221 RFC review
//! (conf 0.85): the route returns 403 for any non-loopback source IP.
//! What: [`is_loopback`] is the pure peer-address predicate; [`rpc_handler`] is
//! the axum handler — it rejects non-loopback peers with 403, otherwise builds a
//! [`StateBackend`] and routes the request through [`crate::mcp::dispatch`],
//! returning the JSON-RPC response. Defense-in-depth: the daemon already binds
//! loopback, AND this handler independently enforces it, so a future bind change
//! cannot silently expose the tool surface.
//! What (hygiene): all diagnostics go to `tracing` (stderr); the handler never
//! writes to stdout (stdout is the MCP channel for the bridge, not the daemon).
//! Test: `is_loopback_accepts_v4_v6_loopback`, `is_loopback_rejects_public`, and
//! the daemon integration test `rpc_rejects_non_loopback_peer` /
//! `rpc_dispatches_tools_list_for_loopback`.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::StatusCode,
    response::IntoResponse,
};
use trusty_common::mcp::Request as McpRequest;

use crate::daemon::mcp_backend::StateBackend;
use crate::daemon::state::DaemonState;

/// True when `addr`'s IP is a loopback address (IPv4 `127.0.0.0/8` or IPv6 `::1`).
///
/// Why: the `/rpc` route must be reachable only from the local host. Keying the
/// decision on the peer IP (rather than only on the daemon's bind address) makes
/// the guarantee hold even if the daemon is later bound to `0.0.0.0` — a
/// non-loopback client is rejected regardless of bind config.
/// What: returns `true` iff `addr.ip()` is an IPv4 or IPv6 loopback address.
/// Test: `is_loopback_accepts_v4_v6_loopback`, `is_loopback_rejects_public`.
pub fn is_loopback(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// `POST /rpc` — loopback-only JSON-RPC dispatch into the MCP tool surface.
///
/// Why: the stdio bridge forwards every MCP request here; the daemon holds the
/// durable state, so dispatch must run in-process against it. The loopback gate
/// is the #1221 hard requirement.
/// What: (1) rejects any non-loopback peer with `403 Forbidden` and a short
/// message — no body is parsed and no tool runs; (2) for a loopback peer, wraps
/// the daemon state in a [`StateBackend`] and calls [`crate::mcp::dispatch`],
/// returning the JSON-RPC [`trusty_common::mcp::Response`] as JSON (HTTP 200;
/// JSON-RPC errors are carried in the envelope, matching `trusty-memory`).
/// Test: `rpc_rejects_non_loopback_peer`, `rpc_dispatches_tools_list_for_loopback`.
pub async fn rpc_handler(
    State(state): State<Arc<DaemonState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(req): Json<McpRequest>,
) -> axum::response::Response {
    if !is_loopback(&peer) {
        tracing::warn!(
            %peer,
            "rejected non-loopback POST /rpc — the RPC endpoint is loopback-only"
        );
        return (
            StatusCode::FORBIDDEN,
            "POST /rpc is loopback-only; remote access is forbidden",
        )
            .into_response();
    }

    let backend = StateBackend::new(Arc::clone(&state));
    let resp = crate::mcp::dispatch(&backend, req).await;
    Json(resp).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn is_loopback_accepts_v4_v6_loopback() {
        let v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7880);
        let v4_8 = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 4, 5, 6)), 7880);
        let v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 7880);
        assert!(is_loopback(&v4));
        assert!(is_loopback(&v4_8), "all of 127.0.0.0/8 is loopback");
        assert!(is_loopback(&v6));
    }

    #[test]
    fn is_loopback_rejects_public() {
        let lan = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)), 5000);
        let public = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443);
        let v6_pub = SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            443,
        );
        assert!(!is_loopback(&lan));
        assert!(!is_loopback(&public));
        assert!(!is_loopback(&v6_pub));
    }
}
