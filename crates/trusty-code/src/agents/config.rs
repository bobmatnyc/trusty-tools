//! Agent configuration schema for tcode.
//!
//! Why: Sub-agents (and the PM itself) are defined declaratively so model,
//! prompt, and LLM parameters can evolve without code changes. As of #2897
//! Slice D, the on-disk source format is Markdown+frontmatter
//! (`.claude/agents/<name>.md`) ONLY — the original TOML loader
//! (`AgentConfig::from_toml_str`/`AgentConfig::load`) has been retired; see
//! `agents::md_loader` for the current loader and
//! `scripts/migrate-tcode-agents-toml-to-md.py` for the one-time converter a
//! project with pre-#2897 `.toml` agent files should run.
//! What: `AgentConfig` and all nested config types. These are pure data
//! structs — format-agnostic — that `agents::md_loader::project_to_agent_config`
//! populates from a composed `.md` document's frontmatter + body.
//! Test: `agent_config_default_is_empty`, `runner_kind_defaults_to_in_process`;
//! the loading path itself is covered by `agents::md_loader::tests::*`.

use serde::{Deserialize, Serialize};

/// Top-level agent configuration.
///
/// Why: Declarative agent configs let operators define or override agents
/// without touching Rust code.
/// What: `agent` carries identity/role; `llm` carries model/token params;
/// `system_prompt` carries the prompt text; optional sections add capabilities.
/// Populated by `agents::md_loader::project_to_agent_config` from a composed
/// `.md` agent document.
/// Test: `agent_config_default_is_empty`; `agents::md_loader::tests::*` cover
/// the actual field-projection contract end-to-end.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentConfig {
    /// Core identity fields.
    pub agent: AgentInfo,
    /// LLM parameters for this agent.
    #[serde(default)]
    pub llm: LlmParams,
    /// System prompt content.
    #[serde(default)]
    pub system_prompt: SystemPrompt,
    /// Optional tool permissions.
    #[serde(default)]
    pub tools: Option<ToolsConfig>,
    /// Optional runner override.
    #[serde(default)]
    pub runner: Option<RunnerConfig>,
}

/// Core identity fields for an agent.
///
/// Why: Every agent needs a stable `name` (the dispatch key) and `model`.
/// What: `name`, optional `role`, optional `model`.
/// Test: `agent_config_default_is_empty` asserts the zero-value default;
/// `agents::md_loader::tests::load_md_agent_base_case` covers real loading.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentInfo {
    /// The agent's dispatch key (e.g. `"engineer"`, `"python-engineer"`).
    pub name: String,
    /// Optional free-form role description.
    #[serde(default)]
    pub role: Option<String>,
    /// LLM model override (e.g. `"anthropic/claude-sonnet-4-6"`,
    /// `"bedrock/us.anthropic.claude-sonnet-4-6"`).
    #[serde(default)]
    pub model: Option<String>,
    /// Human-readable description of what this agent does.
    #[serde(default)]
    pub description: Option<String>,
}

/// LLM parameters for a single agent.
///
/// Why: Each agent may need different temperature, token budget, or model. The
/// backend (OpenRouter vs. Bedrock) is no longer a boolean here — it is derived
/// from the resolved model slug by `provider::provider_for` (#1021), so a single
/// slug like `bedrock/...` selects the backend without a separate flag.
/// What: Standard LLM knobs, all optional with sensible defaults.
/// Test: `agent_config_llm_params`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmParams {
    /// Sampling temperature (0.0–1.0).
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Maximum tokens in the LLM response.
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Model override at the `[llm]` level (lower precedence than `[agent].model`).
    #[serde(default)]
    pub model_override: Option<String>,
}

/// System prompt for an agent.
///
/// Why: Keeping the prompt in config (not source code) lets operators tune
/// agent behavior without a Rust recompile.
/// What: `content` is the raw prompt text; `append_skills` is a list of skill
/// names to inject at startup.
/// Test: `agent_config_system_prompt`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemPrompt {
    /// Raw prompt text.
    #[serde(default)]
    pub content: String,
    /// Skill names to inject into the prompt.
    #[serde(default)]
    pub append_skills: Vec<String>,
}

