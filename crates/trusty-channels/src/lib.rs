//! trusty-channels — native MCP servers for chat channels (Slack, …).
//!
//! Why: The trusty-* ecosystem wants native-Rust MCP surfaces over chat
//! platforms (chat-as-tools) rather than depending on hosted connectors, so
//! agents can send/read messages, list channels/users, search, and react
//! through the same stdio JSON-RPC transport every other trusty MCP server
//! uses. Consolidating every channel behind one crate (per epic #2636's
//! topology decision) keeps shared MCP framing, HTTP-client hardening, and
//! credential wiring in one place instead of duplicating a crate per platform.
//! See ADR-0014.
//! What: One module per channel. Today only [`slack`] exists (its `slack-mcp`
//! binary lives in `src/bin/slack-mcp.rs`); a future `telegram` module and
//! `telegram-mcp` binary slot in alongside it without a new crate.
//! Test: `cargo test -p trusty-channels` covers each channel's client, MCP
//! handshake, and tool registry.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

pub mod slack;
pub mod telegram;
