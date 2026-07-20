//! Credential routing for the controller's own LLM calls (#250, #3248).
//!
//! Why: The PM/ctrl orchestrator was hard-requiring `OPENROUTER_API_KEY` even
//! when the user has equally-valid alternative credentials configured
//! (`ANTHROPIC_API_KEY` from console.anthropic.com, or `CLAUDE_CODE_OAUTH_TOKEN`
//! from `claude setup-token`). This module centralizes the priority decision so
//! both the client constructor and the chat dispatcher agree on which backend
//! is active.
//!
//! Credential detection consults the SAME shared resolver
//! (`trusty_common::inference::credentials::resolve_key`) that
//! `trusty-channels` (Slack/Telegram) and this crate's own `GET /api/models`
//! catalog already use, rather than a raw `std::env::var` read (#3248). That
//! resolver checks process env, then `.env.local`, then the secure
//! keyring/file `KeyStore` `tagent config keys set` writes to — so a
//! credential configured ONLY via the store (never exported to the shell) is
//! no longer silently invisible to `pick_credentials()`. The three provider
//! names (`openrouter`, `anthropic`, `claude-code`) map to the exact same env
//! var names this module always used
//! (`trusty_common::inference::credentials::env_var_for`), so the env-tier
//! behaviour is unchanged — only the store fallback is new.
//!
//! What: `pick_credentials()` resolves the same three credentials in priority
//! order (CLAUDE_CODE_OAUTH_TOKEN > ANTHROPIC_API_KEY > OPENROUTER_API_KEY) and
//! returns a `LlmCredentials` enum describing the routing mode plus a friendly
//! display label for startup logging.
//!
//! Test: `pick_picks_claude_code_when_oauth_set`,
//! `pick_picks_anthropic_when_only_anthropic_set`,
//! `pick_picks_openrouter_when_only_openrouter_set`,
//! `pick_returns_none_when_nothing_set`,
//! `pick_consults_store_when_env_absent`.

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
/// What: Resolves three credentials via
/// [`trusty_common::inference::credentials::resolve_key`] (env >
/// `.env.local` > secure store — #3248). Returns `ClaudeCode` only when
/// `runner == Some(RunnerKind::ClaudeCode)` AND the `claude-code` credential
/// resolves. Otherwise prefers `AnthropicDirect` then `OpenRouter`. Returns
/// `None` when nothing applicable is configured.
/// Test: See module-level tests, including
/// `pick_skips_claude_code_when_runner_not_claude_code`,
/// `pick_consults_store_when_env_absent`.
pub fn pick_credentials(runner: Option<crate::agents::RunnerKind>) -> Option<LlmCredentials> {
    let openrouter = resolve_key_set("openrouter");
    let anthropic = resolve_key_set("anthropic");
    let claude_code = resolve_key_set("claude-code");
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
///
/// Why: since #3248 each option resolves via env, `.env.local`, OR the
/// secure `tagent config keys set` store — the message names all three tiers
/// so a user who already ran `config keys set` isn't told to re-export an env
/// var they don't need.
pub fn missing_credentials_error() -> String {
    "no LLM credentials configured. Set one of (env var, .env.local, or \
     `tagent config keys set <provider>`):\n\
     - OPENROUTER_API_KEY / `openrouter` (OpenRouter routing)\n\
     - ANTHROPIC_API_KEY / `anthropic` (direct api.anthropic.com)\n\
     - CLAUDE_CODE_OAUTH_TOKEN / `claude-code` (via `claude setup-token`)"
        .to_string()
}

/// Providers configured (via any resolver tier) that ctrl/PM's own LLM calls
/// cannot route through — `openrouter`/`anthropic`/`claude-code` are the only
/// three [`pick_credentials`] ever selects.
///
/// Why (#3406 follow-up, live-confirmed): a store entry for e.g. `fireworks`
/// (configured for a DIFFERENT consumer, such as a sub-agent's own model)
/// makes `pick_credentials` correctly return `None` for ctrl/PM — but a bare
/// "credential=none" then reads as "nothing is configured anywhere", which is
/// misleading when the store demonstrably has a working key. Naming the
/// mismatched provider turns a confusing dead end into an actionable
/// diagnostic ("you have `fireworks` configured, but ctrl/PM needs
/// openrouter/anthropic/claude-code").
/// What: walks the shared provider registry, excludes the three providers
/// `pick_credentials` already checks, and returns the `id` of every
/// remaining provider whose credential resolves via
/// [`trusty_common::inference::credentials::resolve_key`]. Never exposes a
/// value — names only, same discipline as `config keys list`.
/// Test: `other_configured_providers_names_fireworks_when_store_has_it`,
/// `other_configured_providers_empty_when_nothing_else_configured`.
pub fn other_configured_providers() -> Vec<&'static str> {
    trusty_common::inference::registry::all()
        .iter()
        .map(|caps| caps.id.as_str())
        .filter(|id| !matches!(*id, "openrouter" | "anthropic" | "claude-code"))
        .filter(|id| resolve_key_set(id))
        .collect()
}

