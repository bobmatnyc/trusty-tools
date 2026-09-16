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
/// Why (#7955): the former default, `openai/gpt-4o-mini`, is rejected with a
/// `404 zdr-violation-by-guardrail` by any OpenRouter account with the
/// zero-data-retention (ZDR) guardrail enabled — every unpinned `run-task`
/// and the ignored `agent_loop_live` smoke failed identically at the first
/// chat call. The default tracks whatever [`normalize_model_alias`]'s
/// `"sonnet"` alias resolves to, so an unpinned run and a `--pm-model sonnet`
/// run always reach the same provider slug.
/// What: The OpenRouter slug for Claude Sonnet 5.
/// Test: `routing::tests::resolve_model_falls_back_to_default`,
/// `routing::tests::default_model_matches_the_sonnet_alias`.
// #8128: retargeted from `anthropic/claude-sonnet-4.5` to the Claude 5 tier
// Claude Code itself now runs; slug read from `GET
// https://openrouter.ai/api/v1/models`.
pub const DEFAULT_MODEL: &str = "anthropic/claude-sonnet-5";

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

/// Short Claude model aliases (`opus`/`sonnet`/`haiku`) mapped to a concrete,
/// provider-valid OpenRouter slug (#3438).
///
/// Why: these three bare words are the `claude` CLI's own `--model` shorthand
/// (see `trusty_agents_common::agents::builder::tier_to_model`, which composes
/// them from an agent's `resource_tier`, and every embedded roster agent's
/// `.md` frontmatter — e.g. `assets/agents/python-engineer.md`'s
/// `model: sonnet`) — valid input for the `claude` subprocess trusty-agents
/// spawns, but NOT a real OpenRouter/Anthropic-API model id. trusty-code talks
/// to providers directly (`llm::client::OpenAiCompatClient`), so a bare alias
/// reaching the wire produces `API error 400: "opus is not a valid model ID"`
/// verbatim — the exact failure #3438 traced. [`resolve_model`] is the single
/// funnel every call site resolves its final slug through
/// (`runner::in_process::InProcessAgentRunner::run_pipeline`,
/// `task::executor`, `run_task::execute_run_task`), so normalising here fixes
/// every path at once rather than patching each `.md` file or call site
/// individually.
/// What: Case-insensitive exact match on the trimmed slug; anything else
/// (an already-concrete slug, a routed `bedrock/`/`fireworks/`/etc. prefix, an
/// unrecognised alias) passes through unchanged — [`resolve_model`] never
/// errors on an unmapped model, it just doesn't rewrite it.
/// Test: `routing::tests::normalize_model_alias_maps_short_aliases`,
/// `routing::tests::normalize_model_alias_passes_through_concrete_slugs`,
/// `assets::tests::every_embedded_agent_model_normalizes_to_a_valid_slug`.
// #8128: the three targets track Claude Code's CURRENT tiers (opus 5 /
// sonnet 5 / haiku 4.5, the newest Haiku OpenRouter serves), not the 4.5
// generation they were pinned to. Each slug was read from `GET
// https://openrouter.ai/api/v1/models`, never composed by hand.
fn normalize_model_alias(model: &str) -> &str {
    match model.trim().to_ascii_lowercase().as_str() {
        "opus" => "anthropic/claude-opus-5",
        "sonnet" => "anthropic/claude-sonnet-5",
        "haiku" => "anthropic/claude-haiku-4.5",
        _ => model,
    }
}

/// Resolve the model slug for an invocation.
///
/// Why: Centralises the precedence so per-call overrides, per-agent config, and
/// the global default never disagree across call sites.
/// What: Returns, in priority order, the first non-empty of:
/// (1) `run_context.model` (per-call override),
/// (2) `agent_config.agent.model`,
/// (3) `agent_config.llm.model_override`,
/// else (4) [`DEFAULT_MODEL`] — each candidate passed through
/// [`normalize_model_alias`] (#3438) so a bare `opus`/`sonnet`/`haiku` alias
/// never reaches a provider as-is.
/// Test: `routing::tests::resolve_model_*` cover every tier;
/// `routing::tests::resolve_model_normalizes_short_alias_from_agent_config`
/// and `resolve_model_normalizes_short_alias_from_run_context` cover #3438.
pub fn resolve_model(agent_config: &AgentConfig, run_context: Option<&RunContext>) -> String {
    // 1. Per-call override wins.
    if let Some(ctx) = run_context
        && let Some(model) = non_empty(ctx.model.as_deref())
    {
        return normalize_model_alias(model).to_string();
    }

    // 2. Agent-level model.
    if let Some(model) = non_empty(agent_config.agent.model.as_deref()) {
        return normalize_model_alias(model).to_string();
    }

    // 3. `[llm].model_override` (lower precedence than `[agent].model`).
    if let Some(model) = non_empty(agent_config.llm.model_override.as_deref()) {
        return normalize_model_alias(model).to_string();
    }

    // 4. Built-in default.
    DEFAULT_MODEL.to_string()
}

