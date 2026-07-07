//! Per-agent model routing precedence.
//!
//! Why: A model can be specified in three places — the per-call [`RunContext`],
//! the agent's TOML config (`[agent].model` or `[llm].model_override`), and a
//! built-in default. The orchestrator needs one authoritative function that
//! resolves these to a single slug so every call site agrees on precedence
//! (#1021).
//! What: [`resolve_model`] implements the precedence ladder and
//! [`DEFAULT_MODEL`] names the fallback slug.
//! Test: `routing::tests::*`.

use crate::agents::AgentConfig;
use crate::tools::RunContext;

/// Default model slug when nothing else is specified.
///
/// Why: A run must always resolve to *some* model; this is the cheap, broadly
/// available fallback used across trusty-code.
/// What: The OpenRouter slug for GPT-4o-mini.
/// Test: `routing::tests::resolve_model_falls_back_to_default`.
pub const DEFAULT_MODEL: &str = "openai/gpt-4o-mini";

/// Default per-turn completion token cap when `[llm].max_tokens` is unset.
///
/// Why: A turn cap must always resolve to *some* value, and it must be large
/// enough for a real tool-calling turn (e.g. `write_file` emitting a whole
/// source file) to complete without truncation — the bug this constant fixes
/// was every turn silently capped at 1024 regardless of agent config,
/// truncating real writes. 8192 is generous enough for typical file writes
/// while still bounding a single turn's cost.
/// What: The fallback completion-token cap used by [`resolve_max_tokens`].
/// Test: `routing::tests::resolve_max_tokens_falls_back_to_default`.
pub const DEFAULT_MAX_TOKENS: u32 = 8192;

/// Resolve the model slug for an invocation.
///
/// Why: Centralises the precedence so per-call overrides, per-agent config, and
/// the global default never disagree across call sites.
/// What: Returns, in priority order, the first non-empty of:
/// (1) `run_context.model` (per-call override),
/// (2) `agent_config.agent.model`,
/// (3) `agent_config.llm.model_override`,
/// else (4) [`DEFAULT_MODEL`].
/// Test: `routing::tests::resolve_model_*` cover every tier.
pub fn resolve_model(agent_config: &AgentConfig, run_context: Option<&RunContext>) -> String {
    // 1. Per-call override wins.
    if let Some(ctx) = run_context
        && let Some(model) = non_empty(ctx.model.as_deref())
    {
        return model.to_string();
    }

    // 2. Agent-level model.
    if let Some(model) = non_empty(agent_config.agent.model.as_deref()) {
        return model.to_string();
    }

    // 3. `[llm].model_override` (lower precedence than `[agent].model`).
    if let Some(model) = non_empty(agent_config.llm.model_override.as_deref()) {
        return model.to_string();
    }

    // 4. Built-in default.
    DEFAULT_MODEL.to_string()
}

/// Resolve the per-turn completion-token cap for an invocation.
///
/// Why: `[llm].max_tokens` is how an operator declares that an agent's turns
/// need a larger completion budget than the default (e.g. an engineer that
/// writes whole files needs far more than a chat-only PM). Before this fix,
/// every call site building an `AgentLoopConfig` ignored the agent's
/// `[llm].max_tokens` entirely and the loop hard-coded a 1024-token cap,
/// truncating any turn that needed to emit more (#run_task-maxtokens-bug).
/// What: Returns `agent_config.llm.max_tokens` when set, else
/// [`DEFAULT_MAX_TOKENS`]. There is no per-call `RunContext` override yet —
/// the config-file value or the built-in default are the only two tiers.
/// Test: `routing::tests::resolve_max_tokens_*`.
pub fn resolve_max_tokens(agent_config: &AgentConfig) -> u32 {
    agent_config.llm.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)
}

