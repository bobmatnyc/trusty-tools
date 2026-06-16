//! `[session_manager]` configuration schema (DOC-14 spec §10).
//!
//! Why: the Session Manager (SM) agent — the PM-like orchestrator that delegates
//! all work by spawning t-mpm sessions — needs a single, opt-in config surface so
//! operators can pick inference providers, model tiers, memory palace, and the
//! rolling-context window without recompiling. SM-1 lays this foundation; later
//! tickets (SM-2..SM-8) read these fields. The top-level `enabled` flag defaults
//! to `false`, so an absent or partial `[session_manager]` section is a strict
//! no-op that preserves the legacy `LlmOverseer`/coordinator behavior.
//! What: [`SessionManagerConfig`] mirrors spec §10's TOML shape — a top-level
//! `enabled` plus three nested subsections (`[session_manager.inference]`,
//! `[session_manager.memory]`, `[session_manager.rounds]`). Every struct and
//! field is `#[serde(default)]`, so partial and absent config both deserialize
//! to sensible defaults.
//! Test: `sm_config_full_parsed`, `sm_config_partial_takes_defaults`,
//! `sm_config_absent_is_all_defaults`, `sm_config_enabled_default_false` in the
//! `tests` module below; integration coverage via `MpmConfig` lives in
//! `core/config.rs::tests`.

use serde::{Deserialize, Serialize};

/// `[session_manager]` — top-level Session Manager configuration (spec §10).
///
/// Why: gives the SM agent a typed, opt-in home in `~/.trusty-mpm/config.toml`.
/// Disabled by default (`enabled = false`) so merely defining the struct never
/// alters runtime behavior — the legacy overseer path stays untouched until an
/// operator opts in and a later ticket (SM-7) wires the agent into an endpoint.
/// What: an `enabled` toggle plus three subsections — `inference` (§5,
/// provider and model tiers), `memory` (§8, palace and recall), and `rounds`
/// (§7, rolling window). All fields are `#[serde(default)]` so partial config
/// parses.
/// Test: `sm_config_*` in the `tests` module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SessionManagerConfig {
    /// Master opt-in switch. `false` (the default) → the SM is inert and the
    /// legacy `LlmOverseer`/coordinator behavior is preserved unchanged.
    pub enabled: bool,

    /// `[session_manager.inference]` — provider selection + per-task model tiers.
    pub inference: SmInferenceConfig,

    /// `[session_manager.memory]` — dedicated SM palace + recall settings.
    pub memory: SmMemoryConfig,

    /// `[session_manager.rounds]` — rolling verbatim context window.
    pub rounds: SmRoundsConfig,
}

/// `[session_manager.inference]` — provider + model-tier selection (spec §5).
///
/// Why: the SM is multi-provider (OpenRouter / Bedrock / Anthropic) and selects
/// a model per task (orchestration vs. summarization vs. compaction, §5.4). This
/// section captures that selection declaratively; SM-2 consumes it to build the
/// provider abstraction. Auth is NOT stored here — it is resolved from env
/// (`OPENROUTER_API_KEY`, AWS chain, `ANTHROPIC_API_KEY`) per spec §10.
/// What: a `provider` selector, three model-tier ids (`sm_model`,
/// `summary_model`, `compaction_model`) with an optional provider prefix, a
/// deprecated `model` alias (interpreted as `sm_model` when that is unset), an
/// ordered `fallback` provider list, sampling `temperature`, and two token
/// budgets governing the compaction safety valve.
/// Test: `sm_config_full_parsed`, `sm_config_partial_takes_defaults`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmInferenceConfig {
    /// Provider selector: `"auto"` | `"openrouter"` | `"bedrock"` | `"anthropic"`.
    /// Default `"auto"` lets SM-2 pick the first provider with valid credentials.
    pub provider: String,

    /// SM chat/orchestration model — MEDIUM (Sonnet) tier. May carry an
    /// `openrouter/` `bedrock/` or `anthropic/` prefix to pin a provider.
    /// Empty → SM-2 uses the active provider's medium-tier default.
    pub sm_model: String,

    /// Session `last_summary` + compaction compression model — INEXPENSIVE
    /// (Haiku) tier. Empty → SM-2 uses the active provider's cheap-tier default.
    pub summary_model: String,

    /// Deprecated back-compat alias for [`sm_model`](Self::sm_model). When
    /// `sm_model` is empty, SM-2 interprets this field as the SM model. Empty by
    /// default; new configs should set `sm_model` instead.
    pub model: String,

    /// Ordered provider-fallback chain, e.g. `["openrouter", "bedrock"]`.
    /// Empty (the default) → no fallback.
    pub fallback: Vec<String>,

    /// Sampling temperature for SM orchestration calls. Default `0.3`.
    pub temperature: f32,

    /// Compaction safety-valve trigger: when assembled context exceeds this many
    /// tokens, the rolling window is compacted (§7.2). Default `24000`.
    pub context_token_budget: u32,

    /// Explicit model for compaction only. Empty (the default) → reuse
    /// `summary_model` (Haiku). A non-empty, optionally prefixed id overrides it.
    pub compaction_model: String,

    /// Hard cap on the compressed-context summary the compaction call may emit
    /// (§7.3). Default `4000` tokens.
    pub compressed_context_max_tokens: u32,
}

