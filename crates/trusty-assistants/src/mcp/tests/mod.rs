//! Tests for the two-tier MCP configuration (#7454).
//!
//! Why: split by the module under test so each file stays readable and the
//! `Test:` pointers in the source name a real module path.
//! What: `extensions_tests` (the `extensions` accessors), `migrate_tests` (the
//! one-time drain of the retired tables), `global_tests` (the shared file and
//! the `.mcp.json` merge), `resolve_tests` (the assistant tier and the
//! per-server statuses), and `schema_tests` (the one-schema regression grep).
//! Test: this module.

mod extensions_tests;
mod global_tests;
mod migrate_tests;
mod resolve_tests;
mod schema_tests;

use std::path::PathBuf;

use trusty_mcp::config::{McpServerConfig, McpTransport};

/// A private temporary directory for one test.
///
/// Why: every test here writes config files, and two tests sharing a directory
/// would make one's shared file the other's "already migrated" state.
pub(crate) fn tempdir(tag: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("trusty-agents-mcp-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// One enabled stdio server with no extensions.
pub(crate) fn stdio(name: &str, command: &str) -> McpServerConfig {
    McpServerConfig::new(
        name,
        McpTransport::Stdio {
            command: command.to_string(),
            args: Vec::new(),
            env: Default::default(),
        },
    )
}