/// Whether `provider`'s credential resolves via the shared 3-tier resolver,
/// without ever exposing the resolved value.
///
/// Why: the single call-through point for `pick_credentials()`'s three
/// probes, so all three consult the same env > `.env.local` > secure-store
/// precedence as every other unified-inference consumer (#3248) instead of a
/// raw `std::env::var` read that only ever saw the env tier.
/// What: delegates to
/// [`trusty_common::inference::credentials::resolve_key`] and keeps only the
/// `is_some()` boolean.
/// Test: `pick_consults_store_when_env_absent`.
fn resolve_key_set(provider: &str) -> bool {
    trusty_common::inference::credentials::resolve_key(provider).is_some()
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

    /// Why: since #3248, `pick_credentials()` also consults the shared secure
    /// store, not just process env — a dev machine with real credentials
    /// stashed via `tagent config keys set` (openrouter/anthropic) would make
    /// a hard-coded `is_none()` flaky. Mirrors the same self-consistency
    /// pattern `crate::api::server::models`'s `zero_credentials_configured_is_stable`
    /// test uses for the identical risk: assert `pick_credentials` agrees with
    /// what `resolve_key` reports right now (with `runner = None`, the
    /// claude-code branch never fires regardless of store contents, since it
    /// additionally requires `runner_is_claude_code`), rather than a fixed
    /// absence.
    /// Test: itself.
    #[test]
    #[serial]
    fn pick_returns_none_when_nothing_set() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_all();
        let expect_some = trusty_common::inference::credentials::resolve_key("openrouter")
            .is_some()
            || trusty_common::inference::credentials::resolve_key("anthropic").is_some();
        assert_eq!(pick_credentials(None).is_some(), expect_some);
    }

    /// Why: the entire point of #3248 — a credential configured ONLY in the
    /// secure store (never exported to the process env) must still be
    /// detected. Sandboxes both `$HOME` (so the store resolves against a
    /// tempdir, never the real one) and the credential env vars, holding both
    /// crate-wide locks for the full body per `test_env`'s documented
    /// convention for tests that mutate `$HOME`.
    /// What: writes `openrouter` directly to a `FileKeyStore` rooted at the
    /// sandboxed `$HOME`, clears every credential env var, and asserts
    /// `pick_credentials(None)` now resolves `OpenRouter` — which would have
    /// been silently `None` before the #3248 fix (raw `std::env::var` only).
    /// Test: itself.
    #[test]
    #[serial]
    fn pick_consults_store_when_env_absent() {
        let _env_guard = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _home_guard = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_all();

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: HOME_LOCK held for the entire test body.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        let store = trusty_common::inference::credentials::FileKeyStore::at(tmp.path());
        trusty_common::inference::credentials::KeyStore::set(
            &store,
            "openrouter",
            "sk-or-from-store", // pragma: allowlist secret
        )
        .expect("seed store");

        assert_eq!(pick_credentials(None), Some(LlmCredentials::OpenRouter));

        // SAFETY: HOME_LOCK still held.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
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

    /// Why: the exact confusing case the owner hit live — `fireworks`
    /// configured via the secure store (a real, working key for a DIFFERENT
    /// consumer) while ctrl/PM's own three providers are all absent. The
    /// diagnostic must name `fireworks` rather than reporting a bare "none".
    /// Sandboxes `$HOME` (mirrors `pick_consults_store_when_env_absent`) so
    /// the store write never touches the real machine's store.
    /// Test: itself.
    #[test]
    #[serial]
    fn other_configured_providers_names_fireworks_when_store_has_it() {
        let _env_guard = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _home_guard = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_all();

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: HOME_LOCK held for the entire test body.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        let store = trusty_common::inference::credentials::FileKeyStore::at(tmp.path());
        trusty_common::inference::credentials::KeyStore::set(
            &store,
            "fireworks",
            "fw-from-store", // pragma: allowlist secret
        )
        .expect("seed store");

        assert_eq!(pick_credentials(None), None);
        let other = other_configured_providers();
        assert!(
            other.contains(&"fireworks"),
            "expected fireworks to be named: {other:?}"
        );
        assert!(
            !other
                .iter()
                .any(|p| matches!(*p, "openrouter" | "anthropic" | "claude-code")),
            "the three providers pick_credentials already checks must never \
             appear here: {other:?}"
        );

        // SAFETY: HOME_LOCK still held.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    /// Why: when NOTHING is configured anywhere, the diagnostic must be
    /// empty — the plain-CLI banner falls back to a bare "none" in that case.
    /// Test: itself.
    #[test]
    #[serial]
    fn other_configured_providers_empty_when_nothing_else_configured() {
        let _env_guard = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _home_guard = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_all();

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: HOME_LOCK held for the entire test body.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        assert_eq!(other_configured_providers(), Vec::<&'static str>::new());

        // SAFETY: HOME_LOCK still held.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
