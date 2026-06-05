//! Agent configuration schema for tcode.
//!
//! Why: Sub-agents (and the PM itself) are defined declaratively in TOML so
//! model, prompt, and LLM parameters can evolve without code changes.
//! What: `AgentConfig` and all nested config types, deserializable from the
//! `.claude/agents/<name>.toml` format compatible with Claude Code sub-agents.
//! Test: `AgentConfig::from_toml_str` on inline TOML succeeds and round-trips.

use serde::{Deserialize, Serialize};

/// Top-level agent configuration loaded from `<name>.toml`.
///
/// Why: Declarative agent configs let operators define or override agents
/// without touching Rust code.
/// What: `agent` carries identity/role; `llm` carries model/token params;
/// `system_prompt` carries the prompt text; optional sections add capabilities.
/// Test: `agent_config_roundtrip`, `agent_config_minimal`.
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

impl AgentConfig {
    /// Parse an `AgentConfig` from a TOML string.
    ///
    /// Why: Useful in tests and for in-process loading of bundled config.
    /// What: Delegates to `toml::from_str`.
    /// Test: `agent_config_roundtrip`.
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Load an `AgentConfig` from a TOML file on disk.
    ///
    /// Why: Primary entry point for the agent loader during harness startup.
    /// What: Reads the file, then calls `from_toml_str`.
    /// Test: Integration tests place a TOML file in a tempdir and call this.
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let src = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read agent config {}: {e}", path.display()))?;
        toml::from_str(&src)
            .map_err(|e| anyhow::anyhow!("invalid agent config {}: {e}", path.display()))
    }
}

/// Core identity fields for an agent.
///
/// Why: Every agent needs a stable `name` (the dispatch key) and `model`.
/// What: `name`, optional `role`, optional `model`.
/// Test: `agent_config_minimal` asserts that `name` is loaded.
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
/// Why: Each agent may need different temperature, token budget, or provider.
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
    /// Route directly to Anthropic API instead of OpenRouter.
    #[serde(default)]
    pub use_anthropic_direct: bool,
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
/// swap backends without code changes.
/// What: `SubProcess` is the default (spawns a new process via NDJSON IPC);
/// `ClaudeCode` wraps the `claude` CLI binary.
/// Test: `runner_kind_deserializes`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerKind {
    /// Spawn a subprocess; communicate via NDJSON over stdin/stdout.
    #[default]
    SubProcess,
    /// Use the `claude` CLI binary as the runner.
    ClaudeCode,
    /// In-process mock runner (tests only).
    InProcess,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_TOML: &str = r#"
[agent]
name = "engineer"
"#;

    const FULL_TOML: &str = r#"
[agent]
name = "python-engineer"
role = "engineer"
model = "anthropic/claude-sonnet-4-6"
description = "Python software engineer"

[llm]
temperature = 0.2
max_tokens = 8192

[system_prompt]
content = "You are a Python expert."

[tools]
allowed = ["delegate_to_agent", "search_code"]

[runner]
kind = "claude_code"
"#;

    /// Minimal config with only `[agent].name` parses successfully.
    ///
    /// Why: Operators should be able to define a minimal agent without
    /// specifying every optional field.
    /// What: `from_toml_str(MINIMAL_TOML)` returns `Ok` with `name == "engineer"`.
    /// Test: This test.
    #[test]
    fn agent_config_minimal() {
        let cfg = AgentConfig::from_toml_str(MINIMAL_TOML).expect("parse minimal");
        assert_eq!(cfg.agent.name, "engineer");
        assert!(cfg.agent.model.is_none());
        assert!(cfg.tools.is_none());
    }

    /// Full config round-trips through TOML parsing.
    ///
    /// Why: Verify all fields are decoded correctly.
    /// What: `from_toml_str(FULL_TOML)` round-trips all fields.
    /// Test: This test.
    #[test]
    fn agent_config_roundtrip() {
        let cfg = AgentConfig::from_toml_str(FULL_TOML).expect("parse full");
        assert_eq!(cfg.agent.name, "python-engineer");
        assert_eq!(
            cfg.agent.model.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert_eq!(cfg.llm.temperature, Some(0.2));
        assert_eq!(cfg.llm.max_tokens, Some(8192));
        assert_eq!(cfg.system_prompt.content, "You are a Python expert.");
        let allowed = cfg.tools.as_ref().and_then(|t| t.allowed.as_ref());
        let expected: Vec<String> =
            vec!["delegate_to_agent".to_string(), "search_code".to_string()];
        assert_eq!(allowed.map(|v| v.as_slice()), Some(expected.as_slice()));
        assert_eq!(
            cfg.runner.as_ref().map(|r| &r.kind),
            Some(&RunnerKind::ClaudeCode)
        );
    }

    /// `RunnerKind` deserializes from `snake_case` strings via a TOML wrapper table.
    ///
    /// Why: Verify the serde rename_all contract.
    /// What: Parse a `[runner]` table with `kind = "claude_code"` → `RunnerKind::ClaudeCode`.
    /// Test: This test.
    #[test]
    fn runner_kind_deserializes() {
        #[derive(serde::Deserialize)]
        struct Wrapper {
            kind: RunnerKind,
        }
        let w: Wrapper = toml::from_str("kind = \"claude_code\"").expect("parse runner kind");
        assert_eq!(w.kind, RunnerKind::ClaudeCode);
    }
}
