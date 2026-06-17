//! Bundled PM + sub-agent configs for the standalone metaharness (#1030).
//!
//! Why: The metaharness boots without the trusty-mpm daemon or `.claude/`
//! scaffolding, yet [`InProcessAgentRunner`] loads each agent from
//! `<config_dir>/<name>.toml`. Shipping the PM and engineer configs inline (and
//! materialising them to a run-scoped temp dir) lets `meta run --demo` drive a
//! real PM → engineer delegation with zero external setup, while still going
//! through the production on-disk agent-loading path.
//! What: [`PM_AGENT_NAME`]/[`ENGINEER_AGENT_NAME`] name the two agents;
//! [`pm_agent_toml`]/[`engineer_agent_toml`] return their TOML bodies (the PM is
//! allowed only `delegate_to_agent` + read tools; the engineer gets the full
//! fs/bash set); [`write_agent_configs`] materialises both into a directory and
//! returns it.
//! Test: `agents::tests` assert the bodies parse as `AgentConfig`s with the
//! expected allowlists, and that `write_agent_configs` writes both files.
//!
//! [`InProcessAgentRunner`]: trusty_code::runner::InProcessAgentRunner

use std::path::Path;

use anyhow::Context as _;

/// Dispatch name (and config file stem) of the project-manager agent.
///
/// Why: The orchestrator loads `<dir>/pm.toml` and runs the PM loop against this
/// agent; centralising the slug keeps the loader, the config file, and the tests
/// in lockstep.
/// What: the literal `"pm"`.
/// Test: `pm_toml_parses_with_delegate_allowed`.
pub(crate) const PM_AGENT_NAME: &str = "pm";

/// Dispatch name (and config file stem) of the sub-agent the PM delegates to.
///
/// Why: The demo drives one delegation to this agent; the delegate tool
/// validates the name against `<dir>/<name>.toml`, so the slug must match.
/// What: the literal `"python-engineer"`.
/// Test: `engineer_toml_parses_with_fs_tools`.
pub(crate) const ENGINEER_AGENT_NAME: &str = "python-engineer";

/// TOML body for the PM agent config.
///
/// Why: The PM's job is to delegate, not to touch files directly; restricting it
/// to `delegate_to_agent` (plus read-only inspection) mirrors the MPM protocol
/// where the PM orchestrates and sub-agents do the work. Acceptance criterion
/// #1030 requires the PM to load a "delegate tool + optional read tools" config.
/// What: an `AgentConfig` TOML pinning the default model and allowing only
/// `delegate_to_agent` and `read_file`; the system prompt instructs the PM to
/// delegate the task to the engineer and report the result.
/// Test: `pm_toml_parses_with_delegate_allowed`.
pub(crate) fn pm_agent_toml() -> String {
    r#"[agent]
name = "pm"
role = "project-manager"
description = "Orchestrates the run by delegating to sub-agents."

[system_prompt]
content = """
You are the PM (project manager) of an in-process agent harness.
You do not write files yourself. To accomplish the user's task you MUST call the
delegate_to_agent tool exactly once, delegating the concrete engineering work to
the `python-engineer` agent. Pass the full task as the `task` argument. After the
engineer reports back, summarise what was accomplished in one short paragraph and
then stop.
"""

[tools]
allowed = ["delegate_to_agent", "read_file"]
"#
    .to_string()
}

/// TOML body for the engineer sub-agent config.
///
/// Why: The engineer is the actor that actually performs file I/O and shell work,
/// so it needs the full fs/bash tool set. Keeping its allowlist explicit makes
/// the capability grant auditable.
/// What: an `AgentConfig` TOML pinning the default model and allowing the fs
/// tools (`read_file`, `write_file`, `edit`) and `bash`; the system prompt
/// instructs it to carry out the task and confirm completion.
/// Test: `engineer_toml_parses_with_fs_tools`.
pub(crate) fn engineer_agent_toml() -> String {
    r#"[agent]
name = "python-engineer"
role = "engineer"
description = "Performs concrete engineering work: file writes, edits, shell."

[system_prompt]
content = """
You are a software engineer in an in-process agent harness.
Carry out the task you are given using the available tools (write_file, edit,
read_file, bash). When the task asks you to create a file, use the write_file
tool with the requested relative path and contents. After the work is done,
reply with a one-line confirmation of exactly what you changed, then stop.
"""

[tools]
allowed = ["read_file", "write_file", "edit", "bash"]
"#
    .to_string()
}

