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

    // ── Trigger classification (Phase 1, #582 / REV-703) ───────────────────
    /// Bot username whose `review_requested` triggers a live (posted) review.
    ///
    /// `PR_REVIEW_BOT_USERNAME` (default: `"trusty-review[bot]"`).  When this
    /// login is the requested reviewer, the review is forced live.
    pub bot_username: String,
    /// Additional reviewer logins (case-insensitive) that force a live review.
    ///
    /// `PR_INTELLIGENCE_LIVE_REVIEW_REQUESTERS` — comma-separated list.  When
    /// any of these logins requests the review, it is forced live; all other
    /// reviewers force a dry-run (REV-703).
    pub live_review_requesters: Vec<String>,

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
            bot_username: std::env::var("PR_REVIEW_BOT_USERNAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "trusty-review[bot]".to_string()),
            live_review_requesters: load_live_review_requesters(),
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

/// Load the live-review-requester allowlist from the environment.
///
/// Why: REV-703 lets specific reviewer logins (beyond the bot itself) force a
/// live review; this parses that allowlist from a comma-separated env var.
/// What: reads `PR_INTELLIGENCE_LIVE_REVIEW_REQUESTERS`, splits on commas,
/// trims, lowercases (logins are compared case-insensitively), and drops empty
/// entries.  Absent var → empty list.
/// Test: covered by `live_review_requesters_parses_csv`.
fn load_live_review_requesters() -> Vec<String> {
    std::env::var("PR_INTELLIGENCE_LIVE_REVIEW_REQUESTERS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
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
#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
