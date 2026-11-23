//! `[tool_registry]` — the registry's POLICY, not its endpoint list (#453, #7454).
//!
//! Why: external JSON-RPC 2.0 endpoints advertise tools via `rpc.discover`
//! over OpenRPC (<https://spec.open-rpc.org/>), and the registry needs to know
//! which ones to contact, what scopes it trusts them for, and how to
//! authenticate. Until #7454 all of that lived in an `[[endpoints]]` array
//! here — a SECOND MCP-server shape inside the same crate that already had
//! `[[mcp.services]]`, which is the duplication ADR-0060 exists to end.
//!
//! What survives here is the one knob that is not a server description:
//! [`ScopeEnforcement`], the policy applied to every endpoint alike. The
//! endpoints themselves are now `trusty_mcp::config::McpServerConfig` entries
//! in the shared file, carrying `driver`, `scopes`, `discovery_ttl_secs`,
//! `eager_discovery`, `auth` and `transport_limits` in
//! [`crate::mcp::extensions`]. An `[[endpoints]]` array left over from before
//! #7454 is ignored by this struct and drained exactly once by
//! [`crate::mcp::shared::migrate`].
//!
//! Test: `super::tests::builder_with_no_endpoints_returns_empty`;
//! `crate::mcp::tests::migrate_tests::converts_a_legacy_registry_endpoint`
//! covers the drain.

use serde::{Deserialize, Serialize};

/// The `[tool_registry]` section.
///
/// Why: kept as a section (rather than folded into `[mcp]`) so every existing
/// `config.toml` keeps parsing and `scope_enforcement` keeps its spelling.
/// What: `scope_enforcement` alone. `Default` is `deny`, the safe answer.
/// Test: `defaults_when_section_absent`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ToolRegistryConfig {
    /// How to handle scope violations: `"deny"` (default) refuses to expose
    /// out-of-scope tools; `"warn"` logs but still exposes them.
    #[serde(default)]
    pub scope_enforcement: ScopeEnforcement,
}

/// What to do with a tool whose scope no declared pattern admits.
///
/// Test: `defaults_when_section_absent`.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ScopeEnforcement {
    /// Drop the tool.
    #[default]
    Deny,
    /// Keep the tool, log the violation.
    Warn,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_section_absent() {
        let cfg: ToolRegistryConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.scope_enforcement, ScopeEnforcement::Deny);
    }

    /// #7454: a `config.toml` still carrying the retired `[[endpoints]]` array
    /// must keep parsing — the array is drained by the migration, not by this
    /// struct, and a hard error here would fail every upgrading user's load.
    #[test]
    fn a_legacy_endpoints_array_still_parses() {
        let toml_str = r#"
            scope_enforcement = "warn"

            [[endpoints]]
            name = "gworkspace"
            driver = "direct"
            command = "trusty-gworkspace-mcp"
        "#;
        let cfg: ToolRegistryConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.scope_enforcement, ScopeEnforcement::Warn);
    }
}
