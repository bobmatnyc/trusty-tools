//! Plugin transports for external tool integrations.
//!
//! Why: trusty-agents wants to call external tools (MCP servers, future plugin
//! hosts) without baking transport details into agent code. Centralising
//! these clients here keeps the agent layer focused on tool dispatch and
//! lets transport implementations evolve independently.
//! What: Currently exposes `StdioMcpClient` — a Model Context Protocol
//! client that speaks JSON-RPC 2.0 over a child process's stdin/stdout.
//! Test: See `stdio_mcp::tests` for JSON-RPC framing and message construction
//! coverage.

#![allow(dead_code)]

pub mod manager;
pub mod python_tool;
pub mod stdio_mcp;
pub mod trusty_memory;
pub mod trusty_search;

pub use manager::{PluginManager, PluginState, PluginStatus, init_global, plugin_manager};
pub use python_tool::{PythonPluginConfig, PythonToolPlugin};
pub use stdio_mcp::{McpTool, ServerInfo, StdioMcpClient};
pub use trusty_memory::TrustyMemoryPlugin;
pub use trusty_search::TrustySearchPlugin;
