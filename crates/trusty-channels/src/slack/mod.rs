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
//! live `tools/call` bodies for all 19 tools — send/read/list (issue #2639),
//! search + reactions (issue #2640), and the claude.ai-connector-parity batch
//! (canvases, conversation create/members, reaction reads, file content,
//! scheduled messages, search extras — epic #3611, issues #3612-#3618). The
//! client holds two tokens: a required bot token and an optional user token
//! that only `search.messages` consults (issue #2640).
//! Test: `cargo test -p trusty-channels` covers the `initialize` handshake, the
//! `tools/list` shape/count, the client's auth/HTTP behaviour
//! (`tests/client_http.rs`), and the tool request paths (`tests/tools_http.rs`).

pub mod api;
pub mod handlers;
pub mod server;
pub mod tools;
