//! Custom-instruction loading for the standalone metaharness (#1048, WI-3).
//!
//! Why: WI-4's `agents.rs` originally hard-coded the PM and engineer system
//! prompts as inline TOML string literals. That bypassed the trusty-mpm
//! instruction machinery entirely — a project's `.trusty-mpm/` overrides
//! (advertised by `BASE_PM.md`, made real by `core::instruction_overrides`) were
//! silently ignored, and every sub-agent's behaviour was frozen in a Rust string
//! rather than sourced from the bundled `src/assets/agents/<name>.md` prompt the
//! rest of trusty-mpm already ships. This module is the instruction-loading layer
//! WI-4's orchestrator consumes: it *reuses* the existing assembly machinery
//! rather than reinventing it, so the metaharness PM honours the same override
//! contract a daemon-launched PM does, and sub-agents inherit their bundled
//! prompts.
//! What: [`assemble_pm_prompt`] is a thin wrapper over
//! [`resolve_pm_prompt`](trusty_mpm::core::instruction_overrides::resolve_pm_prompt)
//! (bundled PM sections layered with project `.trusty-mpm/` overrides, always
//! floored by the non-overridable `BASE_PM.md`). [`load_agent_prompt`] resolves a
//! sub-agent's prompt body from the compile-time bundle ([`bundle::ALL`]),
//! stripping YAML frontmatter so only the Markdown instruction body remains.
//! [`pm_agent_config`] and [`sub_agent_config`] map each assembled/loaded prompt
//! into a trusty-code [`AgentConfig`] (with the agent's model + tool allowlist),
//! which the orchestrator feeds to `trusty_code::prompt::assemble_system_prompt`.
//! Missing sub-agents return a clean [`MetaInstructionError`] rather than panic.
//! Test: the `tests` module proves PM assembly layers a project override above
//! the BASE_PM floor, sub-agent prompts load from the asset and map into an
//! `AgentConfig`, and an unknown agent name yields an error (not a panic).

use std::path::Path;

use trusty_code::agents::{AgentConfig, AgentInfo, SystemPrompt, ToolsConfig};
use trusty_mpm::core::bundle;
use trusty_mpm::core::instruction_overrides::resolve_pm_prompt;

/// A failure raised while loading metaharness instructions.
///
/// Why: Sub-agent prompt loading can fail for a recoverable, operator-facing
/// reason — the requested agent is not in the bundle — and the harness must
/// surface that as a typed, non-panicking error so `meta run` can report it
/// cleanly. A `thiserror` enum keeps the failure surface explicit and lets
/// callers `?`-propagate into `anyhow`.
/// What: [`UnknownAgent`](MetaInstructionError::UnknownAgent) names the agent slug
/// that had no bundled `src/assets/agents/<name>.md` asset.
/// Test: `unknown_agent_prompt_errors` asserts the variant and its message.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MetaInstructionError {
    /// No bundled agent asset matched the requested name.
    #[error(
        "unknown sub-agent '{0}': no bundled prompt at src/assets/agents/{0}.md \
         (see trusty-mpm core::bundle for the available agents)"
    )]
    UnknownAgent(String),
}

/// Assemble the effective PM system prompt for `project_dir`.
///
/// Why: The metaharness PM must obey the same custom-instruction contract a
/// daemon-launched PM does — bundled PM sections, layered with the project's
/// `.trusty-mpm/` overrides, always floored by the non-overridable `BASE_PM.md`.
/// Reusing [`resolve_pm_prompt`] (rather than re-implementing the layering) is
/// the single source of truth #1048 requires, so the two launch paths can never
/// diverge.
/// What: Delegates verbatim to
/// [`resolve_pm_prompt`](trusty_mpm::core::instruction_overrides::resolve_pm_prompt),
/// which reads any override files under `<project_dir>/.trusty-mpm/` and appends
/// the BASE_PM floor last.
/// Test: `pm_prompt_layers_project_override_above_base_floor`,
/// `pm_prompt_without_overrides_is_bundled`.
pub(crate) fn assemble_pm_prompt(project_dir: &Path) -> String {
    resolve_pm_prompt(project_dir)
}

