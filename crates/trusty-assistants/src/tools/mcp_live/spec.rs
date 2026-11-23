//! Live-discovery MCP server specs, drawn from the resolved server set (#3238, #7454).
//!
//! Why: live discovery needs a uniform "enough to spawn" shape. Until #7454
//! this module produced that shape by merging TWO on-disk schemas itself —
//! `[[mcp.services]]` entries and a second `.mcp.json` parser — and by
//! applying its own precedence between them. Both of those moved: ADR-0060
//! makes `trusty_mcp::config::McpServerConfig` the one shape, and
//! [`crate::mcp::shared`] does the `.mcp.json` merge and the precedence once,
//! for every consumer. What is left here is the projection onto what a spawn
//! actually needs.
//!
//! What: [`McpServerSpec`] is that projection; [`gather_specs`] selects the
//! servers eligible for it. The eligibility rules are unchanged — enabled,
//! `discover = true`, a stdio transport, a non-empty command — and so is the
//! trust posture: a project's `.mcp.json` reaches the resolved set only when
//! the operator set `mcp.trust_project_mcp_json`, which is enforced where the
//! merge happens rather than here (#3266).
//!
//! Test: `tests` below, plus
//! `crate::mcp::tests::global_tests::untrusted_mcp_json_contributes_nothing`
//! for the trust gate itself.

use std::collections::HashMap;

use trusty_mcp::config::McpServerConfig;

use crate::mcp::extensions;

/// Where a live-discovery spec came from. Carried only for tracing/debug
/// messages — dispatch behavior is identical regardless of source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecSource {
    /// A server declared in the shared file or an assistant's `[mcp]` table.
    ConfigService,
    /// A server imported from a trusted project `.mcp.json`.
    McpJson,
}

/// A live-discovery-eligible MCP server: enough to spawn + `tools/list`.
#[derive(Debug, Clone)]
pub struct McpServerSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub source: SpecSource,
}

/// Project one assistant's resolved servers onto the spawnable specs.
///
/// Why: both assembly points (`crate::ctrl::pm_task::dispatch::persona` and
/// `crate::runtime::subagent_mode`) need the identical list, so one function
/// decides eligibility. Taking the RESOLVED set as an argument (rather than
/// loading config here) is what makes an assistant-level disable reach live
/// discovery — the caller has already applied the assistant tier.
/// What: keeps every server that is enabled, declares `discover = true`, has
/// a stdio transport, and names a non-empty command. Each other case is
/// skipped with a debug line rather than an error: one misconfigured server
/// must not cost an assistant its other connectors. Order follows the resolved
/// set, so the shared file's entries precede any `.mcp.json` additions and
/// first-wins deduplication downstream stays deterministic.
/// Test: `gather_specs_includes_discover_servers`,
/// `gather_specs_skips_non_stdio_and_empty_command`,
/// `gather_specs_skips_servers_that_do_not_discover`,
/// `gather_specs_marks_mcp_json_sourced_servers`.
pub fn gather_specs(servers: &[&McpServerConfig]) -> Vec<McpServerSpec> {
    let mut specs: Vec<McpServerSpec> = Vec::new();
    for server in servers {
        if !server.enabled || !extensions::discover(server) {
            continue;
        }
        let Some((command, args, env)) = extensions::stdio_parts(server) else {
            tracing::debug!(
                service = %server.name,
                transport = extensions::transport_label(server),
                "mcp_live: skipping non-stdio discover=true server (only stdio transport supported)"
            );
            continue;
        };
        if command.is_empty() {
            tracing::debug!(
                service = %server.name,
                "mcp_live: skipping discover=true server with empty command"
            );
            continue;
        }
        let source = match extensions::source(server).as_deref() {
            Some(extensions::SOURCE_MCP_JSON) => SpecSource::McpJson,
            _ => SpecSource::ConfigService,
        };
        specs.push(McpServerSpec {
            name: server.name.clone(),
            command: command.to_string(),
            args: args.to_vec(),
            env: env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            source,
        });
    }
    specs
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mcp::config::McpTransport;

    fn discover_server(name: &str, command: &str) -> McpServerConfig {
        let mut server = McpServerConfig::new(
            name,
            McpTransport::Stdio {
                command: command.to_string(),
                args: vec!["serve".to_string()],
                env: Default::default(),
            },
        );
        extensions::set(&mut server.extensions, extensions::DISCOVER, &true);
        server
    }

    fn specs_of(servers: &[McpServerConfig]) -> Vec<McpServerSpec> {
        let refs: Vec<&McpServerConfig> = servers.iter().collect();
        gather_specs(&refs)
    }

    #[test]
    fn gather_specs_includes_discover_servers() {
        let servers = vec![discover_server("trusty-memory", "trusty-memory")];
        let specs = specs_of(&servers);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "trusty-memory");
        assert_eq!(specs[0].command, "trusty-memory");
        assert_eq!(specs[0].args, vec!["serve".to_string()]);
        assert_eq!(specs[0].source, SpecSource::ConfigService);
    }

    #[test]
    fn gather_specs_skips_servers_that_do_not_discover() {
        let mut server = discover_server("static-only", "static-bin");
        server.extensions.remove(extensions::DISCOVER);
        assert!(specs_of(&[server]).is_empty());
    }

    #[test]
    fn gather_specs_skips_disabled_servers() {
        let mut server = discover_server("off", "off-bin");
        server.enabled = false;
        assert!(specs_of(&[server]).is_empty());
    }

    #[test]
    fn gather_specs_skips_non_stdio_and_empty_command() {
        let mut remote = discover_server("remote", "unused");
        remote.transport = McpTransport::Http {
            url: "https://example.com/mcp".to_string(),
            headers: Default::default(),
        };
        let commandless = discover_server("commandless", "");
        assert!(specs_of(&[remote, commandless]).is_empty());
    }

    /// #7454: the resolved set marks a `.mcp.json` import with the `source`
    /// extension, and the spec carries that through so a tracing line can
    /// still say where an auto-spawned server came from.
    #[test]
    fn gather_specs_marks_mcp_json_sourced_servers() {
        let mut server = discover_server("from-repo", "repo-bin");
        extensions::set(
            &mut server.extensions,
            extensions::SOURCE,
            &extensions::SOURCE_MCP_JSON,
        );
        let specs = specs_of(&[server]);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].source, SpecSource::McpJson);
    }
}
