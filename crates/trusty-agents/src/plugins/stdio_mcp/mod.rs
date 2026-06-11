//! Re-export shim: `StdioMcpClient` and related types are now defined in
//! `trusty_common::stdio_mcp_client` (epic #1104 Phase 0a, feature
//! `stdio-mcp-client`). All call sites that imported from this module
//! (`crate::plugins::stdio_mcp::StdioMcpClient`, etc.) continue to work
//! unchanged through these re-exports.
//!
//! Why: The client was promoted to `trusty-common` so `trusty-console` and
//! future local services can reuse it without a dependency on `trusty-agents`.
//! Keeping the shim here preserves backward compatibility for trusty-agents'
//! internal call sites without requiring a mechanical rename across the crate.
//! What: Publicly re-exports `StdioMcpClient`, `ServerInfo`, `McpTool`,
//! `MCP_PROTOCOL_VERSION`, and `plugin_stderr_stdio` from
//! `trusty_common::stdio_mcp_client`.
//! Test: All existing tests that exercised the original module now exercise
//! the shared implementation via these re-exports. Run
//! `cargo test -p trusty-agents` to verify.

pub use trusty_common::stdio_mcp_client::{
    MCP_PROTOCOL_VERSION, McpTool, ServerInfo, StdioMcpClient, plugin_stderr_stdio,
};
