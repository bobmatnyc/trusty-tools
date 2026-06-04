//! Per-role model configuration and resolution.
//!
//! Why: the spec (REV-311 / REV-313) requires three independently-selectable
//! LLM roles (reviewer, verifier, summarizer) with a four-level precedence
//! chain.  Keeping this logic in its own file keeps `config/mod.rs` under the
//! 500-line cap and makes the resolution rules easy to find and test.
//! What: `RoleModels`, `RoleConfig`, `RoleCliOverrides`, `RoleEnv`,
//! `FileModels`, `RoleConfigOverride`, and the private `resolve_role` helper.
//! Test: unit tests live in `config/mod.rs` (they access `role_models` from
//! the same module tree); resolution unit tests are `role_models_precedence_*`.

use serde::{Deserialize, Serialize};
use tracing::warn;

use super::Provider;

// ─── Per-role config ──────────────────────────────────────────────────────────

/// Per-role LLM configuration resolved for one role.
///
/// Why: the spec (REV-311) requires each role (reviewer / verifier /
/// summarizer) to be independently model-selectable, even across providers.
/// This struct captures the fully-resolved config for one role.
/// What: holds provider backend, model id, temperature, and max_tokens.
/// Test: resolution logic is tested in `config::tests`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleConfig {
    /// Provider backend for this role.
    pub provider: Provider,
    /// Model identifier (OpenRouter slug or Bedrock inference-profile id).
    pub model: String,
    /// Sampling temperature (spec REV-310 defaults: 0.3 reviewer, 1.0
    /// verifier, 0.0 summarizer).
    pub temperature: f32,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
}

/// TOML-deserialisable config for a single role (all fields optional so
/// partial overrides work).
///
/// Why: the TOML `[models.reviewer]` table may specify any subset of fields
/// and the rest fall back to the role's built-in defaults.
/// What: an optional-field mirror of `RoleConfig` used only during
/// config-file parsing.
/// Test: covered by integration with `ReviewConfig::from_env_and_file`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RoleConfigOverride {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

/// All three per-role model configurations, fully resolved.
///
/// Why: the pipeline and summarizer receive a single `RoleModels` value that
/// already encodes provider+model+temperature for each role, eliminating any
/// runtime config lookup during a review pass.
/// What: holds resolved `RoleConfig` for each of the three roles.
/// Test: `role_models_precedence_cli_wins`, `role_models_precedence_env_wins`,
/// `role_models_precedence_config_file_wins`, `role_models_precedence_defaults`.
#[derive(Debug, Clone)]
pub struct RoleModels {
    /// Reviewer: the main LLM pass that produces the review body.
    pub reviewer: RoleConfig,
    /// Verifier: per-finding verification round (must be foundation-lifecycle
    /// ACTIVE per spec REV-340).
    pub verifier: RoleConfig,
    /// Summarizer: diff Stage-C classification.
    pub summarizer: RoleConfig,
}

impl RoleModels {
    /// Resolve `RoleModels` from the four-level precedence chain.
    ///
    /// Why: spec REV-313 defines: CLI flag → per-role env var → config file
    /// → built-in default.  This constructor implements that chain for all
    /// three roles simultaneously.
    /// What: each role's fields are resolved independently.  `cli_overrides`
    /// carries any values the caller parsed from `--reviewer-model` etc.
    /// `file_models` carries parsed TOML table values.  Both may be `None` to
    /// skip that layer.
    /// Test: unit tests in `config::tests` cover all four precedence levels.
    pub fn resolve(
        cli_overrides: Option<&RoleCliOverrides>,
        env: &RoleEnv,
        file_models: Option<&FileModels>,
    ) -> Self {
        let reviewer = resolve_role(
            cli_overrides.and_then(|c| c.reviewer_model.as_deref()),
            cli_overrides.and_then(|c| c.provider.as_deref()),
            env.reviewer_model.as_deref(),
            env.provider.as_deref(),
            file_models.and_then(|f| f.reviewer.as_ref()),
            crate::llm::models::DEFAULT_REVIEWER_MODEL,
            Provider::Bedrock,
            0.3,
            4096,
        );
        let verifier = resolve_role(
            cli_overrides.and_then(|c| c.verifier_model.as_deref()),
            cli_overrides.and_then(|c| c.provider.as_deref()),
            env.verifier_model.as_deref(),
            env.provider.as_deref(),
            file_models.and_then(|f| f.verifier.as_ref()),
            crate::llm::models::DEFAULT_VERIFIER_MODEL,
            Provider::Bedrock,
            1.0,
            128,
        );
        let summarizer = resolve_role(
            cli_overrides.and_then(|c| c.summarizer_model.as_deref()),
            cli_overrides.and_then(|c| c.provider.as_deref()),
            env.summarizer_model.as_deref(),
            env.provider.as_deref(),
            file_models.and_then(|f| f.summarizer.as_ref()),
            crate::llm::models::DEFAULT_SUMMARIZER_MODEL,
            Provider::Bedrock,
            0.0,
            4096,
        );
        Self {
            reviewer,
            verifier,
            summarizer,
        }
    }

