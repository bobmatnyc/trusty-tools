//! Credential routing for the controller's own LLM calls (#250).
//!
//! Why: The PM/ctrl orchestrator was hard-requiring `OPENROUTER_API_KEY` even
//! when the user has equally-valid alternative credentials configured
//! (`ANTHROPIC_API_KEY` from console.anthropic.com, or `CLAUDE_CODE_OAUTH_TOKEN`
//! from `claude setup-token`). This module centralizes the priority decision so
//! both the client constructor and the chat dispatcher agree on which backend
//! is active.
//!
//! What: `pick_credentials()` inspects the env in priority order
//! (CLAUDE_CODE_OAUTH_TOKEN > ANTHROPIC_API_KEY > OPENROUTER_API_KEY) and
//! returns a `LlmCredentials` enum describing the routing mode plus a friendly
//! display label for startup logging.
//!
//! Test: `pick_picks_claude_code_when_oauth_set`,
//! `pick_picks_anthropic_when_only_anthropic_set`,
//! `pick_picks_openrouter_when_only_openrouter_set`,
//! `pick_returns_none_when_nothing_set`.

/// Resolved LLM backend for the ctrl/PM in-process LLM calls.
///
/// Why: Three discrete routing paths require three different downstream
/// behaviors: the OpenRouter HTTP client, the direct Anthropic API, or the
/// `claude` CLI subprocess. Encoding the choice as an enum keeps the wiring
/// honest across `ctrl::run_pm_task_with_history`.
/// What: One variant per supported credential source.
/// Test: See module-level tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmCredentials {
    /// `CLAUDE_CODE_OAUTH_TOKEN` is set — route via the local `claude` CLI
    /// subprocess. No OPENROUTER_API_KEY or ANTHROPIC_API_KEY needed.
    ClaudeCode,
    /// `ANTHROPIC_API_KEY` is set — call api.anthropic.com directly via the
    /// adapter's native path (force `use_anthropic_direct = true`).
    AnthropicDirect,
    /// `OPENROUTER_API_KEY` is set — the original OpenRouter routing path.
    OpenRouter,
}

impl LlmCredentials {
    /// Short label suitable for startup banners ("LLM: claude-code").
    pub fn label(&self) -> &'static str {
        match self {
            LlmCredentials::ClaudeCode => "claude-code",
            LlmCredentials::AnthropicDirect => "anthropic-direct",
            LlmCredentials::OpenRouter => "openrouter",
        }
    }
}

/// Pick the active credential source based on environment variables, gated
/// by the agent's `runner` field.
///
/// Why: `CLAUDE_CODE_OAUTH_TOKEN` (sk-ant-oat01-*) is ONLY valid for agents
/// that explicitly declare `runner = "claude-code"` in their TOML — it routes
/// through the `claude` CLI subprocess (`ClaudeCodeAgentRunner`), not the
/// Anthropic REST API (which 401s OAuth tokens). Auto-selecting ClaudeCode
/// whenever the env var happens to be set was wrong: it forced ALL agents
/// (including PM/ctrl whose TOMLs do NOT request the claude-code runner)
/// down the slow CLI path. The runner gate makes this explicit: claude-code
/// routing requires both the env var AND the agent's opt-in.
///
/// `ANTHROPIC_API_KEY` is preferred over OpenRouter when both are set
/// (lower-latency direct API). `OPENROUTER_API_KEY` is the deployment / CI
/// fallback.
/// What: Reads three env vars. Returns `ClaudeCode` only when
/// `runner == Some(RunnerKind::ClaudeCode)` AND `CLAUDE_CODE_OAUTH_TOKEN`
/// is set. Otherwise prefers `AnthropicDirect` then `OpenRouter`. Returns
/// `None` when nothing applicable is configured.
/// Test: See module-level tests, including
/// `pick_skips_claude_code_when_runner_not_claude_code`.
pub fn pick_credentials(runner: Option<crate::agents::RunnerKind>) -> Option<LlmCredentials> {
    let openrouter = env_set("OPENROUTER_API_KEY");
    let anthropic = env_set("ANTHROPIC_API_KEY");
    let claude_code = env_set("CLAUDE_CODE_OAUTH_TOKEN");
    let runner_is_claude_code = matches!(runner, Some(crate::agents::RunnerKind::ClaudeCode));
    tracing::debug!(
        openrouter_set = openrouter,
        anthropic_set = anthropic,
        claude_code_set = claude_code,
        runner_is_claude_code,
        "pick_credentials: env probe"
    );
    if claude_code && runner_is_claude_code {
        Some(LlmCredentials::ClaudeCode)
    } else if anthropic {
        Some(LlmCredentials::AnthropicDirect)
    } else if openrouter {
        Some(LlmCredentials::OpenRouter)
    } else {
        None
    }
}

