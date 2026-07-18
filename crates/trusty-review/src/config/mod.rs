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
pub mod context;
pub mod index_resolver;
pub mod mapreduce;
pub mod role_models;
pub mod verification;
// Why: voice configuration loading extracted to keep config/mod.rs under the
// 500-line cap (#610) after adding voice support (#754/#756).
pub mod voice;
// Why: outcome-polling configuration extracted to keep config/mod.rs under the
// 500-line cap — mirrors the verification module pattern.
pub mod outcome;

pub use index_resolver::{
    SourceRootResolution, canonical_source_root, find_git_root, repo_root_from_cwd,
    resolve_index_from_list, resolve_source_root,
};
pub use mapreduce::{DiffStats, MapMode, MapReduceConfig, ReviewPath, select_review_mode};

pub use context::{ContextConfig, ContextFileConfig};
pub use outcome::{OutcomeConfig, OutcomeFileConfig};
pub use role_models::{
    FileModels, RoleCliOverrides, RoleConfig, RoleConfigOverride, RoleEnv, RoleModels,
};
pub use verification::{VerificationConfig, VerificationFileConfig};

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::warn;

// ─── Provider identifier ──────────────────────────────────────────────────────

/// LLM backend provider.
///
/// Why: captures the provider selection in a typed enum so config code and
/// the provider factory can switch cleanly without string comparisons.
/// What: `OpenRouter` targets the OpenRouter API; `Bedrock` targets AWS
/// Bedrock Converse; `Fireworks` targets the Fireworks.ai OpenAI-compatible
/// API.  Serialised as lowercase (`"openrouter"`, `"bedrock"`, `"fireworks"`).
/// Test: `provider_roundtrip_serde` verifies JSON serialisation symmetry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// OpenRouter (default for Stage 1 / local dev).
    #[default]
    OpenRouter,
    /// AWS Bedrock Converse API.
    Bedrock,
    /// Fireworks.ai OpenAI-compatible API.
    Fireworks,
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provider::OpenRouter => write!(f, "openrouter"),
            Provider::Bedrock => write!(f, "bedrock"),
            Provider::Fireworks => write!(f, "fireworks"),
        }
    }
}

impl std::str::FromStr for Provider {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "openrouter" => Ok(Provider::OpenRouter),
            "bedrock" => Ok(Provider::Bedrock),
            "fireworks" => Ok(Provider::Fireworks),
            other => Err(format!("unknown provider: {other}")),
        }
    }
}

// ─── --source-root outcome (#2994) ────────────────────────────────────────────

