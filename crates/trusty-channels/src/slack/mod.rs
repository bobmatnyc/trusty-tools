//! Slack channel: native Slack Web API client + MCP server (scaffold).
//!
//! Why: The Slack surface mirrors `trusty-gworkspace` — a pure Web API client
//! isolated from MCP framing so it can be reused (CLI, tests, a future REST
//! daemon), plus a stdio MCP server exposing chat-as-tools. Grouping both under
//! this one `slack` module keeps the crate ready for sibling channels (e.g.
//! `telegram`) without a per-platform crate. See ADR-0014.
//! What: [`api`] holds the endpoint constants, the typed [`api::error::SlackError`],
//! and the `BaseClient` HTTP wrapper (auth + 401/429 hardening, issue #2638);
//! [`server`] is the JSON-RPC dispatcher wired into `bin/slack-mcp.rs`;
//! [`tools`] is the authoritative `tools/list` registry; [`handlers`] holds the
//! live send/read/list `tools/call` bodies (issue #2639). The search + reaction
//! tools remain deferred stubs (they land in #2640).
//! Test: `cargo test -p trusty-channels` covers the `initialize` handshake, the
//! `tools/list` shape/count, the client's auth/HTTP behaviour
//! (`tests/client_http.rs`), and the send/read tool request paths
//! (`tests/tools_http.rs`).

pub mod api;
pub mod handlers;
pub mod server;
pub mod tools;
