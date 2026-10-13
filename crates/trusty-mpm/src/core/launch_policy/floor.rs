//! The never-deny MCP floor (#8988, Architect ruling Q2).
//!
//! Why: an owner `deny` reaches every subagent in the session, so a server an
//! agent's `tools:` line grants must never be deniable. tm's own builtins also
//! sit in the operator's user scope, so `allow = []` would otherwise deny them.
//! What: [`is_floor`] answers for [`BUILTIN_MANAGED_MCP_SERVERS`] plus
//! [`EXTRA_FLOOR`]. Compiled in; no file can change it.
//! Test: `floor_covers_every_agent_tools_mcp_grant`, `floor_holds_the_builtins_and_chrome`.

use crate::core::mcp_config::BUILTIN_MANAGED_MCP_SERVERS;

/// Floor servers beyond tm's builtins.
///
/// Why: `content/agents/web-qa.md` grants `mcp__claude-in-chrome`, and the PM
/// routes browser work to web-qa.
/// What: `["claude-in-chrome"]`.
/// Test: `floor_covers_every_agent_tools_mcp_grant`.
pub const EXTRA_FLOOR: &[&str] = &["claude-in-chrome"];

/// Whether `name` is a never-deny MCP server.
///
/// Test: `floor_holds_the_builtins_and_chrome`.
pub fn is_floor(name: &str) -> bool {
    BUILTIN_MANAGED_MCP_SERVERS.contains(&name) || EXTRA_FLOOR.contains(&name)
}

#[cfg(test)]
#[path = "floor_tests.rs"]
mod tests;
