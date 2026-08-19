//! Line-delimited JSON-RPC loop on stdin/stdout.
//!
//! Why: Claude Code launches MCP servers as subprocesses and communicates
//! over stdio with one JSON-RPC message per line. The shared
//! `trusty_mcp_core::run_stdio_loop` owns the parse/dispatch/write/flush
//! cycle so every trusty-* MCP server shares the same behaviour.
//!
//! What: [`run`] adapts an [`McpServer`] into the closure shape the shared
//! loop expects and delegates the loop body.
//!
//! Test: covered indirectly by `tools::tests` plus a smoke test in
//! `tests/stdio.rs` that pipes a `tools/list` request through the loop.

use crate::mcp::tools::McpServer;
use anyhow::Result;
use std::sync::Arc;

/// Read JSON-RPC requests line-by-line from stdin, dispatch via `server`,
/// and write responses to stdout. Returns when stdin reaches EOF.
pub async fn run(server: McpServer) -> Result<()> {
    let server = Arc::new(server);
    trusty_mcp::run_stdio_loop(move |req| {
        let server = server.clone();
        async move { server.dispatch(req).await }
    })
    .await
}
