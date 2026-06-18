//! Bundled PM + sub-agent configs for the standalone metaharness (#1030, #1048).
//!
//! Why: The metaharness boots without the trusty-mpm daemon or `.claude/`
//! scaffolding, yet [`InProcessAgentRunner`] loads each agent from
//! `<config_dir>/<name>.toml`. As of WI-3 (#1048) the PM and sub-agent prompts are
//! no longer hard-coded inline TOML literals — they are *assembled* by the
//! instruction-loading layer ([`super::instructions`]): the PM prompt layers the
//! bundled PM sections with the project's `.trusty-mpm/` overrides (floored by
//! `BASE_PM.md`), and the engineer prompt is sourced from its bundled
//! `src/assets/agents/<name>.md` asset. This module owns the *policy* (which agents
//! exist, what tools each may call) and materialises the assembled configs to disk
//! so the demo exercises the real on-disk agent-loading path.
//! What: [`PM_AGENT_NAME`]/[`ENGINEER_AGENT_NAME`] name the two agents and
//! [`PM_TOOLS`]/[`ENGINEER_TOOLS`] declare their tool allowlists (the PM gets only
//! `delegate_to_agent` + read; the engineer gets the full fs/bash set).
//! [`write_agent_configs`] builds both [`AgentConfig`]s via [`super::instructions`]
//! (threading the project dir so PM overrides resolve), serialises them to TOML,
//! and writes them into the run-scoped config dir.
//! Test: `agents::tests` assert the written files parse as `AgentConfig`s with the
//! assembled prompts and expected allowlists.
//!
//! [`InProcessAgentRunner`]: trusty_code::runner::InProcessAgentRunner

use std::path::Path;

use anyhow::Context as _;

use super::instructions::{pm_agent_config, sub_agent_config};

/// Dispatch name (and config file stem) of the project-manager agent.
///
/// Why: The orchestrator loads `<dir>/pm.toml` and runs the PM loop against this
/// agent; centralising the slug keeps the loader, the config file, and the tests
/// in lockstep.
/// What: the literal `"pm"`.
/// Test: `pm_config_written_with_assembled_prompt`.
pub(crate) const PM_AGENT_NAME: &str = "pm";

/// Dispatch name (and config file stem) of the sub-agent the PM delegates to.
///
/// Why: The demo drives one delegation to this agent; the delegate tool
/// validates the name against `<dir>/<name>.toml`, so the slug must match a
/// bundled `src/assets/agents/<name>.md` asset.
/// What: the literal `"python-engineer"` (its prompt is the bundled
/// python-engineer asset, loaded by [`super::instructions`]).
/// Test: `engineer_config_written_with_asset_prompt`.
pub(crate) const ENGINEER_AGENT_NAME: &str = "python-engineer";

/// Tool allowlist for the PM agent.
///
/// Why: The PM's job is to delegate, not to touch files directly; restricting it
/// to `delegate_to_agent` (plus read-only inspection) mirrors the MPM protocol
/// where the PM orchestrates and sub-agents do the work (#1030 AC).
/// What: `delegate_to_agent` + `read_file`.
/// Test: `pm_config_written_with_assembled_prompt` asserts the allowlist.
pub(crate) const PM_TOOLS: &[&str] = &["delegate_to_agent", "read_file"];

/// Tool allowlist for the engineer sub-agent.
///
/// Why: The engineer is the actor that actually performs file I/O and shell work,
/// so it needs the full fs/bash tool set. Keeping its allowlist explicit makes the
/// capability grant auditable.
/// What: `read_file`, `write_file`, `edit`, `bash`.
/// Test: `engineer_config_written_with_asset_prompt` asserts the allowlist.
pub(crate) const ENGINEER_TOOLS: &[&str] = &["read_file", "write_file", "edit", "bash"];