impl Default for SmInferenceConfig {
    /// Why: serde `#[serde(default)]` on each field requires a `Default` whose
    /// values match spec §10's documented defaults (not Rust's zero-values, which
    /// would give `temperature = 0.0` and empty budgets).
    /// What: returns the spec §10 defaults — `provider = "auto"`, the
    /// Sonnet/Haiku tier ids, no fallback, `temperature = 0.3`, and the documented
    /// token budgets.
    /// Test: `sm_config_absent_is_all_defaults`, `sm_config_default_values`.
    fn default() -> Self {
        Self {
            provider: "auto".to_string(),
            sm_model: "anthropic/claude-sonnet-4-6".to_string(),
            summary_model: "anthropic/claude-haiku".to_string(),
            model: String::new(),
            fallback: Vec::new(),
            temperature: 0.3,
            context_token_budget: 24_000,
            compaction_model: String::new(),
            compressed_context_max_tokens: 4_000,
        }
    }
}

/// `[session_manager.memory]` — dedicated SM palace + recall (spec §8).
///
/// Why: the SM keeps durable cross-session knowledge in its own memory palace,
/// separate from per-project palaces, and injects the top-k recall hits into the
/// working prompt. SM-4 creates the palace idempotently and reads these settings.
/// What: the `palace` name and the number of recall hits (`recall_top_k`) to
/// inject into the working context per request.
/// Test: `sm_config_full_parsed`, `sm_config_default_values`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmMemoryConfig {
    /// Dedicated SM palace name. Default `"session-manager"`.
    pub palace: String,

    /// Number of recall hits injected into the working prompt. Default `6`.
    pub recall_top_k: u32,
}

impl Default for SmMemoryConfig {
    /// Why: spec §10 defaults are non-zero (`"session-manager"`, `6`), so the
    /// derived zero-value `Default` would be wrong.
    /// What: returns the spec §10 memory defaults.
    /// Test: `sm_config_absent_is_all_defaults`, `sm_config_default_values`.
    fn default() -> Self {
        Self {
            palace: "session-manager".to_string(),
            recall_top_k: 6,
        }
    }
}

/// `[session_manager.rounds]` — rolling verbatim context window (spec §7).
///
/// Why: the SM keeps the most recent N conversation rounds verbatim and compacts
/// everything older (§7.2). This section sizes that window. SM-5 reads it.
/// What: the `window` size — how many recent rounds stay verbatim before older
/// rounds are folded into the compressed context.
/// Test: `sm_config_full_parsed`, `sm_config_default_values`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SmRoundsConfig {
    /// Verbatim rolling-window size (number of recent rounds). Default `10`.
    pub window: u32,
}

