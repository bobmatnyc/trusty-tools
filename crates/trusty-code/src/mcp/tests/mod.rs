//! Unit coverage for the MCP loader (#5428).
//!
//! Why: every test here points the loader at a tempdir, so no test run can
//! read — or spawn anything named by — the developer's real
//! `~/.trusty-tools/mcp/servers.toml`.
//! What: [`support`] writes the two tier files; the four submodules cover
//! config resolution, the trust gate, the tool adapter, and the assembled set.
//! The real transport is exercised in `crates/trusty-code/tests/mcp_loader_e2e.rs`.

mod config_tests;
mod set_tests;
mod spawn_tests;
mod tool_tests;
mod trust_tests;

/// Fixture writers shared by the submodules.
pub(super) mod support {
    use std::path::{Path, PathBuf};

    use trusty_mcp::config::{McpServerConfig, McpTransport};

    /// Write `text` as the global `servers.toml` inside `dir`.
    pub(crate) fn global_file(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("servers.toml");
        std::fs::write(&path, text).expect("writing the global fixture");
        path
    }

    /// Write `text` as `<project>/.trusty-code/mcp.toml`.
    pub(crate) fn project_file(project: &Path, text: &str) -> PathBuf {
        let dir = project.join(crate::paths::TRUSTY_CODE_DIRNAME);
        std::fs::create_dir_all(&dir).expect("creating the project config dir");
        let path = dir.join(super::super::config::PROJECT_MCP_FILE);
        std::fs::write(&path, text).expect("writing the project fixture");
        path
    }

    /// A global file declaring one stdio server named `name`.
    pub(crate) fn one_stdio_server(name: &str, command: &str) -> String {
        format!(
            "[[servers]]\nname = {name:?}\n[servers.transport]\ntype = \"stdio\"\ncommand = {command:?}\n"
        )
    }

    /// An in-memory stdio server config, for the gate tests.
    pub(crate) fn stdio(name: &str, command: &str, args: &[&str]) -> McpServerConfig {
        McpServerConfig::new(
            name,
            McpTransport::Stdio {
                command: command.to_string(),
                args: args.iter().map(|a| a.to_string()).collect(),
                env: std::collections::BTreeMap::new(),
            },
        )
    }
}