/// Materialise the PM + engineer configs into `dir`, resolving prompts for `project`.
///
/// Why: The runner and delegate tool both load agents from on-disk TOML; writing
/// the assembled configs into a run-scoped directory lets the demo exercise the
/// real loading path while honouring the project's custom instructions (#1048):
/// the PM prompt is layered with `<project>/.trusty-mpm/` overrides and the
/// engineer prompt is sourced from its bundled asset.
/// What: Builds the PM config (via [`pm_agent_config`], threading `project` so
/// overrides resolve) and the engineer config (via [`sub_agent_config`], loading
/// the bundled asset), serialises each to TOML, creates `dir` (if needed), and
/// writes `pm.toml` + `python-engineer.toml`. Surfaces an `anyhow` error naming the
/// offending step on any failure (unknown agent, serialisation, or IO).
/// Test: `pm_config_written_with_assembled_prompt`,
/// `engineer_config_written_with_asset_prompt`, `write_agent_configs_writes_both_files`.
pub(crate) fn write_agent_configs(dir: &Path, project: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create agents dir: {}", dir.display()))?;

    let pm = pm_agent_config(PM_AGENT_NAME, project, None, PM_TOOLS);
    let pm_toml = toml::to_string(&pm).context("failed to serialise PM agent config")?;
    let pm_path = dir.join(format!("{PM_AGENT_NAME}.toml"));
    std::fs::write(&pm_path, pm_toml)
        .with_context(|| format!("failed to write PM config: {}", pm_path.display()))?;

    let engineer = sub_agent_config(ENGINEER_AGENT_NAME, None, ENGINEER_TOOLS)
        .context("failed to load engineer sub-agent prompt")?;
    let eng_toml =
        toml::to_string(&engineer).context("failed to serialise engineer agent config")?;
    let eng_path = dir.join(format!("{ENGINEER_AGENT_NAME}.toml"));
    std::fs::write(&eng_path, eng_toml)
        .with_context(|| format!("failed to write engineer config: {}", eng_path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use trusty_code::agents::AgentConfig;

    use super::*;

    /// The written PM config carries the assembled prompt and the delegate-only
    /// allowlist.
    ///
    /// Why: The PM must be able to delegate but not write files directly, and its
    /// prompt must come from the assembly layer (so project overrides take effect);
    /// a regression in either would break the orchestration contract.
    /// What: Write into a tempdir whose project carries a `PM_INSTRUCTIONS_DEPLOYED.md`
    /// override; load `pm.toml`; assert the override text is present, the BASE_PM
    /// floor is present, and the allowlist is delegate + read only.
    /// Test: this test.
    #[test]
    fn pm_config_written_with_assembled_prompt() {
        let project = tempfile::tempdir().expect("project tempdir");
        let override_dir = project.path().join(".trusty-mpm");
        std::fs::create_dir_all(&override_dir).expect("override dir");
        std::fs::write(
            override_dir.join("PM_INSTRUCTIONS_DEPLOYED.md"),
            "PM_OVERRIDE_FROM_PROJECT\n",
        )
        .expect("write override");

        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        write_agent_configs(cfg_dir.path(), project.path()).expect("write configs");

        let pm = AgentConfig::load(&cfg_dir.path().join("pm.toml")).expect("pm.toml loads");
        assert_eq!(pm.agent.name, PM_AGENT_NAME);
        assert!(
            pm.system_prompt
                .content
                .contains("PM_OVERRIDE_FROM_PROJECT"),
            "project override must be assembled into the PM prompt"
        );
        assert!(
            pm.system_prompt
                .content
                .contains("# BASE_PM Framework Floor"),
            "BASE_PM floor must be present"
        );
        let allowed = pm
            .tools
            .as_ref()
            .and_then(|t| t.allowed.as_ref())
            .expect("pm allowlist");
        assert!(allowed.contains(&"delegate_to_agent".to_string()));
        assert!(!allowed.contains(&"write_file".to_string()));
    }

    /// The written engineer config carries the bundled-asset prompt and fs/bash
    /// allowlist.
    ///
    /// Why: The engineer's behaviour must come from its bundled asset (#1048), not
    /// an inline literal, and it must have the write/edit/bash capabilities.
    /// What: Write configs; load `python-engineer.toml`; assert the asset body is
    /// present (no frontmatter) and the allowlist contains the fs/bash tools.
    /// Test: this test.
    #[test]
    fn engineer_config_written_with_asset_prompt() {
        let project = tempfile::tempdir().expect("project tempdir");
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        write_agent_configs(cfg_dir.path(), project.path()).expect("write configs");

        let eng = AgentConfig::load(&cfg_dir.path().join("python-engineer.toml"))
            .expect("engineer toml loads");
        assert_eq!(eng.agent.name, ENGINEER_AGENT_NAME);
        assert!(
            eng.system_prompt.content.contains("Python"),
            "engineer prompt must come from the bundled python-engineer asset"
        );
        assert!(
            !eng.system_prompt.content.starts_with("---"),
            "frontmatter must be stripped from the engineer prompt"
        );
        let allowed = eng
            .tools
            .as_ref()
            .and_then(|t| t.allowed.as_ref())
            .expect("engineer allowlist");
        for tool in ["read_file", "write_file", "edit", "bash"] {
            assert!(
                allowed.contains(&tool.to_string()),
                "engineer must be allowed {tool}"
            );
        }
    }

    /// `write_agent_configs` writes both TOML files into the target dir.
    ///
    /// Why: The runner loads agents from disk; both files must land and parse.
    /// What: Write into a tempdir; assert both files exist and load as configs.
    /// Test: this test.
    #[test]
    fn write_agent_configs_writes_both_files() {
        let project = tempfile::tempdir().expect("project tempdir");
        let cfg_dir = tempfile::tempdir().expect("cfg tempdir");
        write_agent_configs(cfg_dir.path(), project.path()).expect("write configs");
        let pm = cfg_dir.path().join("pm.toml");
        let eng = cfg_dir.path().join("python-engineer.toml");
        assert!(pm.exists(), "pm.toml must be written");
        assert!(eng.exists(), "python-engineer.toml must be written");
        AgentConfig::load(&pm).expect("pm.toml loads");
        AgentConfig::load(&eng).expect("engineer toml loads");
    }
}