/// Treat empty/whitespace-only strings as absent.
///
/// Why: An empty `model = ""` in TOML or an empty `RunContext.model` should fall
/// through to the next tier rather than route to a blank slug.
/// What: Returns `Some(trimmed-original)` only when the value has non-whitespace
/// content; `None` otherwise.
/// Test: `routing::tests::resolve_model_skips_empty_strings`.
fn non_empty(value: Option<&str>) -> Option<&str> {
    value.and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::config::{AgentInfo, LlmParams};

    /// Build an `AgentConfig` with the given agent model and `model_override`.
    fn cfg(agent_model: Option<&str>, override_model: Option<&str>) -> AgentConfig {
        AgentConfig {
            agent: AgentInfo {
                name: "test-agent".into(),
                model: agent_model.map(str::to_string),
                ..Default::default()
            },
            llm: LlmParams {
                model_override: override_model.map(str::to_string),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// `RunContext.model` wins over agent config and default.
    ///
    /// Why: Per-call overrides must take precedence so a caller can pin a model
    /// for a single invocation.
    /// What: Provide all three sources; assert the RunContext slug is returned.
    /// Test: this test.
    #[test]
    fn resolve_model_run_context_wins() {
        let config = cfg(Some("anthropic/claude-sonnet-4-5"), Some("qwen/qwen-2.5"));
        let ctx = RunContext {
            model: Some("openai/gpt-4o".into()),
            ..Default::default()
        };
        assert_eq!(resolve_model(&config, Some(&ctx)), "openai/gpt-4o");
    }

    /// `[agent].model` wins when no RunContext override is present.
    ///
    /// Why: The agent's declared model is the next authority below per-call.
    /// What: No RunContext model; assert the agent slug is returned over the
    /// `model_override` and the default.
    /// Test: this test.
    #[test]
    fn resolve_model_agent_config_wins_over_default() {
        let config = cfg(Some("anthropic/claude-haiku-4-5"), Some("qwen/qwen-2.5"));
        assert_eq!(
            resolve_model(&config, None),
            "anthropic/claude-haiku-4-5",
            "agent.model must win over model_override and default"
        );
    }

    /// `[llm].model_override` is used when `[agent].model` is absent.
    ///
    /// Why: `model_override` is the documented lower-precedence config slot.
    /// What: Agent model `None`, override set; assert override is returned.
    /// Test: this test.
    #[test]
    fn resolve_model_uses_model_override_when_agent_absent() {
        let config = cfg(None, Some("deepseek/deepseek-chat"));
        assert_eq!(resolve_model(&config, None), "deepseek/deepseek-chat");
    }

    /// Falls back to `DEFAULT_MODEL` when nothing is specified.
    ///
    /// Why: A run must always resolve to a usable slug.
    /// What: Empty config and no RunContext; assert the default.
    /// Test: this test.
    #[test]
    fn resolve_model_falls_back_to_default() {
        let config = cfg(None, None);
        assert_eq!(resolve_model(&config, None), DEFAULT_MODEL);
    }

    /// Empty / whitespace strings are treated as absent at each tier.
    ///
    /// Why: A blank `model = ""` should not route to an empty slug; it must fall
    /// through to the next source.
    /// What: Empty RunContext + empty agent model + valid override → override.
    /// Test: this test.
    #[test]
    fn resolve_model_skips_empty_strings() {
        let config = cfg(Some("   "), Some("google/gemma-2-27b-it"));
        let ctx = RunContext {
            model: Some("".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_model(&config, Some(&ctx)),
            "google/gemma-2-27b-it",
            "empty RunContext and empty agent model must fall through"
        );
    }

    /// Whitespace-padded slugs are trimmed at every precedence tier.
    ///
    /// Why: A TOML or RunContext slug like `"  openai/gpt-4o  "` would otherwise
    /// route to an invalid, whitespace-padded model id. `non_empty` must return
    /// the trimmed value, not the original padded slice.
    /// What: (a) padded `RunContext.model` resolves to the trimmed slug; (b) with
    /// no RunContext, a padded `agent.model` resolves to its trimmed slug; (c) an
    /// all-whitespace `agent.model` falls through to the next tier (`model_override`).
    /// Test: this test.
    #[test]
    fn resolve_model_trims_padded_slugs() {
        // (a) Padded per-call override is trimmed and wins.
        let config = cfg(Some("anthropic/claude-sonnet-4-5"), None);
        let ctx = RunContext {
            model: Some("  openai/gpt-4o  ".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_model(&config, Some(&ctx)),
            "openai/gpt-4o",
            "padded RunContext.model must resolve to the trimmed slug"
        );

        // (b) Padded agent.model is trimmed when no RunContext override exists.
        let config = cfg(Some("  anthropic/claude-haiku-4-5  "), None);
        assert_eq!(
            resolve_model(&config, None),
            "anthropic/claude-haiku-4-5",
            "padded agent.model must resolve to the trimmed slug"
        );

        // (c) All-whitespace agent.model falls through to model_override.
        let config = cfg(Some("   "), Some("deepseek/deepseek-chat"));
        assert_eq!(
            resolve_model(&config, None),
            "deepseek/deepseek-chat",
            "all-whitespace agent.model must fall through to the next tier"
        );
    }

    /// A configured `[llm].max_tokens` is honoured, not clamped to the default.
    ///
    /// Why: This is the regression guard for the bug this function fixes — a
    /// configured cap higher than the old hard-coded 1024 must reach the
    /// caller unchanged.
    /// What: `[llm].max_tokens = 8192` resolves to `8192`.
    /// Test: this test.
    #[test]
    fn resolve_max_tokens_honours_configured_value() {
        let config = AgentConfig {
            llm: LlmParams {
                max_tokens: Some(8192),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(resolve_max_tokens(&config), 8192);
    }

    /// Falls back to `DEFAULT_MAX_TOKENS` when `[llm].max_tokens` is unset.
    ///
    /// Why: A run must always resolve to a usable, generous-enough cap even
    /// when the agent TOML doesn't specify one.
    /// What: Empty config resolves to `DEFAULT_MAX_TOKENS`.
    /// Test: this test.
    #[test]
    fn resolve_max_tokens_falls_back_to_default() {
        let config = AgentConfig::default();
        assert_eq!(resolve_max_tokens(&config), DEFAULT_MAX_TOKENS);
    }
}
