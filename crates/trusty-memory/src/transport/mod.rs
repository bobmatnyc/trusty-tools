//! Transport-layer infrastructure for the trusty-memory daemon.
//!
//! Why: This module groups the transport-agnostic dispatch layer
//! ([`rpc`]) and the HTTP `POST /rpc` handler in [`crate::web`]).
//! Pulling these out of `lib.rs` keeps the daemon's entrypoints
//! small and lets us add future transports without touching the core.
//! The Unix-domain-socket transport and the `trusty-memory-mcp-bridge`
//! binary were removed in PR3 of the #914 stdio-cutover epic — the
//! canonical MCP integration is now `serve --stdio` (PR1 #919).
//! What: re-exports the public surface for callers (the daemon itself
//! and the integration tests).
//! Test: see the individual submodule tests; transport behaviour is
//! exercised by `tests/rpc_http.rs`.

pub mod rpc;

pub use rpc::{dispatch, JsonRpcError, JsonRpcRequest, JsonRpcResponse};
