//! Service layer for trusty-review — shared state and the UDS transport.
//!
//! Why: wraps the existing review pipeline in a long-lived daemon so callers
//! can request a review without spawning a CLI process.
//!
//! #6277 (ADR-0032) moved that daemon off TCP loopback HTTP. It no longer binds
//! a port, no longer writes an `http_addr` discovery file, and no longer serves
//! an axum router; it binds one hardened Unix socket and answers three
//! JSON-RPC methods on it. `trusty-console` remains the workspace's only HTTP
//! surface.
//!
//! What: exports `AppState` and re-exports the wire layer from [`rpc`] —
//! [`rpc::serve`], [`rpc::socket_path`], and the three method names. Every
//! operation's logic lives in `handlers.rs`, over plain types.
//!
//! GitHub webhooks do NOT arrive here. #5181 retired `POST /pr/github/webhook`;
//! `trusty-console` terminates the GitHub request and relays it over a separate
//! UDS socket to `webhook_listener` (ADR-0034).
//!
//! Test: `cargo test -p trusty-review --features http-server` exercises the
//! router over a real socket in `rpc_tests.rs`, and each operation directly in
//! `handlers_tests.rs`.
//!
//! Feature gate: the entire module is compiled only under `http-server`.

pub mod handlers;
pub mod inference_probe;
pub mod rpc;

pub use handlers::AppState;
pub use rpc::{METHOD_HEALTH, METHOD_RUN, METHOD_STATUS, serve, socket_path};
