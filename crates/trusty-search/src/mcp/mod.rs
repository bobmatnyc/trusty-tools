//! MCP (Model Context Protocol) server for `trusty-search`.
//!
//! Why: Claude Code (and other MCP clients) speak JSON-RPC 2.0 over either
//! stdio or HTTP/SSE. The daemon already exposes a REST API; this crate
//! adapts that API into MCP tool calls so an LLM can drive code search.
//!
//! What:
//! - [`McpServer`]   — pure dispatcher; takes a JSON-RPC `Request` and returns
//!   a `Response` by proxying tool calls to the daemon over HTTP.
//! - [`stdio`]       — line-delimited JSON-RPC loop on stdin/stdout.
//! - [`sse`]         — axum router exposing `POST /mcp` and `GET /mcp/sse`.
//!
//! Test: `cargo test -p trusty-search-mcp` covers JSON-RPC parsing, error
//! shapes, and tool-name dispatch with a mocked daemon URL.

pub mod cwd_scope;
pub mod openrpc;
pub mod sse;
pub mod stdio;
pub mod tools;

pub use tools::{error_codes, tool_descriptors, JsonRpcError, McpServer, Request, Response};