/// Materialise the PM + engineer configs into `dir`, returning nothing on success.
///
/// Why: The runner and delegate tool both load agents from on-disk TOML; writing
/// the bundled configs into a run-scoped directory lets the demo exercise the
/// real loading path without requiring the operator to author config files.
/// What: Creates `dir` (if needed) and writes `pm.toml` and `python-engineer.toml`
/// with the bundled bodies; surfaces an `anyhow` error naming the offending path
/// on any IO failure.
/// Test: `write_agent_configs_writes_both_files`.
pub(crate) fn write_agent_configs(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create agents dir: {}", dir.display()))?;
    let pm_path = dir.join(format!("{PM_AGENT_NAME}.toml"));
    std::fs::write(&pm_path, pm_agent_toml())
        .with_context(|| format!("failed to write PM config: {}", pm_path.display()))?;
    let eng_path = dir.join(format!("{ENGINEER_AGENT_NAME}.toml"));
    std::fs::write(&eng_path, engineer_agent_toml())
        .with_context(|| format!("failed to write engineer config: {}", eng_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use trusty_code::agents::AgentConfig;

    use super::*;

    /// The PM config parses and allows only delegate + read.
    ///
    /// Why: The PM must be able to delegate but not write files directly; a
    /// regression in the allowlist would let the PM bypass the engineer.
    /// What: Parse `pm_agent_toml`; assert name and the exact allowlist.
    /// Test: this test.
    #[test]
    fn pm_toml_parses_with_delegate_allowed() {
        let cfg = AgentConfig::from_toml_str(&pm_agent_toml()).expect("pm toml parses");
        assert_eq!(cfg.agent.name, PM_AGENT_NAME);
        let allowed = cfg
            .tools
            .as_ref()
            .and_then(|t| t.allowed.as_ref())
            .expect("pm has an allowlist");
        assert!(allowed.contains(&"delegate_to_agent".to_string()));
        assert!(allowed.contains(&"read_file".to_string()));
        assert!(
            !allowed.contains(&"write_file".to_string()),
            "PM must not be allowed to write files directly"
        );
    }

    /// The engineer config parses and allows the fs/bash tool set.
    ///
    /// Why: The engineer is the actor that performs file I/O; it must have the
    /// write/edit/bash capabilities.
    /// What: Parse `engineer_agent_toml`; assert name and allowlist membership.
    /// Test: this test.
    #[test]
    fn engineer_toml_parses_with_fs_tools() {
        let cfg = AgentConfig::from_toml_str(&engineer_agent_toml()).expect("engineer toml parses");
        assert_eq!(cfg.agent.name, ENGINEER_AGENT_NAME);
        let allowed = cfg
            .tools
            .as_ref()
            .and_then(|t| t.allowed.as_ref())
            .expect("engineer has an allowlist");
        for tool in ["read_file", "write_file", "edit", "bash"] {
            assert!(
                allowed.contains(&tool.to_string()),
                "engineer must be allowed {tool}"
            );
        }
    }

    /// `write_agent_configs` writes both TOML files into the target dir.
    ///
    /// Why: The runner loads agents from disk; both files must land.
    /// What: Write into a tempdir; assert both files exist and parse.
    /// Test: this test.
    #[test]
    fn write_agent_configs_writes_both_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_agent_configs(tmp.path()).expect("write configs");
        let pm = tmp.path().join("pm.toml");
        let eng = tmp.path().join("python-engineer.toml");
        assert!(pm.exists(), "pm.toml must be written");
        assert!(eng.exists(), "python-engineer.toml must be written");
        AgentConfig::load(&pm).expect("pm.toml loads");
        AgentConfig::load(&eng).expect("engineer toml loads");
    }
}
