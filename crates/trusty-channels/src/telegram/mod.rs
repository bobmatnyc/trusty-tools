//! Telegram channel: native Telegram Bot API client + MCP server (scaffold).
//!
//! Why: The Telegram surface mirrors the sibling `slack` module — a pure Bot
//! API client isolated from MCP framing so it can be reused (CLI, tests, a
//! future daemon), plus a stdio MCP server exposing chat-as-tools. Grouping it
//! under this one `telegram` module (rather than a standalone crate) is the
//! epic #2636 topology decision. See ADR-0014.
//! What: [`api`] holds the endpoint constants, the typed
//! [`api::error::TelegramError`], and the `BaseClient` HTTP wrapper (token in
//! the URL path + 401/429 hardening, issue #2641); [`server`] is the JSON-RPC
//! dispatcher wired into `bin/telegram-mcp.rs`; [`tools`] is the authoritative
//! `tools/list` registry. The surface is send/info-only by design — a Telegram
//! bot cannot read or search prior chat history via the Bot API. Every
//! `tools/call` handler is still a `not-yet-implemented` stub (live calls land
//! in follow-up work).
//! Test: `cargo test -p trusty-channels` covers the `initialize` handshake, the
//! `tools/list` shape/count + history-limitation note, the stub-error path, and
//! the client's auth/HTTP behaviour (`tests/telegram_client_http.rs`).

pub mod api;
pub mod server;
pub mod tools;
