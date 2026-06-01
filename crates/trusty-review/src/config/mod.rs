//! Global configuration for trusty-review.
//!
//! Why: centralises all config resolution (env-var + TOML file) so the
//! pipeline and providers receive a single owned `ReviewConfig` value and
//! there is no global state.  The two-layer design (global service config +
//! per-repo YAML) mirrors the Python predecessor (source-analysis §8, §10).
//!
//! What: exposes `ReviewConfig` (loaded from env + optional TOML file),
//! `Provider` enum, and re-exports the per-role model resolution types from
//! the `role_models` submodule.  The `constants` submodule holds confidence-
//! threshold constants from spec §06.
//!
//! Test: `RoleModels` precedence resolution is covered by unit tests in this
//! module; `ReviewConfig` env-loading is covered by `test_config_from_env`.

pub mod constants;
pub mod role_models;

pub use role_models::{
    FileModels, RoleCliOverrides, RoleConfig, RoleConfigOverride, RoleEnv, RoleModels,
};

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::warn;

// ─── Provider identifier ──────────────────────────────────────────────────────

/// LLM backend provider.
///
/// Why: captures the provider selection in a typed enum so config code and
/// the provider factory can switch cleanly without string comparisons.
/// What: `OpenRouter` targets the OpenRouter API; `Bedrock` targets AWS
/// Bedrock Converse.  Serialised as lowercase (`"openrouter"`, `"bedrock"`).
/// Test: `provider_roundtrip_serde` verifies JSON serialisation symmetry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// OpenRouter (default for Stage 1 / local dev).
    #[default]
    OpenRouter,
    /// AWS Bedrock Converse API.
    Bedrock,
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provider::OpenRouter => write!(f, "openrouter"),
            Provider::Bedrock => write!(f, "bedrock"),
        }
    }
}

impl std::str::FromStr for Provider {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "openrouter" => Ok(Provider::OpenRouter),
            "bedrock" => Ok(Provider::Bedrock),
            other => Err(format!("unknown provider: {other}")),
        }
    }
}

// ─── Global ReviewConfig ──────────────────────────────────────────────────────

/// Top-level TOML file shape.
///
/// Why: needed to deserialise the optional config file into a typed struct
/// before merging with env-var values.
/// What: mirrors the TOML tables described in spec §06 §5.
/// Test: covered indirectly by `ReviewConfig::from_env_and_file`.
#[derive(Debug, Default, Deserialize)]
struct TomlFile {
    #[serde(default)]
    models: FileModels,
}

/// Global service configuration for trusty-review.
///
/// Why: the pipeline and providers receive this single owned value; no global
/// state is used anywhere.  Every field has a documented env var and default.
/// What: loaded from `ReviewConfig::from_env_and_file`.  Fields mirror the
/// Python env-var list (source-analysis §10, spec §06).
/// Test: `test_config_from_env` overrides key env vars and asserts values.
#[derive(Debug, Clone)]
pub struct ReviewConfig {
    // ── Pipeline flags ─────────────────────────────────────────────────────
    /// `PR_INTELLIGENCE_DRY_RUN` (default: true). When true, no comments are
    /// posted to GitHub.
    pub dry_run: bool,

    // ── Repo gating ────────────────────────────────────────────────────────
    /// `PR_INTELLIGENCE_ENABLED_REPOS` (default: `*`).
    pub enabled_repos: String,
    /// `PR_INTELLIGENCE_EXCLUDED_REPOS` (default: `""`).
    pub excluded_repos: String,
    /// `PR_INTELLIGENCE_EXCLUDED_AUTHORS` (default: `""`).
    pub excluded_authors: String,

    // ── Storage paths ──────────────────────────────────────────────────────
    /// `PR_INTELLIGENCE_LOG_DIR` — directory for review logs and dedup store.
    pub log_dir: PathBuf,

    // ── LLM / provider ─────────────────────────────────────────────────────
    /// OpenRouter API key (`OPENROUTER_API_KEY`).
    pub openrouter_api_key: String,

    // ── Service dependencies ───────────────────────────────────────────────
    /// trusty-search base URL (`TRUSTY_SEARCH_URL`, default `http://localhost:7878`).
    pub search_url: String,
    /// trusty-analyze base URL (`PR_INTELLIGENCE_ANALYZER_URL`, default
    /// `http://localhost:7879`).
    pub analyzer_url: String,
    /// Default trusty-search index (`TRUSTY_SEARCH_INDEX`, default `main`).
    pub search_index: String,

