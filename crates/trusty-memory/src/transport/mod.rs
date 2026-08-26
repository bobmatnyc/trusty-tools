//! How the daemon is reachable, and what it answers.
//!
//! Why: the daemon's entry points used to be spread between `lib.rs`, an axum
//! router, and this module's dispatcher. #6286 collapsed that: ADR-0032 leaves
//! `trusty-console` as the workspace's only HTTP surface, so trusty-memory
//! serves one hardened Unix socket and nothing else.
//!
//! What, in the order a request meets them:
//!   - [`uds`] binds the socket and owns the method table. It mounts [`rpc`]'s
//!     dispatcher whole through `RpcRouter::fallback` rather than restating its
//!     ~75 names, and registers on top of it the twenty-odd methods that used
//!     to exist only as REST routes.
//!   - [`rpc`] is the transport-agnostic dispatcher: a parsed method name and
//!     params onto the handlers in [`crate::tools`]. It predates the socket —
//!     it was written for `POST /rpc` and the MCP stdio bridge — and is
//!     unchanged by the migration, which is why the fallback seam exists.
//!   - [`methods`] holds the folded former-REST handlers, each a plain async
//!     function over `(&AppState, Params)`.
//!   - [`api_error`] is the one place a handler failure becomes a JSON-RPC
//!     code.
//!
//! **The earlier UDS transport this is not.** `transport/mod.rs` used to record
//! that a Unix socket existed and was removed in PR3 of the #914 stdio-cutover
//! epic. That socket was a different thing — a byte-pipe MCP bridge with its
//! own binary — and its removal is why only the dispatcher survived. This one
//! is the daemon's whole surface and is built on the shared
//! `trusty_common::uds::server` primitives rather than hand-rolled.
//!
//! Test: `uds::tests` for the wire, `rpc::tests` for the dispatcher.

pub mod api_error;
pub mod methods;
pub mod rpc;
pub mod uds;

pub use api_error::{ApiError, ErrorKind, CODE_NOT_FOUND, CODE_REFUSED};
pub use rpc::{dispatch, JsonRpcError, JsonRpcRequest, JsonRpcResponse};
pub use uds::{build_router, serve, socket_path, FOLDED_METHODS, METHOD_HEALTH, STREAM_METHODS};
