//! Transport-layer infrastructure for the trusty-memory daemon.
//!
//! Why: This module groups the transport-agnostic dispatch layer
//! ([`rpc`]) and the per-transport listeners ([`uds`], plus the HTTP
//! `POST /rpc` handler in [`crate::web`]). Pulling these out of
//! `lib.rs` keeps the daemon's entrypoints small and lets us add
//! future transports (gRPC, named pipes) without touching the core.
//! What: re-exports the public surface for callers (the daemon itself
//! and the integration tests).
//! Test: see the individual submodule tests; cross-transport behaviour
//! is exercised by `tests/uds_roundtrip.rs` and `tests/rpc_http.rs`.

pub mod rpc;
pub mod uds;

pub use rpc::{dispatch, JsonRpcError, JsonRpcRequest, JsonRpcResponse};
pub use uds::{
    bind_uds, run_uds, socket_path, socket_path_for, write_uds_addr_file, UDS_ADDR_FILE,
};
