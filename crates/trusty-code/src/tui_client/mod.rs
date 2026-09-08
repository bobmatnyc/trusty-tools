//! `CodeEngine` — the `trusty-code-tui` engine adapter for `tcode tui` (issue
//! #3415, DOC-50 §3.3/§3.4; retransported onto the daemon's Unix socket in
//! #6637).
//!
//! Why: see `crate::tui_client`'s doc comment on the `pub mod tui_client;`
//! declaration in `lib.rs` for the full rationale (ephemeral `--stdio` CLI
//! client vs. long-lived daemon client).
//! What: [`uds_rpc`] (the framed JSON-RPC client over the daemon socket),
//! [`error::EngineError`] (this module's unified error type), and
//! [`engine::CodeEngine`] (the `trusty_code_tui::TuiEngine` impl itself — the
//! module most callers want).
//!
//! `discovery`, `rpc` and `sse` are gone (#6637). Discovery answered "which
//! `TCODE_DAEMON_URL` or `http_addr` file names a live daemon"; the socket path
//! is derived from the data directory, so there is nothing to discover. `rpc`
//! and `sse` were the HTTP and Server-Sent-Events halves, replaced by
//! [`uds_rpc::UdsRpcClient::call`] and [`uds_rpc::UdsRpcClient::open_stream`].
//! Test: see each submodule's own docs; the full engine flow against a real
//! daemon socket lives in `tests/tui_client_engine.rs`.
//!
//! [`uds_rpc`]: crate::tui_client::uds_rpc
//! [`error::EngineError`]: crate::tui_client::error::EngineError
//! [`engine::CodeEngine`]: crate::tui_client::engine::CodeEngine
//! [`uds_rpc::UdsRpcClient::call`]: crate::tui_client::uds_rpc::UdsRpcClient::call
//! [`uds_rpc::UdsRpcClient::open_stream`]: crate::tui_client::uds_rpc::UdsRpcClient::open_stream

pub mod engine;
mod engine_state;
pub mod error;
mod session_events;
pub mod uds_rpc;
mod workstream_subscription;

pub use engine::CodeEngine;
pub use error::EngineError;