/// Load a sub-agent's instruction body from the compile-time bundle.
///
/// Why: Sub-agent prompts live in `src/assets/agents/<name>.md` (already embedded
/// via [`bundle::ALL`]); the metaharness should source them from there rather
/// than from a hard-coded Rust string, so a sub-agent's behaviour stays in lockstep
/// with the catalog the rest of trusty-mpm ships and deploys. The asset carries
/// YAML frontmatter (name/role/model/…) plus a Markdown body; only the body is the
/// instruction the LLM should receive.
/// What: Looks up `agents/<name>.md` in [`bundle::ALL`]; on a hit, returns the
/// asset body with its leading YAML frontmatter block stripped (see
/// [`strip_frontmatter`]); on a miss, returns
/// [`MetaInstructionError::UnknownAgent`].
/// Test: `engineer_prompt_loads_from_asset`, `loaded_prompt_omits_frontmatter`,
/// `unknown_agent_prompt_errors`.
pub(crate) fn load_agent_prompt(agent_name: &str) -> Result<String, MetaInstructionError> {
    let rel_path = format!("agents/{agent_name}.md");
    let raw = bundle::ALL
        .iter()
        .find(|artifact| artifact.rel_path == rel_path)
        .map(|artifact| artifact.contents)
        .ok_or_else(|| MetaInstructionError::UnknownAgent(agent_name.to_string()))?;
    Ok(strip_frontmatter(raw).trim().to_string())
}

/// Strip a leading YAML frontmatter block from a bundled agent asset.
///
/// Why: Agent assets begin with a `---\n…\n---\n` YAML block (name/role/model/…)
/// that is metadata, not instruction text; feeding it to the LLM as part of the
/// system prompt would waste tokens and leak scaffolding into the model's context.
/// What: If `raw` starts with a `---` fence line, drops everything up to and
/// including the matching closing `---` fence and returns the remaining body;
/// otherwise returns `raw` unchanged (an asset without frontmatter is already a
/// body).
/// Test: `loaded_prompt_omits_frontmatter`, `strip_frontmatter_passes_through_bodyless`.
fn strip_frontmatter(raw: &str) -> &str {
    // The opening fence must be the very first line (a line that is exactly `---`,
    // ignoring a trailing CR for CRLF files). If it is not, there is no
    // frontmatter and the whole input is already a body.
    let first_line = raw.lines().next().unwrap_or("");
    if first_line.trim_end_matches('\r') != "---" {
        return raw;
    }
    // Skip past the opening fence line, then scan for the closing `---` fence
    // line. `split_inclusive('\n')` keeps line terminators so byte offsets line
    // up with the original string, letting us return a borrowed body slice.
    let mut consumed = first_line.len();
    // Account for the opening fence's own newline (LF or CRLF) if present.
    consumed += raw[consumed..]
        .strip_prefix("\r\n")
        .map(|_| 2)
        .or_else(|| raw[consumed..].strip_prefix('\n').map(|_| 1))
        .unwrap_or(0);

    let mut offset = consumed;
    for line in raw[consumed..].split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            // Body begins immediately after this closing fence line.
            return &raw[offset + line.len()..];
        }
        offset += line.len();
    }
    // Unterminated frontmatter — treat the whole thing as a body (robustness).
    raw
}

/// Build the PM [`AgentConfig`] with its assembled prompt mapped in.
///
/// Why: The orchestrator loads an [`AgentConfig`] and feeds
/// `config.system_prompt.content` into `trusty_code::prompt::assemble_system_prompt`.
/// To make the metaharness PM honour project overrides, the assembled PM prompt
/// (from [`assemble_pm_prompt`]) must be the config's `system_prompt.content`.
/// What: Returns an [`AgentConfig`] named `pm` (role `project-manager`) whose
/// `system_prompt.content` is [`assemble_pm_prompt`]'s output, with `model` set
/// (when `Some`) and the given tool allowlist applied. The PM is constrained to a
/// delegate + read-only surface by its `allowed` list, mirroring the MPM protocol
/// where the PM orchestrates and sub-agents act.
/// Test: `pm_config_carries_assembled_prompt_and_allowlist`.
pub(crate) fn pm_agent_config(
    pm_name: &str,
    project_dir: &Path,
    model: Option<&str>,
    allowed_tools: &[&str],
) -> AgentConfig {
    AgentConfig {
        agent: AgentInfo {
            name: pm_name.to_string(),
            role: Some("project-manager".to_string()),
            model: model.map(str::to_string),
            description: Some("Orchestrates the run by delegating to sub-agents.".to_string()),
        },
        system_prompt: SystemPrompt {
            content: assemble_pm_prompt(project_dir),
            append_skills: Vec::new(),
        },
        tools: Some(tools_allowing(allowed_tools)),
        ..AgentConfig::default()
    }
}

