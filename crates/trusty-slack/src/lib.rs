//! Native Slack MCP server for the Trusty suite (scaffold).
//!
//! Why: The trusty-* ecosystem wants a single, native-Rust MCP surface over
//! Slack (chat-as-tools) rather than depending on the hosted connector, so
//! agents can send/read messages, list channels/users, search, and react
//! through the same stdio JSON-RPC transport every other trusty MCP server
//! uses. See ADR-0014.
//! What: Two logical layers mirroring `trusty-gworkspace` — a pure Slack Web
//! API client under `api::` (`BaseClient` + endpoint constants) and an MCP
//! server in `server` + `bin/slack-mcp.rs` that dispatches `initialize`,
//! `tools/list`, and `tools/call`. This is a *scaffold*: the tool schemas are
//! authoritative, but every `tools/call` handler is a stub that returns a
//! clear `not-yet-implemented` MCP error. Live Slack HTTP calls and auth are
//! deferred pending a token decision.
//! Test: `cargo test -p trusty-slack` covers the `initialize` handshake, the
//! `tools/list` shape/count, and that a `tools/call` returns the stub error.

pub mod api;
pub mod server;
pub mod tools;