/// Multi-line user-facing error listing every supported credential option.
pub fn missing_credentials_error() -> String {
    "no LLM credentials configured. Set one of:\n\
     - OPENROUTER_API_KEY in .env.local (OpenRouter routing)\n\
     - ANTHROPIC_API_KEY in .env.local (direct api.anthropic.com)\n\
     - CLAUDE_CODE_OAUTH_TOKEN in env (via `claude setup-token`)"
        .to_string()
}

fn env_set(name: &str) -> bool {
    std::env::var(name)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

/// Qualify a bare Claude / Anthropic model id with the `anthropic/` provider
/// prefix when routing through OpenRouter (#268).
///
/// Why: OpenRouter's REST API requires the provider prefix on model ids
/// (e.g. `anthropic/claude-sonnet-4-6`). Agent TOMLs in the wild often carry
/// bare ids like `claude-sonnet-4-6` or `claude-haiku-4-5`. Without
/// qualification, sub-agent processes that route via OpenRouter return 400.
/// The PM loop already had this logic inline at one call site; this helper
/// centralizes the rule so every dispatch path (PM, persona, sub-agent) uses
/// the same prefix policy.
/// What: When `creds == OpenRouter` AND the model has no provider segment
/// (no `/`) AND it is not a `bedrock/` shorthand AND the bare name contains
/// `claude` or `anthropic`, returns `format!("anthropic/{model}")`. In all
/// other cases the model id is returned unchanged. Pure function; never
/// panics.
/// Test: `qualifies_bare_claude_id_for_openrouter`,
/// `leaves_already_prefixed_id_alone`, `leaves_non_openrouter_alone`,
/// `leaves_bedrock_shorthand_alone`, `leaves_unrelated_id_alone`.
pub fn qualify_openrouter_model(creds: &LlmCredentials, model: &str) -> String {
    if !matches!(creds, LlmCredentials::OpenRouter) {
        return model.to_string();
    }
    if model.contains('/') || model.starts_with("bedrock/") {
        return model.to_string();
    }
    let lower = model.to_ascii_lowercase();
    if lower.contains("claude") || lower.contains("anthropic") {
        format!("anthropic/{}", model)
    } else {
        model.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Helper: clear all three env vars before exercising precedence rules.
    /// SAFETY: env-mutation tests below are guarded by `#[serial]` AND
    /// `crate::test_env::ENV_LOCK` to prevent CI data races (#274). The
    /// `#[serial]` attribute serializes against any other `#[serial]` test in
    /// the binary; ENV_LOCK additionally serializes against the rest of the
    /// crate's env-touching tests that don't use serial_test.
    fn clear_all() {
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
        }
    }

    #[test]
    #[serial]
    fn pick_returns_none_when_nothing_set() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_all();
        assert!(pick_credentials(None).is_none());
    }

    #[test]
    #[serial]
    fn pick_picks_openrouter_when_only_openrouter_set() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_all();
        unsafe {
            std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-test");
        }
        assert_eq!(pick_credentials(None), Some(LlmCredentials::OpenRouter));
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
        }
    }

    #[test]
    #[serial]
    fn pick_picks_anthropic_when_only_anthropic_set() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_all();
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-api03-test");
        }
        assert_eq!(
            pick_credentials(None),
            Some(LlmCredentials::AnthropicDirect)
        );
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
    }

    #[test]
    #[serial]
    fn pick_picks_claude_code_when_oauth_set() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_all();
        unsafe {
            std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-test");
        }
        // Only when the agent's runner explicitly opts into claude-code.
        assert_eq!(
            pick_credentials(Some(crate::agents::RunnerKind::ClaudeCode)),
            Some(LlmCredentials::ClaudeCode)
        );
        unsafe {
            std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
        }
    }

    #[test]
    #[serial]
    fn pick_skips_claude_code_when_runner_not_claude_code() {
        // Regression for the bug where /provider reported claude-code (and the
        // dispatch went through the slow `claude` CLI) just because
        // CLAUDE_CODE_OAUTH_TOKEN was set in the environment, even though the
        // agent TOML didn't declare runner = "claude-code". Now the env var
        // alone must NOT auto-select claude-code; it should fall through to
        // OpenRouter.
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_all();
        unsafe {
            std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-test");
            std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-test");
        }
        // Default subprocess runner: must NOT pick claude-code.
        assert_eq!(
            pick_credentials(Some(crate::agents::RunnerKind::Subprocess)),
            Some(LlmCredentials::OpenRouter)
        );
        // None (caller doesn't know): also must NOT pick claude-code.
        assert_eq!(pick_credentials(None), Some(LlmCredentials::OpenRouter));
        unsafe {
            std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
            std::env::remove_var("OPENROUTER_API_KEY");
        }
    }

    #[test]
    #[serial]
    fn pick_prefers_claude_code_only_when_runner_opts_in() {
        // When OAuth + OpenRouter are both set AND the agent declares
        // runner = "claude-code", claude-code wins. Without the runner
        // opt-in, OpenRouter wins (see `pick_skips_claude_code_when_runner_not_claude_code`).
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_all();
        unsafe {
            std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "sk-ant-oat01-test");
            std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-test");
        }
        assert_eq!(
            pick_credentials(Some(crate::agents::RunnerKind::ClaudeCode)),
            Some(LlmCredentials::ClaudeCode)
        );
        unsafe {
            std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
            std::env::remove_var("OPENROUTER_API_KEY");
        }
    }

    #[test]
    #[serial]
    fn pick_prefers_anthropic_over_openrouter_when_both_set() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_all();
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-api03-test");
            std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-test");
        }
        assert_eq!(
            pick_credentials(None),
            Some(LlmCredentials::AnthropicDirect)
        );
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("OPENROUTER_API_KEY");
        }
    }

    #[test]
    fn label_renders_human_friendly_strings() {
        assert_eq!(LlmCredentials::ClaudeCode.label(), "claude-code");
        assert_eq!(LlmCredentials::AnthropicDirect.label(), "anthropic-direct");
        assert_eq!(LlmCredentials::OpenRouter.label(), "openrouter");
    }

    #[test]
    fn qualifies_bare_claude_id_for_openrouter() {
        let r = qualify_openrouter_model(&LlmCredentials::OpenRouter, "claude-sonnet-4-6");
        assert_eq!(r, "anthropic/claude-sonnet-4-6");
        let r2 = qualify_openrouter_model(&LlmCredentials::OpenRouter, "claude-haiku-4-5");
        assert_eq!(r2, "anthropic/claude-haiku-4-5");
    }

    #[test]
    fn leaves_already_prefixed_id_alone() {
        let r =
            qualify_openrouter_model(&LlmCredentials::OpenRouter, "anthropic/claude-sonnet-4-6");
        assert_eq!(r, "anthropic/claude-sonnet-4-6");
        let r2 = qualify_openrouter_model(&LlmCredentials::OpenRouter, "openai/gpt-4o");
        assert_eq!(r2, "openai/gpt-4o");
    }

    #[test]
    fn leaves_non_openrouter_alone() {
        // Direct Anthropic and ClaudeCode paths must not get an `anthropic/`
        // prefix — those endpoints use bare model ids.
        let r = qualify_openrouter_model(&LlmCredentials::AnthropicDirect, "claude-sonnet-4-6");
        assert_eq!(r, "claude-sonnet-4-6");
        let r2 = qualify_openrouter_model(&LlmCredentials::ClaudeCode, "claude-sonnet-4-6");
        assert_eq!(r2, "claude-sonnet-4-6");
    }

    #[test]
    fn leaves_bedrock_shorthand_alone() {
        let r = qualify_openrouter_model(&LlmCredentials::OpenRouter, "bedrock/claude-sonnet-4-6");
        assert_eq!(r, "bedrock/claude-sonnet-4-6");
    }

    #[test]
    fn leaves_unrelated_id_alone() {
        let r = qualify_openrouter_model(&LlmCredentials::OpenRouter, "gpt-4o");
        assert_eq!(r, "gpt-4o");
    }
}
