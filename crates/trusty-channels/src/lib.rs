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
//! What: One module per channel: [`slack`] and [`telegram`] (each with an MCP
//! binary under `src/bin/`), and [`gchat`] — the Google Chat API layer, its
//! routes, egress gate and question ledger, with no binary yet (#9448).
//! Test: `cargo test -p trusty-channels` covers each channel's client, MCP
//! handshake, and tool registry.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

pub mod gchat;
pub mod slack;
pub mod telegram;