    /// Convenience: resolve with only env vars (no CLI, no config file).
    ///
    /// Why: tests and simple callers don't always want to construct all layers.
    /// What: calls `resolve` with `cli_overrides = None` and `file_models = None`.
    /// Test: used by unit tests in `config::tests`.
    pub fn from_env(env: &RoleEnv) -> Self {
        Self::resolve(None, env, None)
    }
}

/// CLI-provided per-role model overrides (all optional).
///
/// Why: spec REV-312 requires CLI flags to be the highest-precedence override
/// for model selection.
/// What: a plain struct carried from the CLI argument parser into
/// `RoleModels::resolve`.
/// Test: covered by `role_models_precedence_cli_wins`.
#[derive(Debug, Default, Clone)]
pub struct RoleCliOverrides {
    /// `--reviewer-model <id>`.
    pub reviewer_model: Option<String>,
    /// `--verifier-model <id>`.
    pub verifier_model: Option<String>,
    /// `--summarizer-model <id>`.
    pub summarizer_model: Option<String>,
    /// `--provider openrouter|bedrock`.
    pub provider: Option<String>,
}

/// Per-role model ids read from environment variables.
///
/// Why: spec REV-313 / doc 06 §LLM defines `TRUSTY_REVIEW_REVIEWER_MODEL`,
/// `TRUSTY_REVIEW_VERIFIER_MODEL`, `TRUSTY_REVIEW_SUMMARIZER_MODEL`, and
/// `TRUSTY_REVIEW_PROVIDER` as the second precedence layer.
/// What: struct populated from `std::env` inside `ReviewConfig::from_env_and_file`.
/// Test: `role_models_precedence_env_wins`.
#[derive(Debug, Default, Clone)]
pub struct RoleEnv {
    pub reviewer_model: Option<String>,
    pub verifier_model: Option<String>,
    pub summarizer_model: Option<String>,
    pub provider: Option<String>,
}

impl RoleEnv {
    /// Load role-model env vars.
    ///
    /// Why: encapsulates all env-var reads for role models so callers don't
    /// need to spell out variable names.
    /// What: reads `TRUSTY_REVIEW_REVIEWER_MODEL`, `TRUSTY_REVIEW_VERIFIER_MODEL`,
    /// `TRUSTY_REVIEW_SUMMARIZER_MODEL`, `TRUSTY_REVIEW_PROVIDER` from the
    /// process environment.
    /// Test: `role_models_precedence_env_wins` sets these vars and asserts.
    pub fn from_env() -> Self {
        Self {
            reviewer_model: std::env::var("TRUSTY_REVIEW_REVIEWER_MODEL").ok(),
            verifier_model: std::env::var("TRUSTY_REVIEW_VERIFIER_MODEL").ok(),
            summarizer_model: std::env::var("TRUSTY_REVIEW_SUMMARIZER_MODEL").ok(),
            provider: std::env::var("TRUSTY_REVIEW_PROVIDER").ok(),
        }
    }
}

/// TOML `[models]` table.
///
/// Why: config-file parsing needs a struct to accept partial override tables.
/// What: each field is an optional `RoleConfigOverride`.
/// Test: covered by `ReviewConfig::from_env_and_file`.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct FileModels {
    pub reviewer: Option<RoleConfigOverride>,
    pub verifier: Option<RoleConfigOverride>,
    pub summarizer: Option<RoleConfigOverride>,
}

// ─── Resolution helper (private) ─────────────────────────────────────────────

/// Resolve a single role's full config from the four-level precedence chain.
///
/// Why: avoids copy-pasting the same precedence logic three times.
/// What: each parameter is one precedence level; the first `Some` wins.
/// Test: covered indirectly by `RoleModels` unit tests.
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_role(
    cli_model: Option<&str>,
    cli_provider: Option<&str>,
    env_model: Option<&str>,
    env_provider: Option<&str>,
    file: Option<&RoleConfigOverride>,
    default_model: &str,
    default_provider: Provider,
    default_temp: f32,
    default_max_tokens: u32,
) -> RoleConfig {
    // Model: CLI → env → config file → built-in default.
    let model = cli_model
        .or(env_model)
        .or(file.and_then(|f| f.model.as_deref()))
        .unwrap_or(default_model)
        .to_string();

    // Provider: CLI → env → config file → built-in default.
    let provider_str = cli_provider.or(env_provider);
    let provider = provider_str
        .and_then(|s| {
            s.parse::<Provider>()
                .map_err(|e| {
                    warn!("unrecognised provider {s:?}: {e} — using default");
                })
                .ok()
        })
        .or_else(|| {
            file.and_then(|f| {
                f.provider.as_deref().and_then(|s| {
                    s.parse::<Provider>()
                        .map_err(|e| {
                            warn!("config file provider {s:?}: {e} — using default");
                        })
                        .ok()
                })
            })
        })
        .unwrap_or(default_provider);

    // Temperature: config file → built-in default.
    let temperature = file.and_then(|f| f.temperature).unwrap_or(default_temp);

    // max_tokens: config file → built-in default.
    let max_tokens = file
        .and_then(|f| f.max_tokens)
        .unwrap_or(default_max_tokens);

    RoleConfig {
        provider,
        model,
        temperature,
        max_tokens,
    }
}
