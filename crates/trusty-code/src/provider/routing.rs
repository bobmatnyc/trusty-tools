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

/// Environment variable overriding the `run-task` wall-clock deadline (#2207).
///
/// Why: Lets an operator (or the M3 bake-off runner) raise the deadline for a
/// whole invocation without a CLI flag at every call site — mirrors
/// `crate::mode::MODE_ENV_VAR`'s "escape hatch" precedent for a small, direct
/// `std::env` read in production code.
/// What: Read by [`resolve_deadline_secs`] as the middle precedence tier.
/// Test: `routing::tests::resolve_deadline_secs_*`.
pub const RUN_DEADLINE_ENV_VAR: &str = "TCODE_RUN_DEADLINE_SECONDS";

/// Generous default wall-clock deadline for a `run-task` invocation, in
/// seconds, when neither a CLI override nor [`RUN_DEADLINE_ENV_VAR`] is set.
///
/// Why: The AgentLoop's own built-in default (120s, `crate::agent_loop::AgentLoopConfig::default`)
/// is tuned for a short interactive turn, not a full PM->engineer run-task
/// invocation — #2207 found that cap cutting off a real, otherwise-successful
/// task at turn 13 before it reached `finish_task`. 30 minutes is generous
/// enough for the M3 bake-off's L1 pilot to complete cleanly while L2/L3 (1-3
/// hour problems) can still raise it further via the flag/env override.
/// What: The fallback tier of [`resolve_deadline_secs`].
/// Test: `routing::tests::resolve_deadline_secs_falls_back_to_default`.
pub const DEFAULT_RUN_DEADLINE_SECS: u64 = 1800;

/// Resolve the wall-clock deadline (in seconds) for a `run-task` invocation.
///
/// Why: #2207 — the AgentLoop's built-in 120s timeout is too tight for a real
/// PM->engineer run-task on anything but a trivial task, cutting off
/// otherwise-successful runs before they reach `finish_task`. A single
/// resolver mirrors [`resolve_model`]/[`resolve_max_tokens`]'s precedence-
/// ladder pattern so every call site (the CLI's legacy in-process path, the
/// daemon's `task.run`, and the in-process sub-agent runner) agrees.
/// What: Returns, in priority order, the first of: (1) `cli_override` (the
/// `run-task --timeout-seconds`/`task.run` `deadline_secs` request param,
/// threaded in by the caller), (2) [`RUN_DEADLINE_ENV_VAR`] parsed as a `u64`
/// (an unparseable value is treated as absent, falling through — logged at
/// `warn` — rather than erroring the whole run), else (3)
/// [`DEFAULT_RUN_DEADLINE_SECS`].
/// Test: `routing::tests::resolve_deadline_secs_cli_override_wins`,
/// `routing::tests::resolve_deadline_secs_env_wins_over_default`,
/// `routing::tests::resolve_deadline_secs_invalid_env_falls_back_to_default`,
/// `routing::tests::resolve_deadline_secs_falls_back_to_default`.
pub fn resolve_deadline_secs(cli_override: Option<u64>) -> u64 {
    if let Some(secs) = cli_override {
        return secs;
    }

    if let Ok(raw) = std::env::var(RUN_DEADLINE_ENV_VAR) {
        match raw.trim().parse::<u64>() {
            Ok(secs) => return secs,
            Err(e) => {
                tracing::warn!(
                    "{RUN_DEADLINE_ENV_VAR}={raw:?} is not a valid u64 ({e}); \
                     falling back to DEFAULT_RUN_DEADLINE_SECS"
                );
            }
        }
    }

    DEFAULT_RUN_DEADLINE_SECS
}

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

/// Serializes every test in this module that sets/reads the process-wide
/// [`RUN_DEADLINE_ENV_VAR`] — `cargo test` runs tests in parallel within one
/// binary, and an unguarded `set_var`/`remove_var` pair would race across
/// tests (mirrors `crate::mode::MODE_ENV_LOCK`'s identical rationale).
#[cfg(test)]
pub(crate) static RUN_DEADLINE_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

    // ── #2207: resolve_deadline_secs precedence ─────────────────────────────

    /// Serializes access to [`RUN_DEADLINE_ENV_VAR`] for one test closure,
    /// restoring the prior (absent) state afterward.
    async fn with_env_deadline<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = RUN_DEADLINE_ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by `RUN_DEADLINE_ENV_LOCK`.
        unsafe {
            match value {
                Some(v) => std::env::set_var(RUN_DEADLINE_ENV_VAR, v),
                None => std::env::remove_var(RUN_DEADLINE_ENV_VAR),
            }
        }
        let result = f();
        unsafe {
            std::env::remove_var(RUN_DEADLINE_ENV_VAR);
        }
        result
    }

    /// The explicit CLI override wins over both the env var and the default.
    ///
    /// Why: `run-task --timeout-seconds` (or `task.run`'s `deadline_secs`
    /// param) must be the highest-precedence source — an operator setting it
    /// explicitly for one run must never be silently overridden.
    /// What: Set the env var to a different value; assert the override wins.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_deadline_secs_cli_override_wins() {
        with_env_deadline(Some("60"), || {
            assert_eq!(resolve_deadline_secs(Some(7200)), 7200);
        })
        .await;
    }

    /// The env var wins over the built-in default when no override is given.
    ///
    /// Why: `TCODE_RUN_DEADLINE_SECONDS` is the escape-hatch tier for a whole
    /// invocation without threading a flag through every call site.
    /// What: No CLI override; env var set; assert the env value is returned.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_deadline_secs_env_wins_over_default() {
        with_env_deadline(Some("900"), || {
            assert_eq!(resolve_deadline_secs(None), 900);
        })
        .await;
    }

    /// An unparseable env var falls through to the default rather than
    /// erroring the whole run.
    ///
    /// Why: A misconfigured environment must degrade gracefully, not crash a
    /// long-running task.
    /// What: Env var set to a non-numeric string; assert the default wins.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_deadline_secs_invalid_env_falls_back_to_default() {
        with_env_deadline(Some("not-a-number"), || {
            assert_eq!(resolve_deadline_secs(None), DEFAULT_RUN_DEADLINE_SECS);
        })
        .await;
    }

    /// Falls back to [`DEFAULT_RUN_DEADLINE_SECS`] when nothing is set.
    ///
    /// Why: A run must always resolve to a usable, generous deadline even
    /// when no override is configured anywhere.
    /// What: No CLI override, no env var; assert the default.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_deadline_secs_falls_back_to_default() {
        with_env_deadline(None, || {
            assert_eq!(resolve_deadline_secs(None), DEFAULT_RUN_DEADLINE_SECS);
        })
        .await;
    }
}
