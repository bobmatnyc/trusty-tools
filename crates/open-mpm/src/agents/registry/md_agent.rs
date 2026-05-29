//! Parser for `.md` + YAML-frontmatter agent files (claude-mpm format).
//!
//! Why: Supports the claude-mpm ecosystem convention where agent definitions
//! are authored as Markdown with a YAML frontmatter header, so operators can
//! drop `.md` agents into `.claude/agents/` or `.open-mpm/agents/` without
//! hand-converting to TOML.
//! What: `parse_md_agent` splits frontmatter from body, deserializes the
//! `MdAgentFrontmatter` subset, and assembles a full `AgentConfig` with sane
//! defaults plus the standard model-resolution + adapter selection.
//! Test: `md_agent_file_parses_frontmatter_and_body`,
//! `registry_picks_up_md_files_alongside_toml`.

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use crate::agents::{
    AgentCapabilities, AgentCompressConfig, AgentConfig, AgentInfo, LlmParams, RunnerKind,
    SystemPrompt, ToolChoice, ToolsConfig,
};
use crate::llm::adapter::adapter_for_model;

/// YAML frontmatter shape for `.md` agent files.
///
/// Why: Supports the claude-mpm ecosystem convention where agent definitions
/// are authored as Markdown with a YAML frontmatter header. Lets operators
/// drop `.md` agents into `.claude/agents/` or `.open-mpm/agents/` without
/// hand-converting to TOML.
/// What: Mirrors the subset of `AgentConfig` fields an operator is likely to
/// override per-agent. Unknown keys are ignored (no `deny_unknown_fields`) so
/// richer claude-mpm frontmatter doesn't fail parsing.
/// Test: `md_agent_file_parses_frontmatter_and_body`.
#[derive(Debug, Deserialize, Default)]
struct MdAgentFrontmatter {
    name: Option<String>,
    role: Option<String>,
    model: Option<String>,
    description: Option<String>,
    #[serde(default)]
    runner: Option<String>,
    #[serde(default)]
    capabilities: Option<AgentCapabilities>,
}

/// Parse an `.md` agent file into an `AgentConfig`.
///
/// Why: `AgentRegistry::load` supports both `.toml` and `.md` agent formats so
/// users can install agents using whichever convention suits their workflow.
/// The `.md` + YAML frontmatter format matches the claude-mpm ecosystem.
/// What: Reads the file, splits on `---` fences, parses frontmatter via
/// serde_yml, and uses the post-frontmatter body as the system prompt. Missing
/// fields fall back to sane defaults: `role = "agent"`, generic model, runner
/// = Subprocess. The resolver still applies `OPEN_MPM_MODEL_*` / default env
/// overrides through `resolve_model`.
/// Test: `md_agent_file_parses_frontmatter_and_body`,
/// `registry_picks_up_md_files_alongside_toml`.
pub(super) fn parse_md_agent(path: &Path) -> anyhow::Result<AgentConfig> {
    use anyhow::{Context, anyhow};

    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read agent md {}", path.display()))?;
    let trimmed = raw.trim_start_matches('\u{feff}');
    let trimmed = trimmed.trim_start_matches(['\n', '\r']);

    let rest = trimmed
        .strip_prefix("---")
        .ok_or_else(|| anyhow!("agent md missing opening --- fence: {}", path.display()))?;
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
        .ok_or_else(|| anyhow!("agent md malformed fence: {}", path.display()))?;
    let end_rel = rest
        .find("\n---")
        .ok_or_else(|| anyhow!("agent md missing closing --- fence: {}", path.display()))?;
    let fm_str = &rest[..end_rel];
    let after = &rest[end_rel + 4..];
    let body = after
        .strip_prefix('\n')
        .or_else(|| after.strip_prefix("\r\n"))
        .unwrap_or(after)
        .to_string();

    let fm: MdAgentFrontmatter = serde_yml::from_str(fm_str)
        .with_context(|| format!("failed to parse agent md frontmatter {}", path.display()))?;

    let name = fm.name.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    });
    let agent_model_raw = fm.model.unwrap_or_default();
    let (resolved_model, _src) = crate::agents::resolve_model(&name, &agent_model_raw, None);
    let runner = match fm.runner.as_deref() {
        Some("claude-code") => RunnerKind::ClaudeCode,
        Some("inline") => RunnerKind::Inline,
        Some("in-process") => RunnerKind::InProcess,
        Some("subprocess") | None => RunnerKind::Subprocess,
        Some(other) => {
            return Err(anyhow!(
                "agent md {} has unknown runner {:?}",
                path.display(),
                other
            ));
        }
    };
    let adapter: Arc<dyn crate::llm::adapter::ModelAdapter> =
        Arc::from(adapter_for_model(&resolved_model));

    Ok(AgentConfig {
        agent: AgentInfo {
            name,
            role: fm.role.unwrap_or_else(|| "agent".to_string()),
            model: resolved_model,
            description: fm.description.unwrap_or_default(),
            persistent_session: false,
            runner,
            capabilities: fm.capabilities,
            display_name: None,
            prompt_label: None,
        },
        llm: LlmParams {
            temperature: 0.2,
            max_tokens: 8192,
            model_override: None,
            enable_prompt_caching: true,
            max_turns: 20,
            tool_choice: ToolChoice::Auto,
            use_finish_task: false,
            use_anthropic_direct: false,
            claude_allowed_tools: Vec::new(),
            aws_profile: None,
            aws_region: None,
            elevation_threshold: None,
            elevation_model: None,
            stop_sequences: Vec::new(),
            routing_model: None,
            thinking_enabled: None,
        },
        system_prompt: SystemPrompt {
            content: body,
            skills: None,
        },
        tools: ToolsConfig::default(),
        compress: AgentCompressConfig::default(),
        runner_config: crate::agents::RunnerConfig::default(),
        session: crate::agents::SessionCompressionConfig::default(),
        plugins: crate::agents::AgentPluginsConfig::default(),
        rbac: crate::agents::RbacConfig::default(),
        adapter,
    })
}
