//! Unit tests for `crate::permissions`, one module per concern (#7948).
//!
//! Why: the permission map is a security artifact; each acceptance criterion
//! on #7948 is pinned by a test that a wrong implementation fails.
//! What: `config_tests` (parse + fail-closed validation), `matcher_tests`
//! (globs, subjects, precedence), `gate_tests` (runtime decision, ask round
//! trip, session grants, redaction), `protocol_tests` (the JSON-RPC surface).
//! Test: this module's children.

mod config_tests;
mod gate_tests;
mod matcher_tests;
mod protocol_tests;

use crate::agents::config::{AgentConfig, AgentInfo, ToolsConfig};

/// Build an `AgentConfig` carrying the `permissions:` block in `yaml`, parsed
/// through the real parser so tests take the same path an agent file does.
pub(crate) fn agent_with_permissions(yaml: &str) -> AgentConfig {
    let document = format!("---\nname: t\nrole: engineer\n{yaml}---\n\nBody.\n");
    let permissions = crate::permissions::parse_permissions("test-agent", &document)
        .expect("test fixture must be a valid permissions block");
    AgentConfig {
        agent: AgentInfo {
            name: "t".into(),
            ..Default::default()
        },
        permissions,
        ..Default::default()
    }
}

/// Build an `AgentConfig` with a legacy `tcode_tools` allowlist and, if given,
/// a permissions block.
pub(crate) fn agent_with_allowlist(allowed: &[&str], yaml: Option<&str>) -> AgentConfig {
    let mut cfg = match yaml {
        Some(y) => agent_with_permissions(y),
        None => AgentConfig::default(),
    };
    cfg.tools = Some(ToolsConfig {
        allowed: Some(allowed.iter().map(|s| s.to_string()).collect()),
    });
    cfg
}