impl Default for SmRoundsConfig {
    /// Why: spec §10 default is `10`, not the derived zero-value.
    /// What: returns the spec §10 rounds default.
    /// Test: `sm_config_absent_is_all_defaults`, `sm_config_default_values`.
    fn default() -> Self {
        Self { window: 10 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: prove a fully-specified `[session_manager]` block round-trips every
    /// field to the value written (no silent default-clobbering).
    /// What: parses a complete TOML section and asserts each nested field.
    /// Test: this is the test.
    #[test]
    fn sm_config_full_parsed() {
        let toml = r#"
enabled = true

[inference]
provider = "openrouter"
sm_model = "openrouter/anthropic/claude-sonnet-4-6"
summary_model = "openrouter/anthropic/claude-haiku"
model = "legacy/sonnet"
fallback = ["openrouter", "bedrock"]
temperature = 0.5
context_token_budget = 32000
compaction_model = "anthropic/claude-haiku"
compressed_context_max_tokens = 6000

[memory]
palace = "sm-palace"
recall_top_k = 9

[rounds]
window = 20
"#;
        let cfg: SessionManagerConfig = toml::from_str(toml).expect("full SM config parses");
        assert!(cfg.enabled);
        assert_eq!(cfg.inference.provider, "openrouter");
        assert_eq!(
            cfg.inference.sm_model,
            "openrouter/anthropic/claude-sonnet-4-6"
        );
        assert_eq!(
            cfg.inference.summary_model,
            "openrouter/anthropic/claude-haiku"
        );
        assert_eq!(cfg.inference.model, "legacy/sonnet");
        assert_eq!(cfg.inference.fallback, vec!["openrouter", "bedrock"]);
        assert_eq!(cfg.inference.temperature, 0.5);
        assert_eq!(cfg.inference.context_token_budget, 32_000);
        assert_eq!(cfg.inference.compaction_model, "anthropic/claude-haiku");
        assert_eq!(cfg.inference.compressed_context_max_tokens, 6_000);
        assert_eq!(cfg.memory.palace, "sm-palace");
        assert_eq!(cfg.memory.recall_top_k, 9);
        assert_eq!(cfg.rounds.window, 20);
    }

    /// Why: a partial `[session_manager]` (only a couple fields) must keep the
    /// written values and fill every unspecified field from spec §10 defaults.
    /// What: parses a sparse TOML section and asserts the set fields plus the
    /// defaulted ones.
    /// Test: this is the test.
    #[test]
    fn sm_config_partial_takes_defaults() {
        let toml = r#"
enabled = true

[inference]
temperature = 0.7
"#;
        let cfg: SessionManagerConfig = toml::from_str(toml).expect("partial SM config parses");
        // Explicitly set:
        assert!(cfg.enabled);
        assert_eq!(cfg.inference.temperature, 0.7);
        // Defaulted within an otherwise-touched subsection:
        assert_eq!(cfg.inference.provider, "auto");
        assert_eq!(cfg.inference.context_token_budget, 24_000);
        // Entirely-absent subsections default whole:
        assert_eq!(cfg.memory, SmMemoryConfig::default());
        assert_eq!(cfg.rounds, SmRoundsConfig::default());
    }

    /// Why: an absent `[session_manager]` section (empty input) must yield the
    /// full default struct — the zero-regression contract for SM-1.
    /// What: parses empty TOML and asserts equality with `Default`.
    /// Test: this is the test.
    #[test]
    fn sm_config_absent_is_all_defaults() {
        let cfg: SessionManagerConfig = toml::from_str("").expect("empty input parses");
        assert_eq!(cfg, SessionManagerConfig::default());
    }

    /// Why: the headline no-op guarantee — `enabled` defaults to `false` so
    /// merely having the struct present changes nothing at runtime.
    /// What: asserts the default `enabled` value.
    /// Test: this is the test.
    #[test]
    fn sm_config_enabled_default_false() {
        assert!(!SessionManagerConfig::default().enabled);
    }

    /// Why: pin spec §10's documented non-zero defaults so a future careless
    /// `#[derive(Default)]` swap (which would zero them) fails loudly.
    /// What: asserts each defaulted field value.
    /// Test: this is the test.
    #[test]
    fn sm_config_default_values() {
        let cfg = SessionManagerConfig::default();
        assert_eq!(cfg.inference.provider, "auto");
        assert_eq!(cfg.inference.sm_model, "anthropic/claude-sonnet-4-6");
        assert_eq!(cfg.inference.summary_model, "anthropic/claude-haiku");
        assert!(cfg.inference.model.is_empty());
        assert!(cfg.inference.fallback.is_empty());
        assert_eq!(cfg.inference.temperature, 0.3);
        assert_eq!(cfg.inference.context_token_budget, 24_000);
        assert!(cfg.inference.compaction_model.is_empty());
        assert_eq!(cfg.inference.compressed_context_max_tokens, 4_000);
        assert_eq!(cfg.memory.palace, "session-manager");
        assert_eq!(cfg.memory.recall_top_k, 6);
        assert_eq!(cfg.rounds.window, 10);
    }
}
