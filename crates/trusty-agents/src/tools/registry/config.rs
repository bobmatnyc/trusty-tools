//! TOML schema for the OpenRPC tool registry (#453).
//!
//! Why: External JSON-RPC 2.0 endpoints (`gworkspace`, `trusty-memory`, etc.)
//! advertise tools via `rpc.discover` over OpenRPC (https://spec.open-rpc.org/). The
//! registry needs operator-provided configuration to know which endpoints
//! to contact, which scopes the operator trusts that endpoint to serve,
//! how to authenticate, and what transport limits to enforce. This module
//! is the deserialization schema; no behaviour lives here.
//! What: `ToolRegistryConfig` is the `[tool_registry]` section of
//! `~/.trusty-agents/config.toml`. Endpoints are listed under `[[endpoints]]`.
//! Test: Schema correctness is exercised indirectly through the registry
//! builder's startup path; this module's only contract is round-trip
//! deserialization which is tested in `mod.rs`.

use serde::{Deserialize, Serialize};

/// `[tool_registry]` section.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ToolRegistryConfig {
    /// How to handle scope violations: `"deny"` (default) refuses to expose
    /// out-of-scope tools; `"warn"` logs but still exposes them.
    #[serde(default)]
    pub scope_enforcement: ScopeEnforcement,
    /// One entry per remote endpoint (direct JSON-RPC or stdio-mcp).
    #[serde(default)]
    pub endpoints: Vec<EndpointConfig>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ScopeEnforcement {
    #[default]
    Deny,
    Warn,
}

/// A single endpoint entry. `driver` selects how we talk to it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EndpointConfig {
    pub name: String,
    pub driver: DriverKind,
    #[serde(default)]
    pub description: Option<String>,
    /// Reserved for a future `http-ompm` driver kind. Unused by the
    /// `direct` driver since #455 (which pivoted `direct` to stdio
    /// JSON-RPC 2.0). Kept on the schema so older configs that still
    /// carry a `url = "..."` line parse without error.
    #[serde(default)]
    pub url: Option<String>,
    /// Subprocess command — used by BOTH `driver = "direct"` (stdio
    /// JSON-RPC 2.0 / OpenRPC, #455) and `driver = "stdio-mcp"` (MCP
    /// `2024-11-05`). Path resolution follows `PATH` if the value has no
    /// `/`.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Operator-declared trusted scope patterns. Tools the endpoint
    /// advertises with scopes outside this list are filtered out at
    /// discovery time.
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default = "default_discovery_ttl_secs")]
    pub discovery_ttl_secs: u64,
    #[serde(default)]
    pub eager_discovery: bool,
    #[serde(default)]
    pub auth: Option<AuthConfig>,
    #[serde(default)]
    pub transport: Option<TransportConfig>,
}

fn default_true() -> bool {
    true
}

fn default_discovery_ttl_secs() -> u64 {
    300
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DriverKind {
    Direct,
    StdioMcp,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthConfig {
    pub kind: AuthKind,
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub header: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuthKind {
    None,
    BearerEnv,
    HeaderEnv,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct TransportConfig {
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_concurrency: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_direct_endpoint() {
        // #455: `direct` is stdio JSON-RPC 2.0; uses `command` + `args`.
        let toml_str = r#"
            scope_enforcement = "deny"

            [[endpoints]]
            name = "gworkspace"
            driver = "direct"
            command = "gworkspace-rpc"
            args = []
            scopes = ["google.*"]
        "#;
        let cfg: ToolRegistryConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.endpoints.len(), 1);
        let ep = &cfg.endpoints[0];
        assert_eq!(ep.name, "gworkspace");
        assert_eq!(ep.driver, DriverKind::Direct);
        assert!(ep.enabled);
        assert_eq!(ep.scopes, vec!["google.*"]);
        assert_eq!(ep.discovery_ttl_secs, 300);
        assert_eq!(ep.command.as_deref(), Some("gworkspace-rpc"));
    }

    #[test]
    fn parses_legacy_url_field_without_error() {
        // (#455) Older configs may still carry `url =`. Schema accepts
        // it for back-compat but the `direct` driver ignores it.
        let toml_str = r#"
            [[endpoints]]
            name = "legacy"
            driver = "direct"
            command = "legacy-rpc"
            url = "http://127.0.0.1:9999/rpc"
        "#;
        let cfg: ToolRegistryConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            cfg.endpoints[0].url.as_deref(),
            Some("http://127.0.0.1:9999/rpc")
        );
        assert_eq!(cfg.endpoints[0].command.as_deref(), Some("legacy-rpc"));
    }

    #[test]
    fn defaults_when_section_absent() {
        let cfg: ToolRegistryConfig = toml::from_str("").unwrap();
        assert!(cfg.endpoints.is_empty());
        assert_eq!(cfg.scope_enforcement, ScopeEnforcement::Deny);
    }
}
