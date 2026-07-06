//! JSON-RPC 2.0 core: wire types, error codes, and the method router (#2053).
//!
//! Why: `tcode serve` must speak the exact same JSON-RPC-over-STDIO contract
//! as the rest of the trusty-* family so tooling (CLI clients, MCP bridges,
//! future TUI/GUI frontends) has one dispatch convention to learn. Re-using
//! `trusty_common::mcp`'s wire types — rather than hand-rolling a second,
//! structurally-identical `Request`/`Response`/`JsonRpcError` — is the
//! ecosystem-consistent choice: `trusty-search` (`mcp::stdio::run`) and
//! `trusty-memory` (`commands::serve_stdio_bridge::run_stdio_bridge`) both
//! already build on it, and its own module doc explicitly exists to prevent
//! "bug-fixed-in-one-not-the-other" drift between servers.
//!
//! What tcode adds on top is new: a [`Router`] that maps method names to
//! async handlers via a registry rather than a single hand-written `match`.
//! trusty-memory/trusty-search dispatch through one large `match` because
//! their method surface is fixed; tcode's is not — later tickets register
//! `session.*` (#2054), `task.*` (#2056), and `harness.describe` (#2066)
//! methods, and a registry lets each land as an isolated `router.register(...)`
//! call instead of growing one match arm-by-arm. [`RpcError`] is the typed
//! error a handler returns; the router converts it into the JSON-RPC error
//! envelope.
//!
//! Test: see `error::tests` and `router::tests`.

pub mod error;
pub mod router;

pub use error::RpcError;
pub use router::{MethodHandler, Router};
pub use trusty_common::mcp::{Request, Response, error_codes};