/// Environment variable overriding the TOP-LEVEL (PM) agent's model (#8030).
///
/// Why: `--engineer-model`/[`ENGINEER_MODEL_ENV_VAR`] only rewire the
/// DELEGATED runner, so the top-level agent's own calls could be repointed
/// only by hand-editing the deployed `.trusty-code/agents/<agent>.md` — the
/// concrete failure #8030 records is `--engineer-model` set to a ZDR-safe
/// slug while the PM's own calls still 404'd on the old default.
/// What: The middle precedence tier of [`resolve_pm_model_override`], below
/// the `--pm-model` flag and above the agent's front-matter `model:`.
/// Test: `routing::tests::resolve_pm_model_override_env_wins_over_nothing`,
/// `routing::tests::resolve_pm_model_override_flag_wins_over_env`.
pub const PM_MODEL_ENV_VAR: &str = "TCODE_PM_MODEL";

/// Environment variable overriding the PM loop's turn cap (#8128).
///
/// Why: a bake-off/parity run against Claude Code's tiers needs a turn cap
/// bigger than [`crate::agent_loop::AgentLoopConfig`]'s built-in 8 for a
/// multi-step delivery task, and an env var raises it for a whole invocation
/// without a flag at every call site — the same escape-hatch precedent
/// [`RUN_DEADLINE_ENV_VAR`] sets.
/// What: The middle precedence tier of [`resolve_max_turns`].
/// Test: `routing::tests::resolve_max_turns_env_wins_over_default`,
/// `routing::tests::resolve_max_turns_rejects_zero_from_env`.
pub const MAX_TURNS_ENV_VAR: &str = "TCODE_MAX_TURNS";

