//! Google Workspace API client layer.
//!
//! Why: Keep the pure HTTP/auth concerns isolated from MCP framing so the
//! same client can be reused outside of MCP (CLI tools, tests, future REST
//! daemon).
//! What: Re-exports auth, client, constants, and per-service modules.
//! Test: Module-level tests cover token model + storage round-trips.

pub mod auth;
pub mod bulk;
pub mod client;
pub mod constants;
pub mod services;
