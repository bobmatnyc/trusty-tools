//! Google Workspace MCP server for the Trusty suite.
//!
//! Why: Provides a Rust port of the Python gworkspace-mcp project so the
//! trusty-* ecosystem has a single shared toolchain for Gmail/Drive/Calendar/
//! Docs/Sheets/Slides/Tasks access through Model Context Protocol.
//! What: Two logical layers — a pure Google Workspace API client under
//! `api::` (auth, token storage, service modules) and an MCP server in
//! `server` + `bin/trusty-gworkspace-mcp.rs` that dispatches JSON-RPC tool calls.
//! Test: `cargo test -p trusty-gworkspace` runs the auth-model deserialise
//! tests and the tools-list shape test.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

pub mod api;
pub mod cli;
pub mod openrpc;
pub mod server;
pub mod tools;
