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

pub mod api;
pub mod cli;
pub mod openrpc;
pub mod server;
pub mod tools;