/// Per-agent tool permissions.
///
/// Why: Restricts which tools an agent may call, preventing e.g. the
/// plan-agent from shelling out.
/// What: `allowed` is an explicit allowlist; `None` means "all registered tools".
/// Test: `ToolRegistry::dispatch_gated` tests exercise the allowlist.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsConfig {
    /// Explicit allowlist of tool names. `None` = all tools permitted.
    #[serde(default)]
    pub allowed: Option<Vec<String>>,
}

/// Runner backend selection for an agent.
///
/// Why: Different agents may use different execution backends (subprocess,
/// in-process, Claude Code CLI).
/// What: `kind` selects the backend; defaults to `SubProcess`.
/// Test: `agent_config_runner_kind`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunnerConfig {
    /// Which backend to use for this agent.
    #[serde(default)]
    pub kind: RunnerKind,
}

/// Execution backend for agent invocations.
///
/// Why: Abstracts over the concrete runner selected at startup so config can
/// swap backends without code changes. As of #1029 the in-process runner is the
/// real, production default — the M1 model-comparison harness drives a delegated
/// sub-agent inside the PM's own process (cheap, observable, usage rolls up onto
/// one transcript). The subprocess and Claude-Code CLI backends are retained as
/// future variants for deployments that need process isolation.
/// What: `InProcess` is the default (drives the sub-agent's `AgentLoop` in the
/// same process — see `crate::runner::InProcessAgentRunner`); `SubProcess` spawns
/// a new process via NDJSON IPC; `ClaudeCode` wraps the `claude` CLI binary.
/// Test: `runner_kind_defaults_to_in_process`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerKind {
    /// Drive the sub-agent's `AgentLoop` in the PM's own process (the default).
    #[default]
    InProcess,
    /// Spawn a subprocess; communicate via NDJSON over stdin/stdout.
    SubProcess,
    /// Use the `claude` CLI binary as the runner.
    ClaudeCode,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `AgentConfig::default()` yields the expected all-empty baseline.
    ///
    /// Why: #2897 Slice D retired the TOML parsing tests (`from_toml_str`
    /// itself is gone); the actual frontmatter -> `AgentConfig` field mapping
    /// is now covered end-to-end by `agents::md_loader::tests::*`
    /// (`load_md_agent_base_case`, `parallel fixture` field assertions,
    /// etc.). What remains testable in THIS module is the plain-data
    /// `Default` contract every construction path relies on.
    /// What: every field is the type's zero value; `tools`/`runner` are
    /// `None`.
    /// Test: this test.
    #[test]
    fn agent_config_default_is_empty() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.agent.name, "");
        assert!(cfg.agent.model.is_none());
        assert!(cfg.tools.is_none());
        assert!(cfg.runner.is_none());
        assert_eq!(cfg.system_prompt.content, "");
    }

    /// `RunnerKind` defaults to `InProcess` (the real default since #1029).
    ///
    /// Why: The acceptance criterion for #1029 makes the in-process runner the
    /// production default; a regression that flipped it back to `SubProcess`
    /// would silently route every undeclared agent through the wrong backend.
    /// What: `RunnerKind::default()` equals `InProcess`; a default-constructed
    /// `AgentConfig` (mirroring an agent with no `[runner]`/no runner
    /// frontmatter) leaves `runner` `None` (callers treat absence as default).
    /// Test: this test.
    #[test]
    fn runner_kind_defaults_to_in_process() {
        assert_eq!(RunnerKind::default(), RunnerKind::InProcess);
        let cfg = AgentConfig::default();
        assert!(
            cfg.runner.is_none(),
            "absent runner config leaves runner None; callers apply the InProcess default"
        );
    }
}