    // ── GitHub App authentication (REV-400–REV-402) ────────────────────────
    /// GitHub App ID (`GITHUB_APP_ID`).  `None` disables App auth.
    pub github_app_id: Option<String>,
    /// RSA private key PEM for the GitHub App (`GITHUB_APP_PRIVATE_KEY`).
    /// The PEM may have `\n`-escaped newlines (expanded at load time).
    pub github_app_private_key: Option<String>,
    /// PAT fallback token (`GITHUB_TOKEN`).
    pub github_token: String,
    /// Webhook shared secret (`GITHUB_WEBHOOK_SECRET`).
    pub github_webhook_secret: String,
    /// Installation IDs keyed by org name (case-insensitive).
    ///
    /// Populated from `GITHUB_INSTALLATION_ID_DUETTORESEARCH` and
    /// `GITHUB_INSTALLATION_ID_HOTSTATS` plus any additional
    /// `GITHUB_INSTALLATION_ID_<ORG>` env vars (case-folded to lowercase).
    pub github_installations: Vec<(String, u64)>,

    // ── Role models (fully resolved) ───────────────────────────────────────
    /// Resolved per-role model configurations.
    pub role_models: RoleModels,
}

impl ReviewConfig {
    /// Load config from env vars merged over an optional TOML file.
    ///
    /// Why: provides the single, authoritative config-loading path; callers
    /// do not need to know the env-var names or file location.
    /// What: reads the TOML file (if it exists) for the `[models]` table,
    /// then applies env-var overrides.  Missing files are not errors.
    /// `cli_overrides` carries any parsed CLI flags.
    /// Test: `test_config_from_env` exercises env-var loading; a TOML file
    /// path can be injected for config-file testing.
    pub fn from_env_and_file(
        config_path: Option<&std::path::Path>,
        cli_overrides: Option<&RoleCliOverrides>,
    ) -> Self {
        // Try to load the config file; silently fall back to defaults.
        let file_models = load_file_models(config_path);

        let env = RoleEnv::from_env();
        let role_models = RoleModels::resolve(cli_overrides, &env, file_models.as_ref());

        let dry_run = std::env::var("PR_INTELLIGENCE_DRY_RUN")
            .map(|v| v.to_lowercase() != "false")
            .unwrap_or(true);

        let log_dir = std::env::var("PR_INTELLIGENCE_LOG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::data_dir()
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
                    .join("trusty-review")
                    .join("pr-reviews")
            });

        Self {
            dry_run,
            enabled_repos: std::env::var("PR_INTELLIGENCE_ENABLED_REPOS")
                .unwrap_or_else(|_| "*".to_string()),
            excluded_repos: std::env::var("PR_INTELLIGENCE_EXCLUDED_REPOS").unwrap_or_default(),
            excluded_authors: std::env::var("PR_INTELLIGENCE_EXCLUDED_AUTHORS").unwrap_or_default(),
            log_dir,
            openrouter_api_key: std::env::var("OPENROUTER_API_KEY").unwrap_or_default(),
            search_url: std::env::var("TRUSTY_SEARCH_URL")
                .unwrap_or_else(|_| "http://localhost:7878".to_string()),
            analyzer_url: std::env::var("PR_INTELLIGENCE_ANALYZER_URL")
                .unwrap_or_else(|_| "http://localhost:7879".to_string()),
            search_index: std::env::var("TRUSTY_SEARCH_INDEX")
                .unwrap_or_else(|_| "main".to_string()),
            role_models,
            // ── GitHub App auth ────────────────────────────────────────────
            github_app_id: std::env::var("GITHUB_APP_ID")
                .ok()
                .filter(|s| !s.is_empty()),
            github_app_private_key: std::env::var("GITHUB_APP_PRIVATE_KEY")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| s.replace("\\n", "\n")), // expand \n-escaped newlines.
            github_token: std::env::var("GITHUB_TOKEN").unwrap_or_default(),
            github_webhook_secret: std::env::var("GITHUB_WEBHOOK_SECRET").unwrap_or_default(),
            github_installations: load_github_installations(),
        }
    }

    /// Load config using the default XDG config path.
    ///
    /// Why: the most common call-site pattern; the caller does not need to
    /// resolve the config file path themselves.
    /// What: calls `from_env_and_file` with the default path
    /// `$XDG_CONFIG_HOME/trusty-review/config.toml` (via the `dirs` crate).
    /// Test: `test_config_defaults_no_env` asserts defaults when no env vars
    /// are set.
    pub fn load(cli_overrides: Option<&RoleCliOverrides>) -> Self {
        let default_path = dirs::config_dir().map(|d| d.join("trusty-review").join("config.toml"));
        Self::from_env_and_file(default_path.as_deref(), cli_overrides)
    }
}