/// Build a sub-agent [`AgentConfig`] with its bundled prompt mapped in.
///
/// Why: Same contract as [`pm_agent_config`], but for a sub-agent: the
/// orchestrator needs an [`AgentConfig`] whose `system_prompt.content` is the
/// sub-agent's bundled instruction body, plus the model and tool capabilities it
/// is allowed. Sourcing the body from the bundle (via [`load_agent_prompt`]) keeps
/// the sub-agent's behaviour consistent with the shipped catalog.
/// What: Loads the prompt via [`load_agent_prompt`] (propagating
/// [`MetaInstructionError::UnknownAgent`] for an unknown name), then returns an
/// [`AgentConfig`] named `agent_name` (role `engineer`) with that prompt, the given
/// model, and the given tool allowlist.
/// Test: `sub_agent_config_maps_asset_into_config`,
/// `sub_agent_config_propagates_unknown_agent`.
pub(crate) fn sub_agent_config(
    agent_name: &str,
    model: Option<&str>,
    allowed_tools: &[&str],
) -> Result<AgentConfig, MetaInstructionError> {
    let content = load_agent_prompt(agent_name)?;
    Ok(AgentConfig {
        agent: AgentInfo {
            name: agent_name.to_string(),
            role: Some("engineer".to_string()),
            model: model.map(str::to_string),
            description: Some(
                "Performs concrete engineering work: file writes, edits, shell.".to_string(),
            ),
        },
        system_prompt: SystemPrompt {
            content,
            append_skills: Vec::new(),
        },
        tools: Some(tools_allowing(allowed_tools)),
        ..AgentConfig::default()
    })
}