/// Outcome of `ReviewConfig::resolve_source_root`.
///
/// Why: the caller (`cmd_run`/`cmd_compare`) needs to know whether an explicit
/// `--source-root <dir>` resolved to a real index (nothing further to do) or
/// forced a diff-only fallback that also requires swapping in a
/// `NullSearchClient` and surfacing the notice to the operator.
/// What: `Matched` carries the resolved index id (already applied to
/// `search_index` by the time this is returned — informational only);
/// `DiffOnly` carries the human-readable notice to print/log.
/// Test: `source_root_matches_registered_index`,
/// `source_root_no_match_forces_diff_only`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceRootOutcome {
    /// `--source-root` mapped to this registered index id.
    Matched(String),
    /// No registered index matched (or the daemon was unreachable) — diff-only
    /// review with this notice.
    DiffOnly(String),
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
    #[serde(default)]
    verification: VerificationFileConfig,
    #[serde(default)]
    context: ContextFileConfig,
    #[serde(default)]
    voice: voice::VoiceFileConfig,
    #[serde(default)]
    coverage: crate::coverage::CoverageFileConfig,
    #[serde(default)]
    outcome: outcome::OutcomeFileConfig,
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
    /// Fireworks.ai API key (`FIREWORKS_API_KEY`).
    ///
    /// The env var name matches trusty-common's `env_var_for("fireworks")`
    /// convention, so a key exported for tcode works here unchanged.
    pub fireworks_api_key: String,

    // ── Service dependencies ───────────────────────────────────────────────
    /// trusty-search base URL (`TRUSTY_SEARCH_URL`, default `http://localhost:7878`).
    pub search_url: String,
    /// trusty-analyze base URL (`PR_INTELLIGENCE_ANALYZER_URL`, default
    /// `http://localhost:7879`).
    pub analyzer_url: String,
    /// Default trusty-search index (`TRUSTY_SEARCH_INDEX`, default `main`).
    ///
    /// When `TRUSTY_SEARCH_INDEX` is not set, this starts as `"main"` and is
    /// overwritten by `ReviewConfig::resolve_index` at startup (issue #661).
    pub search_index: String,

    /// True when `TRUSTY_SEARCH_INDEX` was explicitly set by the operator.
    ///
    /// Why: auto-derivation must respect explicit configuration; checking
    /// whether the env var was present is cheaper than re-reading it later.
    /// What: set to `true` only when `std::env::var("TRUSTY_SEARCH_INDEX")` is
    /// `Ok(_)` at config-load time.  Used by `resolve_index` to skip derivation.
    /// Test: covered by the `resolve_index` unit tests in `config_tests.rs`.
    pub search_index_explicit: bool,

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

    // ── Verification round (Phase 2, #583) ─────────────────────────────────
    /// Resolved verification-round settings (`enabled`, `liveness_check`).
    pub verification: VerificationConfig,

    // ── Required-context gate (#590) ───────────────────────────────────────
    /// Resolved required-context settings (`require_search`, `require_analyze`).
    /// Both default to `true`: a missing dependency skips the review rather than
    /// degrading to a context-free verdict.
    pub context: ContextConfig,

    // ── External context sources (Phase 6, #550) ───────────────────────────
    /// Resolved per-source settings for the external enrichment sources
    /// (JIRA / Confluence / GitHub Issues; APEX in PR-B).  Each source defaults
    /// to disabled unless its credentials are present (auto-enable) — so the
    /// crate works out of the box with no Atlassian/GitHub-context config.  These
    /// sources are best-effort / fail-open, distinct from the REQUIRED gate above.
    pub context_sources: crate::integrations::context::ContextSourcesConfig,

    // ── APEX / KB indexed context (Phase 6 PR-B, REV-420, #550) ───────────
    /// trusty-search index id that contains the APEX/KB corpus.
    ///
    /// `TRUSTY_SEARCH_APEX_INDEX` (default: `""`).  Empty string ⇒ APEX disabled
    /// (fail-open no-op).  For Option A this is typically the SAME index id as
    /// `search_index`; the APEX hits are distinguished by `apex_path_prefixes`.
    pub apex_index: String,

    /// Corpus-relative path prefixes that mark a search hit as an APEX/KB doc.
    ///
    /// `TRUSTY_REVIEW_APEX_PATH_PREFIXES` — comma-separated list (e.g.
    /// `apex/,specs/,docs/adr/`).  Empty ⇒ treat ALL hits from `apex_index` as
    /// APEX (no prefix filtering).  Non-empty ⇒ retain only hits whose `file`
    /// starts with one of the prefixes.
    pub apex_path_prefixes: Vec<String>,

    // ── Voice package (Phase 3, #754 / #756) ──────────────────────────────
    /// Name of the voice package to load for the 3-layer prompt composition
    /// (stock → principles → voice).
    ///
    /// `TRUSTY_REVIEW_VOICE_PACKAGE` env var or `[voice] package = "duetto"` in
    /// the TOML config.  Empty/absent = no voice package (opt-in).
    pub voice_package: Option<String>,

    /// Whether the universal best-practices principles layer (#756) is enabled.
    ///
    /// `TRUSTY_REVIEW_PRINCIPLES=false` disables it; defaults to `true`
    /// (default-on per issue #756 spec: "universal/safe").
    /// The config file key is `[voice] principles = false`.
    pub voice_principles: bool,

    // ── Coverage gating (issue #1014) ─────────────────────────────────────
    /// Resolved coverage-gating policy.
    ///
    /// `TRUSTY_REVIEW_COVERAGE_ENABLED=true` enables coverage gating (off by
    /// default).  All fields configurable via env vars or `[coverage]` TOML table.
    /// When `enabled = false`, the coverage pipeline is a complete no-op.
    pub coverage: crate::coverage::CoveragePolicy,

    // ── Outcome polling (issue #1421) ─────────────────────────────────────
    /// Resolved outcome-polling settings (`enabled`, `poll_delay_minutes`,
    /// `dismissal_threshold`).  Defaults to disabled — opt-in via
    /// `TRUSTY_REVIEW_OUTCOME_ENABLED=true`.
    pub outcome: OutcomeConfig,
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
        // Try to load the config file; silently fall back to defaults.  Parse
        // it once so both the `[models]` and `[verification]` tables come from
        // the same read.
        let toml_file = load_toml_file(config_path);
        let file_models = toml_file.as_ref().map(|f| f.models.clone());
        let file_verification = toml_file.as_ref().map(|f| f.verification.clone());
        let file_context = toml_file.as_ref().map(|f| f.context.clone());
        let file_voice: Option<&voice::VoiceFileConfig> = toml_file.as_ref().map(|f| &f.voice);
        let file_coverage: Option<&crate::coverage::CoverageFileConfig> =
            toml_file.as_ref().map(|f| &f.coverage);
        let file_outcome: Option<&outcome::OutcomeFileConfig> =
            toml_file.as_ref().map(|f| &f.outcome);

        let env = RoleEnv::from_env();
        let role_models = RoleModels::resolve(cli_overrides, &env, file_models.as_ref());
        let verification = VerificationConfig::from_env_and_file(file_verification.as_ref());
        let context = ContextConfig::from_env_and_file(file_context.as_ref());
        let context_sources = crate::integrations::context::ContextSourcesConfig::from_env_and_file(
            file_context.as_ref().map(|c| &c.sources),
        );

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
            openrouter_api_key: std::env::var(trusty_common::env_vars::ENV_OPENROUTER_API_KEY)
                .unwrap_or_default(),
            fireworks_api_key: std::env::var("FIREWORKS_API_KEY").unwrap_or_default(),
            search_url: std::env::var("TRUSTY_SEARCH_URL")
                .unwrap_or_else(|_| "http://localhost:7878".to_string()),
            analyzer_url: std::env::var("PR_INTELLIGENCE_ANALYZER_URL")
                .unwrap_or_else(|_| "http://localhost:7879".to_string()),
            search_index: std::env::var("TRUSTY_SEARCH_INDEX")
                .unwrap_or_else(|_| "main".to_string()),
            search_index_explicit: std::env::var("TRUSTY_SEARCH_INDEX").is_ok(),
            role_models,
            // ── GitHub App auth ────────────────────────────────────────────
            github_app_id: std::env::var("GITHUB_APP_ID")
                .ok()
                .filter(|s| !s.is_empty()),
            github_app_private_key: std::env::var("GITHUB_APP_PRIVATE_KEY")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| s.replace("\\n", "\n")), // expand \n-escaped newlines.
            github_token: std::env::var(trusty_common::env_vars::ENV_GITHUB_TOKEN)
                .unwrap_or_default(),
            github_webhook_secret: std::env::var("GITHUB_WEBHOOK_SECRET").unwrap_or_default(),
            github_installations: load_github_installations(),
            bot_username: std::env::var("PR_REVIEW_BOT_USERNAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "trusty-review[bot]".to_string()),
            live_review_requesters: load_live_review_requesters(),
            verification,
            context,
            context_sources,
            apex_index: std::env::var("TRUSTY_SEARCH_APEX_INDEX").unwrap_or_default(),
            apex_path_prefixes: load_apex_path_prefixes(),
            voice_package: voice::load_voice_package(file_voice),
            voice_principles: voice::load_voice_principles(file_voice),
            coverage: crate::coverage::CoveragePolicy::from_env_and_file(file_coverage),
            outcome: OutcomeConfig::from_env_and_file(file_outcome),
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

    /// Resolve and cache the best trusty-search index for the current project.
    ///
    /// Why: user-level MCP wiring omits `TRUSTY_SEARCH_INDEX`; the default
    /// `"main"` is wrong for most projects.  This method queries the daemon and
    /// picks the index whose `root_path` best matches the current repo root
    /// (issue #661).  When `TRUSTY_SEARCH_INDEX` is explicitly set, this is a
    /// no-op so explicit configuration always wins.
    /// What: calls `SearchClient::list_indexes` on the configured daemon, runs
    /// `index_resolver::resolve_index_from_list`, and writes the result into
    /// `self.search_index`.  Failures degrade to a stderr warning; `search_index`
    /// is left unchanged (keeps the `"main"` default or the explicit value).
    /// Test: `resolve_index_noop_when_explicit`, `resolve_index_updates_when_unset`.
    pub async fn resolve_index(
        &mut self,
        client: &dyn crate::integrations::search_client::SearchClient,
    ) {
        if self.search_index_explicit {
            // Operator set TRUSTY_SEARCH_INDEX explicitly — honour it verbatim.
            return;
        }
        let repo_root = index_resolver::repo_root_from_cwd();
        match client.list_indexes().await {
            Ok(indexes) => {
                if let Some(id) = index_resolver::resolve_index_from_list(&indexes, &repo_root) {
                    tracing::info!(
                        index = %id,
                        repo_root = %repo_root.display(),
                        "trusty-review: auto-derived search index from repo root"
                    );
                    self.search_index = id;
                } else {
                    warn!(
                        repo_root = %repo_root.display(),
                        "trusty-review: could not auto-derive search index; \
                         using fallback \"main\". Set TRUSTY_SEARCH_INDEX to suppress."
                    );
                }
            }
            Err(e) => {
                warn!(
                    "trusty-review: index auto-derive failed (daemon unreachable?): {e}; \
                     using fallback \"main\""
                );
            }
        }
    }

    /// Resolve the trusty-search index for an explicit `--source-root <dir>`
    /// (issue #2994).
    ///
    /// Why: `resolve_index` (env / CWD auto-derive) cannot point a review at an
    /// arbitrary checkout without first running `trusty-search index <dir>`
    /// out of band. `--source-root` gives an ergonomic, explicit one-off path:
    /// if `dir` already maps to a registered index, use it (same
    /// longest-root-path matching as `resolve_index`, just against an
    /// explicit dir instead of the CWD). If it does not, this method chooses
    /// the SAFE fallback — diff-only review with a clear, actionable notice —
    /// rather than triggering an ephemeral index. An ephemeral/ad-hoc index
    /// was considered (per the #2994 proposal) but rejected for this pass:
    /// it interacts with the #2914 ephemeral-index-leak investigation, and
    /// reliable cleanup is not trivially safe from this crate alone, so
    /// diff-only-with-notice is the shipped default; ephemeral indexing
    /// remains a documented follow-up.
    /// What: canonicalises `dir`, lists indexes on `client`, and matches via
    /// `index_resolver::resolve_source_root`. On a match: sets `search_index`
    /// to the matched id and `search_index_explicit = true` (so `resolve_index`
    /// becomes a no-op and this choice is never clobbered by CWD auto-derive),
    /// returning `SourceRootOutcome::Matched`. On no match, or when the daemon
    /// is unreachable: sets `search_index_explicit = true` and
    /// `context.require_search = false` so the required-context gate
    /// (`pipeline::context_gate::preflight_context`) degrades instead of
    /// skipping, and returns `SourceRootOutcome::DiffOnly` with a notice the
    /// caller must surface (stderr/log) and use to swap `deps.search` for a
    /// `NullSearchClient` — relaxing `require_search` alone is not sufficient
    /// because a healthy daemon would otherwise silently be queried against
    /// whatever index `search_index` still holds.
    /// Test: `source_root_matches_registered_index`,
    /// `source_root_no_match_forces_diff_only`,
    /// `source_root_daemon_unreachable_forces_diff_only`.
    pub async fn resolve_source_root(
        &mut self,
        client: &dyn crate::integrations::search_client::SearchClient,
        dir: &std::path::Path,
    ) -> SourceRootOutcome {
        let canonical = index_resolver::canonical_source_root(dir);
        let indexes = match client.list_indexes().await {
            Ok(indexes) => indexes,
            Err(e) => {
                self.search_index_explicit = true;
                self.context.require_search = false;
                let notice = format!(
                    "--source-root {}: trusty-search unreachable while resolving an index \
                     ({e}) — proceeding in diff-only mode (no code-context retrieval)",
                    canonical.display()
                );
                warn!("{notice}");
                return SourceRootOutcome::DiffOnly(notice);
            }
        };

        match index_resolver::resolve_source_root(&indexes, &canonical) {
            SourceRootResolution::Matched(id) => {
                tracing::info!(
                    index = %id,
                    source_root = %canonical.display(),
                    "trusty-review: --source-root resolved to a registered index"
                );
                self.search_index = id.clone();
                self.search_index_explicit = true;
                SourceRootOutcome::Matched(id)
            }
            SourceRootResolution::NoIndex => {
                self.search_index_explicit = true;
                self.context.require_search = false;
                let notice = format!(
                    "--source-root {} has no registered trusty-search index — proceeding in \
                     diff-only mode (no code-context retrieval). Run `trusty-search index {}` \
                     to enable full context, or omit --source-root to use the \
                     auto-derived/TRUSTY_SEARCH_INDEX index.",
                    canonical.display(),
                    canonical.display()
                );
                warn!("{notice}");
                SourceRootOutcome::DiffOnly(notice)
            }
        }
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

/// Load the APEX/KB path-prefix filter list from the environment.
///
/// Why: REV-420 lets operators scope APEX hits to specific corpus sub-paths
/// (e.g. `apex/,specs/`) so that, in a mixed code+docs index, only the doc
/// segment is treated as APEX context.  This helper parses that list once.
/// What: reads `TRUSTY_REVIEW_APEX_PATH_PREFIXES`, splits on commas, trims, and
/// drops empty entries.  Absent var → empty list (no prefix filtering).
/// Test: covered by `apex_path_prefixes_parses_csv` in `config_tests.rs`.
fn load_apex_path_prefixes() -> Vec<String> {
    std::env::var("TRUSTY_REVIEW_APEX_PATH_PREFIXES")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Try to parse the whole TOML config file; return `None` on any error
/// (fail-open per spec REV-511).
///
/// Why: config-file absence or parse errors must never block a review.  Parsing
/// once and returning the whole `TomlFile` lets the caller pull both the
/// `[models]` and `[verification]` tables from a single read instead of opening
/// the file twice.
/// What: reads the file as a string and deserialises as `TomlFile`.  Any I/O or
/// TOML error logs a warning and returns `None`.
/// Test: covered indirectly by `ReviewConfig` unit tests.
fn load_toml_file(path: Option<&std::path::Path>) -> Option<TomlFile> {
    let path = path?;
    match std::fs::read_to_string(path) {
        Err(_) => None,
        Ok(s) => match toml::from_str::<TomlFile>(&s) {
            Ok(f) => Some(f),
            Err(e) => {
                warn!(?path, "failed to parse config file: {e}");
                None
            }
        },
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

/// resolve_index and wiring-path tests — split to keep config_tests.rs under
/// the 500-line cap (#610).
#[cfg(test)]
#[path = "config_resolve_index_tests.rs"]
mod resolve_index_tests;