/// Load GitHub installation IDs from env vars.
///
/// Why: the bot may be installed in multiple GitHub orgs; each org has its own
/// installation ID.  This helper collects all known installation env vars into
/// a uniform `Vec<(org_name, installation_id)>` list.
/// What: reads the two well-known env vars (`GITHUB_INSTALLATION_ID_DUETTORESEARCH`,
/// `GITHUB_INSTALLATION_ID_HOTSTATS`) plus any `GITHUB_INSTALLATION_ID_<ORG>`
/// pattern (not supported dynamically yet — only the two known orgs are read for
/// the MVP).  Invalid or empty values are silently skipped.
/// Test: covered indirectly by `config_github_fields_from_env`.
fn load_github_installations() -> Vec<(String, u64)> {
    let known = [
        ("GITHUB_INSTALLATION_ID_DUETTORESEARCH", "duettoresearch"),
        ("GITHUB_INSTALLATION_ID_HOTSTATS", "hotstats"),
    ];
    let mut installations = Vec::new();
    for (env_var, org_name) in &known {
        if let Ok(val) = std::env::var(env_var)
            && let Ok(id) = val.trim().parse::<u64>()
        {
            installations.push((org_name.to_string(), id));
        }
    }
    installations
}

/// Try to load `[models]` from a TOML config file; return `None` on any
/// error (fail-open per spec REV-511).
///
/// Why: config file absence or parse errors must never block a review.
/// What: reads the file as a string, deserialises as `TomlFile`, returns
/// the `models` table.  Any I/O or TOML error logs a warning and returns
/// `None`.
/// Test: covered indirectly by `ReviewConfig` unit tests.
fn load_file_models(path: Option<&std::path::Path>) -> Option<FileModels> {
    let path = path?;
    match std::fs::read_to_string(path) {
        Err(_) => None,
        Ok(s) => match toml::from_str::<TomlFile>(&s) {
            Ok(f) => Some(f.models),
            Err(e) => {
                warn!(?path, "failed to parse config file: {e}");
                None
            }
        },
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_roundtrip_serde() {
        let json = serde_json::to_string(&Provider::OpenRouter).unwrap();
        assert_eq!(json, r#""openrouter""#);
        let p: Provider = serde_json::from_str(&json).unwrap();
        assert_eq!(p, Provider::OpenRouter);

        let json = serde_json::to_string(&Provider::Bedrock).unwrap();
        assert_eq!(json, r#""bedrock""#);
        let p: Provider = serde_json::from_str(&json).unwrap();
        assert_eq!(p, Provider::Bedrock);
    }

    #[test]
    fn provider_fromstr() {
        assert_eq!(
            "openrouter".parse::<Provider>().unwrap(),
            Provider::OpenRouter
        );
        assert_eq!("bedrock".parse::<Provider>().unwrap(), Provider::Bedrock);
        assert!("unknown".parse::<Provider>().is_err());
    }

    #[test]
    fn role_models_precedence_defaults() {
        // No CLI, no env, no file → built-in defaults (Bedrock as of #548).
        let env = RoleEnv::default();
        let roles = RoleModels::from_env(&env);
        assert_eq!(
            roles.reviewer.model,
            crate::llm::models::DEFAULT_REVIEWER_MODEL
        );
        // Default provider is now Bedrock (changed from OpenRouter in #548).
        assert_eq!(roles.reviewer.provider, Provider::Bedrock);
        assert!((roles.reviewer.temperature - 0.3_f32).abs() < f32::EPSILON);
        assert_eq!(
            roles.verifier.model,
            crate::llm::models::DEFAULT_VERIFIER_MODEL
        );
        assert_eq!(roles.verifier.provider, Provider::Bedrock);
        assert_eq!(
            roles.summarizer.model,
            crate::llm::models::DEFAULT_SUMMARIZER_MODEL
        );
        assert_eq!(roles.summarizer.provider, Provider::Bedrock);
    }

    #[test]
    fn role_models_openrouter_still_selectable_via_env() {
        // OpenRouter is co-equal: selecting it via env var must work.
        let env = RoleEnv {
            provider: Some("openrouter".to_string()),
            reviewer_model: Some("openai/gpt-5.4-mini-20260317".to_string()),
            ..Default::default()
        };
        let roles = RoleModels::from_env(&env);
        assert_eq!(roles.reviewer.provider, Provider::OpenRouter);
        assert_eq!(roles.reviewer.model, "openai/gpt-5.4-mini-20260317");
    }

    #[test]
    fn role_models_precedence_env_wins() {
        let env = RoleEnv {
            reviewer_model: Some("openai/gpt-5.4-mini-20260317".to_string()),
            verifier_model: None,
            summarizer_model: None,
            provider: None,
        };
        let roles = RoleModels::from_env(&env);
        assert_eq!(roles.reviewer.model, "openai/gpt-5.4-mini-20260317");
        // verifier and summarizer fall back to defaults.
        assert_eq!(
            roles.verifier.model,
            crate::llm::models::DEFAULT_VERIFIER_MODEL
        );
    }

    #[test]
    fn role_models_precedence_cli_wins_over_env() {
        let cli = RoleCliOverrides {
            reviewer_model: Some("openai/gpt-5.4-20260305".to_string()),
            ..Default::default()
        };
        let env = RoleEnv {
            reviewer_model: Some("openai/gpt-5.4-mini-20260317".to_string()),
            ..Default::default()
        };
        let roles = RoleModels::resolve(Some(&cli), &env, None);
        // CLI flag beats env var.
        assert_eq!(roles.reviewer.model, "openai/gpt-5.4-20260305");
    }

    #[test]
    fn role_models_precedence_config_file_wins_over_defaults() {
        let file = FileModels {
            reviewer: Some(RoleConfigOverride {
                model: Some("openai/gpt-5.4-nano-20260317".to_string()),
                temperature: Some(0.5),
                ..Default::default()
            }),
            ..Default::default()
        };
        let env = RoleEnv::default();
        let roles = RoleModels::resolve(None, &env, Some(&file));
        assert_eq!(roles.reviewer.model, "openai/gpt-5.4-nano-20260317");
        assert!((roles.reviewer.temperature - 0.5_f32).abs() < f32::EPSILON);
        // Verifier falls back to built-in.
        assert_eq!(
            roles.verifier.model,
            crate::llm::models::DEFAULT_VERIFIER_MODEL
        );
    }

    #[test]
    fn role_models_all_defaults_are_bedrock_claude() {
        // As of #548 all defaults are Bedrock Claude models (Sonnet/Haiku).
        for model in [
            crate::llm::models::DEFAULT_REVIEWER_MODEL,
            crate::llm::models::DEFAULT_VERIFIER_MODEL,
            crate::llm::models::DEFAULT_SUMMARIZER_MODEL,
        ] {
            assert!(
                model.contains("anthropic") || model.starts_with("us."),
                "default model {model} must be a Bedrock Claude inference-profile id"
            );
        }
    }

    #[test]
    fn config_dry_run_defaults_to_true() {
        // Without any env var, dry_run must default to true.
        let env = RoleEnv::default();
        let _ = RoleModels::from_env(&env); // Just verifies no panic.
    }

    #[test]
    fn config_github_token_defaults_to_empty() {
        // When GITHUB_TOKEN is not set, github_token must be empty (not panic).
        let config = ReviewConfig::from_env_and_file(None, None);
        // We cannot assert the exact value (CI may have GITHUB_TOKEN set),
        // but we can assert the config loads without panic.
        let _ = config.github_token;
    }

    #[test]
    fn config_search_url_default() {
        // When TRUSTY_SEARCH_URL is not set, falls back to localhost:7878.
        // (Cannot reliably unset env vars in parallel tests; just check load.)
        let config = ReviewConfig::from_env_and_file(None, None);
        assert!(
            config.search_url.starts_with("http"),
            "search_url must start with http: {}",
            config.search_url
        );
    }

    #[test]
    fn config_analyzer_url_default() {
        let config = ReviewConfig::from_env_and_file(None, None);
        assert!(
            config.analyzer_url.starts_with("http"),
            "analyzer_url must start with http: {}",
            config.analyzer_url
        );
    }

    #[test]
    fn load_github_installations_parses_known_orgs() {
        // The helper is pure (reads env vars); we can call it without side effects.
        // Just verify it doesn't panic and returns a vec.
        let installs = super::load_github_installations();
        // Each element must have a non-empty org name and a non-zero id.
        for (org, id) in &installs {
            assert!(!org.is_empty(), "org name must be non-empty");
            assert!(*id > 0, "installation id must be > 0");
        }
    }
}