/// Build a [`ToolsConfig`] allowlist from a slice of tool names.
///
/// Why: Both config builders share the same "explicit allowlist" wiring; one
/// helper keeps the `&[&str] → ToolsConfig` mapping in a single place.
/// What: Returns a [`ToolsConfig`] whose `allowed` is `Some(owned list)`.
/// Test: exercised via `pm_config_carries_assembled_prompt_and_allowlist` and
/// `sub_agent_config_maps_asset_into_config`.
fn tools_allowing(allowed_tools: &[&str]) -> ToolsConfig {
    ToolsConfig {
        allowed: Some(allowed_tools.iter().map(|t| t.to_string()).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    use super::super::agents::{ENGINEER_AGENT_NAME, PM_AGENT_NAME};

    /// The PM tool surface used in tests (mirrors the WI-4 demo PM allowlist).
    const PM_TOOLS: &[&str] = &["delegate_to_agent", "read_file"];
    /// The engineer tool surface used in tests.
    const ENGINEER_TOOLS: &[&str] = &["read_file", "write_file", "edit", "bash"];

    /// Write `<project>/.trusty-mpm/<name>` with `content`, creating dirs.
    fn write_override(project: &Path, name: &str, content: &str) {
        let dir = project.join(".trusty-mpm");
        fs::create_dir_all(&dir).expect("create .trusty-mpm");
        fs::write(dir.join(name), content).expect("write override");
    }

    /// A project with a custom `PM_INSTRUCTIONS_DEPLOYED.md` yields a PM prompt
    /// that contains the override text and is still floored by BASE_PM (#1048 AC).
    #[test]
    fn pm_prompt_layers_project_override_above_base_floor() {
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            "PM_INSTRUCTIONS_DEPLOYED.md",
            "# Wholly Custom PM\n\nMETAHARNESS_OVERRIDE_MARKER\n",
        );

        let prompt = assemble_pm_prompt(tmp.path());

        // The override text appears...
        assert!(
            prompt.contains("METAHARNESS_OVERRIDE_MARKER"),
            "override text must appear in the assembled PM prompt"
        );
        // ...and the non-overridable BASE_PM floor is still appended last.
        assert!(
            prompt.contains("# BASE_PM Framework Floor"),
            "BASE_PM floor must always be present"
        );
        let marker = prompt
            .find("METAHARNESS_OVERRIDE_MARKER")
            .expect("marker present");
        let floor = prompt
            .find("# BASE_PM Framework Floor")
            .expect("floor present");
        assert!(
            marker < floor,
            "the project override must precede (end above) the BASE_PM floor"
        );
    }

    /// With no overrides, the PM prompt is the bundled assembly (BASE_PM last).
    #[test]
    fn pm_prompt_without_overrides_is_bundled() {
        let tmp = TempDir::new().unwrap();
        let prompt = assemble_pm_prompt(tmp.path());
        assert!(prompt.contains("# PM Agent -- Claude MPM"));
        assert!(prompt.contains("# BASE_PM Framework Floor"));
    }

    /// The engineer sub-agent prompt loads from the bundled asset.
    #[test]
    fn engineer_prompt_loads_from_asset() {
        let prompt = load_agent_prompt(ENGINEER_AGENT_NAME).expect("engineer prompt loads");
        // Body of src/assets/agents/python-engineer.md.
        assert!(
            prompt.contains("Python"),
            "the python-engineer asset body must be loaded"
        );
        assert!(!prompt.is_empty(), "loaded prompt must be non-empty");
    }

    /// The loaded prompt has the YAML frontmatter stripped.
    #[test]
    fn loaded_prompt_omits_frontmatter() {
        let prompt = load_agent_prompt(ENGINEER_AGENT_NAME).expect("engineer prompt loads");
        assert!(
            !prompt.starts_with("---"),
            "frontmatter fence must be stripped from the loaded prompt"
        );
        assert!(
            !prompt.contains("extends: base-engineer"),
            "frontmatter keys must not leak into the instruction body"
        );
    }

    /// An unknown agent name returns a clean error rather than panicking (#1048).
    #[test]
    fn unknown_agent_prompt_errors() {
        let err = load_agent_prompt("no-such-agent-xyz")
            .expect_err("unknown agent must error, not panic");
        match &err {
            MetaInstructionError::UnknownAgent(name) => assert_eq!(name, "no-such-agent-xyz"),
        }
        assert!(format!("{err}").contains("no-such-agent-xyz"));
    }

    /// `strip_frontmatter` passes a body-only string through unchanged.
    #[test]
    fn strip_frontmatter_passes_through_bodyless() {
        let body = "# Just a Body\n\nNo frontmatter here.\n";
        assert_eq!(strip_frontmatter(body), body);
    }

    /// `strip_frontmatter` drops a leading YAML block and keeps the body.
    #[test]
    fn strip_frontmatter_drops_leading_block() {
        let doc = "---\nname: x\nrole: y\n---\n# Body\n\ncontent\n";
        let body = strip_frontmatter(doc);
        assert!(body.starts_with("# Body"));
        assert!(!body.contains("name: x"));
    }

    /// The PM config carries the assembled prompt + the model + the allowlist.
    #[test]
    fn pm_config_carries_assembled_prompt_and_allowlist() {
        let tmp = TempDir::new().unwrap();
        write_override(
            tmp.path(),
            "PM_INSTRUCTIONS_DEPLOYED.md",
            "PM_CONFIG_MARKER\n",
        );
        let cfg = pm_agent_config(PM_AGENT_NAME, tmp.path(), Some("anthropic/x"), PM_TOOLS);

        assert_eq!(cfg.agent.name, PM_AGENT_NAME);
        assert_eq!(cfg.agent.model.as_deref(), Some("anthropic/x"));
        assert!(cfg.system_prompt.content.contains("PM_CONFIG_MARKER"));
        assert!(
            cfg.system_prompt
                .content
                .contains("# BASE_PM Framework Floor")
        );
        let allowed = cfg
            .tools
            .as_ref()
            .and_then(|t| t.allowed.as_ref())
            .expect("pm has an allowlist");
        assert!(allowed.contains(&"delegate_to_agent".to_string()));
        assert!(
            !allowed.contains(&"write_file".to_string()),
            "PM must not be allowed to write files directly"
        );
    }

    /// The sub-agent config maps the bundled asset body into `system_prompt`.
    #[test]
    fn sub_agent_config_maps_asset_into_config() {
        let cfg = sub_agent_config(ENGINEER_AGENT_NAME, None, ENGINEER_TOOLS)
            .expect("engineer config builds");
        assert_eq!(cfg.agent.name, ENGINEER_AGENT_NAME);
        assert!(cfg.system_prompt.content.contains("Python"));
        assert!(!cfg.system_prompt.content.starts_with("---"));
        let allowed = cfg
            .tools
            .as_ref()
            .and_then(|t| t.allowed.as_ref())
            .expect("engineer has an allowlist");
        for tool in ENGINEER_TOOLS {
            assert!(
                allowed.contains(&tool.to_string()),
                "engineer must be allowed {tool}"
            );
        }
    }

    /// Building a sub-agent config for an unknown agent propagates the error.
    #[test]
    fn sub_agent_config_propagates_unknown_agent() {
        let err = sub_agent_config("no-such-agent-xyz", None, ENGINEER_TOOLS)
            .expect_err("unknown agent must error");
        assert!(matches!(err, MetaInstructionError::UnknownAgent(_)));
    }

    /// The built configs serialise back to TOML and reparse — proving they map
    /// cleanly onto the on-disk `AgentConfig` format the runner loads.
    #[test]
    fn configs_roundtrip_through_toml() {
        let tmp = TempDir::new().unwrap();
        let pm = pm_agent_config(PM_AGENT_NAME, tmp.path(), Some("m"), PM_TOOLS);
        let pm_toml = toml::to_string(&pm).expect("pm serialises");
        let reparsed = AgentConfig::from_toml_str(&pm_toml).expect("pm reparses");
        assert_eq!(reparsed.agent.name, PM_AGENT_NAME);
        assert!(
            reparsed
                .system_prompt
                .content
                .contains("# BASE_PM Framework Floor")
        );
    }
}