/// Resolve the TOP-LEVEL (PM) agent's model override for one invocation.
///
/// Why (#8030): the top-level agent's model had no CLI/env override at all —
/// `resolve_model(&pm_config, None)` went straight to the agent's front
/// matter. This is the missing first tier, shaped exactly like the engineer's
/// so one precedence story covers both halves of a run.
/// What: Returns the first non-blank of (1) `cli_override` (the `--pm-model`
/// flag or `task.run`'s `pm_model` field), (2) [`PM_MODEL_ENV_VAR`]. `None`
/// means "no override" and leaves [`resolve_model`]'s own ladder (front
/// matter, then [`DEFAULT_MODEL`]) untouched. The returned value is NOT
/// normalised here — [`resolve_model_with_override`] runs it through
/// [`normalize_model_alias`] at the point of use, so `--pm-model opus`
/// behaves exactly like `--engineer-model opus`.
/// Test: `routing::tests::resolve_pm_model_override_flag_wins_over_env`,
/// `routing::tests::resolve_pm_model_override_env_wins_over_nothing`,
/// `routing::tests::resolve_pm_model_override_blank_is_absent`.
pub fn resolve_pm_model_override(cli_override: Option<String>) -> Option<String> {
    if let Some(model) = cli_override.filter(|s| !s.trim().is_empty()) {
        return Some(model);
    }
    std::env::var(PM_MODEL_ENV_VAR)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Resolve a model slug with an explicit per-run override ahead of the agent
/// config (#8030).
///
/// Why: every top-level call site reads `resolve_model(&cfg, None)`, and
/// threading a synthetic [`RunContext`] through each one purely to carry a
/// CLI flag would restate a whole tool-call context to express one string.
/// What: `override_model`, when non-blank, wins and is passed through
/// [`normalize_model_alias`]; otherwise this is exactly
/// `resolve_model(agent_config, None)`.
/// Test: `routing::tests::resolve_model_with_override_wins_over_agent_config`,
/// `routing::tests::resolve_model_with_override_normalizes_short_alias`,
/// `routing::tests::resolve_model_with_override_absent_matches_resolve_model`.
pub fn resolve_model_with_override(
    agent_config: &AgentConfig,
    override_model: Option<&str>,
) -> String {
    if let Some(model) = non_empty(override_model) {
        return normalize_model_alias(model).to_string();
    }
    resolve_model(agent_config, None)
}

/// Resolve the PM loop's turn-cap override for one invocation (#8128).
///
/// Why: `run_task`/`task::executor` never overrode
/// [`crate::agent_loop::AgentLoopConfig`]'s `max_turns: 8`, so a multi-step
/// delivery task could not be given more turns without a rebuild.
/// What: Returns the first of (1) `cli_override` (`--max-turns` /
/// `task.run`'s `max_turns`), (2) [`MAX_TURNS_ENV_VAR`] parsed as a `u32`,
/// else `Ok(None)` — "no override", which leaves the loop's own default in
/// place so absent behaviour is unchanged. A zero from EITHER source is an
/// `Err`: a zero-turn loop makes no LLM call at all and would report an
/// empty run as a normal one. An unparseable env value is treated as absent
/// (logged at `warn`), matching [`resolve_deadline_secs`].
/// Test: `routing::tests::resolve_max_turns_flag_wins_over_env`,
/// `routing::tests::resolve_max_turns_env_wins_over_default`,
/// `routing::tests::resolve_max_turns_rejects_zero_from_flag`,
/// `routing::tests::resolve_max_turns_rejects_zero_from_env`,
/// `routing::tests::resolve_max_turns_invalid_env_falls_back_to_default`.
pub fn resolve_max_turns(cli_override: Option<u32>) -> Result<Option<u32>, String> {
    if let Some(turns) = cli_override {
        if turns == 0 {
            return Err("--max-turns must be at least 1 (0 would run no turns at all)".to_string());
        }
        return Ok(Some(turns));
    }

    if let Ok(raw) = std::env::var(MAX_TURNS_ENV_VAR) {
        match raw.trim().parse::<u32>() {
            Ok(0) => {
                return Err(format!(
                    "{MAX_TURNS_ENV_VAR}=0 is invalid: the turn cap must be at least 1 \
                     (0 would run no turns at all)"
                ));
            }
            Ok(turns) => return Ok(Some(turns)),
            Err(e) => {
                tracing::warn!(
                    "{MAX_TURNS_ENV_VAR}={raw:?} is not a valid u32 ({e}); \
                     falling back to the loop's built-in turn cap"
                );
            }
        }
    }

    Ok(None)
}

/// Fallback context-window size (in tokens) for a model slug this resolver
/// does not recognise.
///
/// Why: [`resolve_context_window`] must always return a usable number even
/// for an unlisted/unknown slug (a new provider, a typo'd override, etc.) —
/// 128,000 is a conservative, widely-supported floor shared by several
/// mainstream hosted models (e.g. `gpt-4o`), so an unknown model is treated
/// no more generously than that.
/// What: The fallback tier of [`resolve_context_window`].
/// Test: `routing::tests::resolve_context_window_falls_back_to_default_for_unknown_model`.
pub const DEFAULT_CONTEXT_WINDOW: usize = 128_000;

/// Resolve the real context-window size (in tokens) for a model slug.
///
/// Why: #2308 — `CompactionConfig::default()`'s flat `token_threshold: 6_000`
/// is model-blind: ~3% of Bedrock Claude Sonnet's real 200K-token window, so
/// ordinary coding turns (post-#2261 tool-call-arg counting) blow through it
/// constantly and trigger pathological re-compaction. Compaction must instead
/// scale with the model actually in use, which means a resolver is needed
/// alongside [`resolve_model`] to turn a slug into a window size before
/// [`crate::agent_loop::CompactionConfig::for_context_window`] can compute a
/// proportional threshold.
/// What: A minimal substring/format table, checked in order: (1) any slug
/// containing `claude-sonnet` or `claude-opus` (covers both the bare
/// Anthropic slug and the Bedrock-routed
/// `bedrock/us.anthropic.claude-sonnet-4-6` / `-opus-*` inference-profile
/// format, see `crate::llm::bedrock::bedrock_model_id`) -> 200,000; (2) any
/// slug beginning with the `atlascloud/` routing prefix -> 1,050,000 (#2536 —
/// AtlasCloud's catalog default `openai/gpt-5.6-sol` has a 1.05M-token window,
/// keyed off the routing prefix rather than the nested model id so the whole
/// AtlasCloud family gets the large window); (3) any slug containing `gpt-4o` ->
/// 128,000; else (4) [`DEFAULT_CONTEXT_WINDOW`]. Deliberately NOT a config file
/// or per-agent override — the window size is a property of the model itself, not
/// something an operator should need to tune per project.
/// Test: `routing::tests::resolve_context_window_maps_bedrock_sonnet_to_200k`,
/// `routing::tests::resolve_context_window_maps_atlascloud_to_1m`,
/// `routing::tests::resolve_context_window_falls_back_to_default_for_unknown_model`.
pub fn resolve_context_window(model: &str) -> usize {
    if model.contains("claude-sonnet") || model.contains("claude-opus") {
        200_000
    } else if model.starts_with("atlascloud/") {
        1_050_000
    } else if model.contains("gpt-4o") {
        128_000
    } else {
        DEFAULT_CONTEXT_WINDOW
    }
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

/// Same rationale as [`RUN_DEADLINE_ENV_LOCK`], for the two process-wide
/// top-level-override variables ([`PM_MODEL_ENV_VAR`], [`MAX_TURNS_ENV_VAR`]).
/// One lock covers both: no test needs them held independently, and a single
/// lock cannot deadlock against itself the way a pair acquired in two orders
/// could (#8030, #8128).
#[cfg(test)]
pub(crate) static TOP_LEVEL_OVERRIDE_ENV_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

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

    // ── #3438: short-alias normalization ────────────────────────────────────

    /// `normalize_model_alias` maps all three short Claude aliases to a
    /// concrete OpenRouter slug, case-insensitively and trimmed.
    ///
    /// Why: `opus`/`sonnet`/`haiku` are the `claude` CLI's own shorthand
    /// (`trusty_agents_common::agents::builder::tier_to_model`'s output and
    /// every composed roster agent's `.md` `model:` field), not valid
    /// OpenRouter/API model ids — this is the exact substitution that fixes
    /// the `"opus is not a valid model ID"` 400 (#3438).
    /// What: Each of the three aliases, plus an uppercase/padded variant,
    /// resolves to its mapped concrete slug.
    /// Test: this test.
    #[test]
    fn normalize_model_alias_maps_short_aliases() {
        // #8128: the Claude 5 tiers, read from OpenRouter's own model list.
        assert_eq!(normalize_model_alias("opus"), "anthropic/claude-opus-5");
        assert_eq!(normalize_model_alias("sonnet"), "anthropic/claude-sonnet-5");
        assert_eq!(normalize_model_alias("haiku"), "anthropic/claude-haiku-4.5");
        assert_eq!(
            normalize_model_alias("  Opus  "),
            "anthropic/claude-opus-5",
            "must be case-insensitive and trim surrounding whitespace"
        );
    }

    /// [`DEFAULT_MODEL`] is the SAME slug the `"sonnet"` alias resolves to.
    ///
    /// Why (#8128): the default's own docs promise an unpinned run and a
    /// `--pm-model sonnet` run reach one provider slug. Pinning that here is
    /// what stops the two from drifting the next time a tier moves — the
    /// exact drift #8128 found, where the alias table and the default were
    /// both stale but only one was noticed.
    /// What: asserts the equality directly.
    /// Test: this test.
    #[test]
    fn default_model_matches_the_sonnet_alias() {
        assert_eq!(DEFAULT_MODEL, normalize_model_alias("sonnet"));
    }

    /// An already-concrete slug (or any string that isn't one of the three
    /// bare aliases) passes through [`normalize_model_alias`] unchanged.
    ///
    /// Why: The normalizer must not mangle a valid `vendor/model` slug, a
    /// routed `bedrock/`/`fireworks/`/`together/`/`atlascloud/` prefix, or an
    /// unrecognised model string — it only rewrites the three known aliases.
    /// What: A realistic OpenRouter slug and an arbitrary unknown string both
    /// round-trip unchanged.
    /// Test: this test.
    #[test]
    fn normalize_model_alias_passes_through_concrete_slugs() {
        assert_eq!(
            normalize_model_alias("anthropic/claude-sonnet-4-5"),
            "anthropic/claude-sonnet-4-5"
        );
        assert_eq!(
            normalize_model_alias("bedrock/us.anthropic.claude-opus-4-6"),
            "bedrock/us.anthropic.claude-opus-4-6"
        );
        assert_eq!(
            normalize_model_alias("some-vendor/unheard-of-model"),
            "some-vendor/unheard-of-model"
        );
    }

    /// [`resolve_model`] normalizes a bare alias sourced from `[agent].model`
    /// (the tier-composed or `.md`-frontmatter case #3438 actually hit).
    ///
    /// Why: Regression guard at the public `resolve_model` seam, not just the
    /// private helper — this is what every real call site invokes.
    /// What: `agent.model = "opus"` resolves to the concrete slug, not the
    /// bare alias.
    /// Test: this test.
    #[test]
    fn resolve_model_normalizes_short_alias_from_agent_config() {
        let config = cfg(Some("opus"), None);
        assert_eq!(resolve_model(&config, None), "anthropic/claude-opus-5");
    }

    /// [`resolve_model`] normalizes a bare alias sourced from a per-call
    /// `RunContext.model` override too.
    ///
    /// Why: The highest-precedence tier must not bypass normalization — a
    /// caller pinning `RunContext.model = "sonnet"` must still reach the
    /// provider with a valid concrete id.
    /// What: `RunContext.model = "sonnet"` resolves to the concrete slug.
    /// Test: this test.
    #[test]
    fn resolve_model_normalizes_short_alias_from_run_context() {
        let config = cfg(Some("anthropic/claude-haiku-4-5"), None);
        let ctx = RunContext {
            model: Some("sonnet".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_model(&config, Some(&ctx)),
            "anthropic/claude-sonnet-5"
        );
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

    // ── #2308: resolve_context_window ───────────────────────────────────────

    /// The real Bedrock-routed Claude Sonnet slug maps to a 200K window.
    ///
    /// Why: This is the exact model whose flat 6,000-token compaction
    /// threshold (#2308) was ~3% of its real context window; the resolver
    /// must recognise the SAME slug format `resolve_model`/the Bedrock
    /// dispatch layer actually produce
    /// (`bedrock/us.anthropic.claude-sonnet-4-6`, see
    /// `crate::llm::bedrock::bedrock_model_id`'s doctests), not just a bare
    /// `claude-sonnet-*` slug.
    /// What: Assert the real Bedrock dispatch slug resolves to 200,000.
    /// Test: this test.
    #[test]
    fn resolve_context_window_maps_bedrock_sonnet_to_200k() {
        assert_eq!(
            resolve_context_window("bedrock/us.anthropic.claude-sonnet-4-6"),
            200_000
        );
        assert_eq!(
            resolve_context_window("bedrock/us.anthropic.claude-opus-4-6"),
            200_000
        );
    }

    /// An `atlascloud/*` slug maps to AtlasCloud's 1.05M-token window (#2536),
    /// including the nested `atlascloud/openai/gpt-5.6-sol` form.
    ///
    /// Why: AtlasCloud's default `openai/gpt-5.6-sol` carries a 1,050,000-token
    /// context window; keying off the `atlascloud/` routing prefix (not the
    /// nested `gpt-4o`-adjacent model id) gives the whole family the large window
    /// so compaction scales correctly.
    /// What: Assert the nested and a bare AtlasCloud slug resolve to 1,050,000.
    /// Test: this test.
    #[test]
    fn resolve_context_window_maps_atlascloud_to_1m() {
        assert_eq!(
            resolve_context_window("atlascloud/openai/gpt-5.6-sol"),
            1_050_000
        );
        assert_eq!(resolve_context_window("atlascloud/deepseek-v3"), 1_050_000);
    }

    /// An unrecognised model slug falls back to [`DEFAULT_CONTEXT_WINDOW`].
    ///
    /// Why: A resolver that panics or silently returns 0 on an unknown slug
    /// would break every downstream compaction-threshold calculation.
    /// What: Assert an arbitrary/unknown slug resolves to the default.
    /// Test: this test.
    #[test]
    fn resolve_context_window_falls_back_to_default_for_unknown_model() {
        assert_eq!(
            resolve_context_window("some-vendor/unheard-of-model-v1"),
            DEFAULT_CONTEXT_WINDOW
        );
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

    // ── #8030 / #8128: top-level PM overrides ───────────────────────────────

    /// Serializes access to one of the two top-level-override env vars for a
    /// single test closure, restoring the prior (absent) state afterward.
    async fn with_env_var<T>(name: &str, value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = TOP_LEVEL_OVERRIDE_ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by the lock above.
        unsafe {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
        let result = f();
        unsafe {
            std::env::remove_var(name);
        }
        result
    }

    /// The `--pm-model` flag wins over [`PM_MODEL_ENV_VAR`].
    ///
    /// Why (#8030): an operator pinning the top-level model for one run must
    /// not be silently overridden by an exported default.
    /// What: env set to one slug, flag to another; assert the flag.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_pm_model_override_flag_wins_over_env() {
        with_env_var(PM_MODEL_ENV_VAR, Some("anthropic/claude-haiku-4.5"), || {
            assert_eq!(
                resolve_pm_model_override(Some("anthropic/claude-opus-5".to_string())),
                Some("anthropic/claude-opus-5".to_string())
            );
        })
        .await;
    }

    /// [`PM_MODEL_ENV_VAR`] supplies the override when no flag is given, and
    /// its absence means "no override at all".
    ///
    /// Why (#8030): the env tier is what lets a bake-off runner repoint the
    /// top-level agent for a whole invocation without a flag at every call.
    /// What: with the var set, the value is returned; unset, `None`.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_pm_model_override_env_wins_over_nothing() {
        with_env_var(PM_MODEL_ENV_VAR, Some("anthropic/claude-opus-5"), || {
            assert_eq!(
                resolve_pm_model_override(None),
                Some("anthropic/claude-opus-5".to_string())
            );
        })
        .await;
        with_env_var(PM_MODEL_ENV_VAR, None, || {
            assert_eq!(resolve_pm_model_override(None), None);
        })
        .await;
    }

    /// A blank flag or env value is treated as absent at both tiers.
    ///
    /// Why: `--pm-model ""` (or an exported empty var) must fall through to
    /// the agent's own model, not pin an empty slug a provider would reject.
    /// What: blank flag with a blank env → `None`; blank flag with a real env
    /// value → the env value.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_pm_model_override_blank_is_absent() {
        with_env_var(PM_MODEL_ENV_VAR, Some("  "), || {
            assert_eq!(resolve_pm_model_override(Some("   ".to_string())), None);
        })
        .await;
        with_env_var(PM_MODEL_ENV_VAR, Some("anthropic/claude-opus-5"), || {
            assert_eq!(
                resolve_pm_model_override(Some("".to_string())),
                Some("anthropic/claude-opus-5".to_string())
            );
        })
        .await;
    }

    /// [`resolve_model_with_override`] puts the per-run override ahead of the
    /// agent config.
    ///
    /// Why (#8030): this is the whole point of the new tier — the top-level
    /// agent's front-matter `model:` must lose to an explicit run override.
    /// What: config pins one slug, override names another; assert the
    /// override.
    /// Test: this test.
    #[test]
    fn resolve_model_with_override_wins_over_agent_config() {
        let config = cfg(Some("anthropic/claude-haiku-4.5"), None);
        assert_eq!(
            resolve_model_with_override(&config, Some("anthropic/claude-opus-5")),
            "anthropic/claude-opus-5"
        );
    }

    /// A short alias reaching [`resolve_model_with_override`] is normalised,
    /// exactly as one reaching [`resolve_model`] is.
    ///
    /// Why (#3438/#8030): `--pm-model opus` must behave like
    /// `--engineer-model opus`; a bare alias on the wire is the
    /// `"opus is not a valid model ID"` 400.
    /// What: `"opus"` resolves to the concrete Claude 5 slug.
    /// Test: this test.
    #[test]
    fn resolve_model_with_override_normalizes_short_alias() {
        let config = cfg(Some("anthropic/claude-haiku-4.5"), None);
        assert_eq!(
            resolve_model_with_override(&config, Some("opus")),
            "anthropic/claude-opus-5"
        );
    }

    /// With no override, [`resolve_model_with_override`] is exactly
    /// [`resolve_model`] — including the blank-is-absent rule.
    ///
    /// Why: the new tier must be additive; every pre-#8030 call site keeps
    /// its behaviour byte-for-byte.
    /// What: `None` and a blank string both fall through to the config, and a
    /// configless agent falls through to [`DEFAULT_MODEL`].
    /// Test: this test.
    #[test]
    fn resolve_model_with_override_absent_matches_resolve_model() {
        let config = cfg(Some("anthropic/claude-haiku-4.5"), None);
        assert_eq!(
            resolve_model_with_override(&config, None),
            resolve_model(&config, None)
        );
        assert_eq!(
            resolve_model_with_override(&config, Some("  ")),
            "anthropic/claude-haiku-4.5"
        );
        assert_eq!(
            resolve_model_with_override(&cfg(None, None), None),
            DEFAULT_MODEL
        );
    }

    /// The `--max-turns` flag wins over [`MAX_TURNS_ENV_VAR`].
    ///
    /// Why (#8128): same precedence contract every other per-run override in
    /// this module follows.
    /// What: env set to one cap, flag to another; assert the flag.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_max_turns_flag_wins_over_env() {
        with_env_var(MAX_TURNS_ENV_VAR, Some("40"), || {
            assert_eq!(resolve_max_turns(Some(12)), Ok(Some(12)));
        })
        .await;
    }

    /// [`MAX_TURNS_ENV_VAR`] supplies the cap when no flag is given, and its
    /// absence leaves the loop's own default in place.
    ///
    /// Why (#8128): `None` must mean "do not override", so an absent flag
    /// changes nothing about a run.
    /// What: with the var set, that cap is returned; unset, `Ok(None)`.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_max_turns_env_wins_over_default() {
        with_env_var(MAX_TURNS_ENV_VAR, Some("40"), || {
            assert_eq!(resolve_max_turns(None), Ok(Some(40)));
        })
        .await;
        with_env_var(MAX_TURNS_ENV_VAR, None, || {
            assert_eq!(resolve_max_turns(None), Ok(None));
        })
        .await;
    }

    /// A zero flag is an error, not a run that does nothing.
    ///
    /// Why (#8128): a zero-turn loop makes no LLM call and would report an
    /// empty run as a normal one — the operator must be told, not guessed at.
    /// What: `Some(0)` returns `Err` naming the flag.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_max_turns_rejects_zero_from_flag() {
        with_env_var(MAX_TURNS_ENV_VAR, None, || {
            let err = resolve_max_turns(Some(0)).expect_err("0 must be rejected");
            assert!(
                err.contains("--max-turns"),
                "the error must name the flag; got {err:?}"
            );
        })
        .await;
    }

    /// A zero env value is rejected the same way as a zero flag.
    ///
    /// Why (#8128): the env tier must not be a back door around the flag's
    /// own validation.
    /// What: `TCODE_MAX_TURNS=0` returns `Err` naming the variable.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_max_turns_rejects_zero_from_env() {
        with_env_var(MAX_TURNS_ENV_VAR, Some("0"), || {
            let err = resolve_max_turns(None).expect_err("0 must be rejected");
            assert!(
                err.contains(MAX_TURNS_ENV_VAR),
                "the error must name the env var; got {err:?}"
            );
        })
        .await;
    }

    /// An unparseable env value falls through to "no override" rather than
    /// erroring the whole run.
    ///
    /// Why: matches [`resolve_deadline_secs`]'s established degrade-gracefully
    /// contract for a misconfigured environment.
    /// What: a non-numeric value yields `Ok(None)`.
    /// Test: this test.
    #[tokio::test]
    async fn resolve_max_turns_invalid_env_falls_back_to_default() {
        with_env_var(MAX_TURNS_ENV_VAR, Some("not-a-number"), || {
            assert_eq!(resolve_max_turns(None), Ok(None));
        })
        .await;
    }
}
