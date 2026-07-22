//! Parser for `.md` + YAML-frontmatter agent files (claude-mpm format).
//!
//! Why: Supports the claude-mpm ecosystem convention where agent definitions
//! are authored as Markdown with a YAML frontmatter header, so operators can
//! drop `.md` agents into `.claude/agents/` or `.trusty-agents/agents/` without
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
/// drop `.md` agents into `.claude/agents/` or `.trusty-agents/agents/` without
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
    /// Persona/display name layered by a personalization overlay (DOC-41
    /// §2.5.1, #3055).
    ///
    /// Why: base agents are nameless; the friendly persona name (e.g.
    /// `Izzie`) comes from the child overlay that `extends` the base. Parsing
    /// it here lets a `.md` child set `display_name` even when its base has
    /// none — scalar child-overrides semantics.
    /// What: Optional string mapped straight onto `AgentInfo::display_name`.
    /// Test: `agents::extends::tests::extends_child_sets_display_name_over_none`.
    #[serde(default)]
    display_name: Option<String>,
    /// Per-agent tool allowlist / scopes layered by a personalization overlay
    /// (DOC-41 §2.5.1 worked example, #3055).
    ///
    /// Why: the §2.5.1 personalization example authors a `.md` agent whose
    /// frontmatter declares `tools:\n  allowed: [...]` — before this the `.md`
    /// loader hardcoded `ToolsConfig::default()` and silently dropped any
    /// declared tools, so the `extends` tool UNION had nothing to union. Parsing
    /// it here makes the personalization example actually work.
    /// What: Optional `ToolsConfig` (same shape as the TOML `[tools]` table);
    /// absent → `ToolsConfig::default()` (no restriction), exactly as before.
    /// Test: `agents::extends::tests::registry_resolves_extends_chain`.
    #[serde(default)]
    tools: Option<ToolsConfig>,
    /// Name of a base agent to inherit from (`extends:`), DOC-41 §2.5 / #3055.
    ///
    /// Why: makes the previously-inert `extends:` frontmatter key live so a
    /// user can personalize a bundled base agent without forking it.
    /// What: Optional base-agent name; resolved + merged by the loader.
    /// Test: `agents::extends::tests`.
    #[serde(default)]
    extends: Option<String>,
}

/// Parse an `.md` agent file into an `AgentConfig`.
///
/// Why: `AgentRegistry::load` supports both `.toml` and `.md` agent formats so
/// users can install agents using whichever convention suits their workflow.
/// The `.md` + YAML frontmatter format matches the claude-mpm ecosystem.
/// What: Reads the file, splits on `---` fences, parses frontmatter via
/// serde_yaml, and uses the post-frontmatter body as the system prompt. Missing
/// fields fall back to sane defaults: `role = "agent"`, generic model, runner
/// = Subprocess. The resolver still applies `TAGENT_MODEL_*` / default env
/// overrides through `resolve_model`.
/// Test: `md_agent_file_parses_frontmatter_and_body`,
/// `registry_picks_up_md_files_alongside_toml`.
pub(crate) fn parse_md_agent(path: &Path) -> anyhow::Result<AgentConfig> {
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

    let fm: MdAgentFrontmatter = serde_yaml::from_str(fm_str)
        .with_context(|| format!("failed to parse agent md frontmatter {}", path.display()))?;

    let name = fm.name.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    });
    let agent_model_raw = fm.model.unwrap_or_default();
    // #3055: when this agent `extends` a base, defer model resolution until
    // after the `extends` merge. A child that omits `model` must INHERIT the
    // base's, so keep the raw (possibly empty) value here rather than resolving
    // it to a default now; `AgentRegistry::load` re-resolves the merged config.
    let resolved_model = if fm.extends.is_some() {
        agent_model_raw
    } else {
        crate::agents::resolve_model(&name, &agent_model_raw, None).0
    };
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

    // #3052 PR A follow-up: `.md` frontmatter doesn't expose `temperature`/
    // `max_tokens` as parseable keys (`MdAgentFrontmatter` has no such
    // fields), so a `.md` file can never explicitly declare them. When this
    // agent `extends` a base, use the UNSET sentinel so `merge_extends`
    // inherits the base's real tuned values instead of clobbering them with
    // a hardcoded stub (the pre-#3052 wholesale-inherit behavior, preserved
    // here for `.md` children specifically). A non-`extends` `.md` agent has
    // no base to inherit from, so it keeps the historical 0.2/8192 defaults.
    let (llm_temperature, llm_max_tokens) = if fm.extends.is_some() {
        (
            crate::agents::params::unset_temperature(),
            crate::agents::params::unset_max_tokens(),
        )
    } else {
        (0.2, 8192)
    };

    Ok(AgentConfig {
        agent: AgentInfo {
            name,
            role: fm.role.unwrap_or_else(|| "agent".to_string()),
            model: resolved_model,
            description: fm.description.unwrap_or_default(),
            persistent_session: false,
            runner,
            capabilities: fm.capabilities,
            display_name: fm.display_name,
            prompt_label: None,
            extends: fm.extends,
        },
        llm: LlmParams {
            temperature: llm_temperature,
            max_tokens: llm_max_tokens,
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
        tools: fm.tools.unwrap_or_default(),
        compress: AgentCompressConfig::default(),
        runner_config: crate::agents::RunnerConfig::default(),
        session: crate::agents::SessionCompressionConfig::default(),
        plugins: crate::agents::AgentPluginsConfig::default(),
        rbac: crate::agents::RbacConfig::default(),
        adapter,
    })
}
