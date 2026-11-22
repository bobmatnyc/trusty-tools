//! MCP (Model Context Protocol) connections: two tiers, one authority.
//!
//! Why: trusty-agents itself runs as an LLM-driven agent harness, and its
//! assistants reach external capability through MCP servers. Until #7454 this
//! crate described those servers with THREE independent schemas of its own
//! (`mcp::config::McpService`, `tools::registry::config::EndpointConfig`,
//! `mcp::mcp_json::McpJsonServer`), all global-only, and ran a second parser
//! over the identical `.mcp.json` format `trusty-mpm` already read. ADR-0060
//! collapses all of that onto `trusty_mcp::config::McpServerConfig` and adds
//! the tier the owner asked for on 2026-09-11: an MCP connection is set
//! globally OR per assistant.
//!
//! What:
//! - [`extensions`] — the consumer-specific keys this crate keeps inside
//!   `McpServerConfig::extensions`, and the only place they are decoded.
//! - [`shared`] — the global tier: the file shared with `trusty-code`
//!   (`~/.trusty-tools/mcp/servers.toml`), the one-time migration off the
//!   retired tables, the `.mcp.json` import, and
//!   [`shared::resolve_for_assistant`], which layers an assistant's own
//!   `[mcp]` overrides on top through `trusty_mcp::config::resolve`.
//! - [`config`] — `GlobalConfig`, the rest of `~/.trusty-agents/config.toml`.
//!   It no longer describes MCP SERVERS; what remains under `[mcp]` is this
//!   crate's own policy (which agent roles get the prompt layer, whether a
//!   project's `.mcp.json` is trusted).
//!
//! The assistant tier itself lives with the assistant:
//! [`crate::assistants::mcp`].
//!
//! Test: `tests` — the submodules under `mcp/tests/`.

pub mod config;
pub mod extensions;
pub mod shared;

#[cfg(test)]
mod tests;

pub use config::GlobalConfig;
#[allow(unused_imports)]
pub use config::{LocalInferenceConfig, McpSection};
#[allow(unused_imports)]
pub use extensions::{AuthKind, AuthSpec, DriverKind, ToolDescriptor, TransportLimits};
#[allow(unused_imports)]
pub use shared::{
    GlobalTier, McpIssue, McpTier, ResolvedMcp, ServerStatus, discover_mcp_json_paths,
    load_global_at, resolve_for_assistant, resolve_here, resolve_in,
};
