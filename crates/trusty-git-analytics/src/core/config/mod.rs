//! Configuration types deserialized from YAML.
//!
//! The full configuration schema is documented in
//! `docs/requirements/configuration.md`. This module implements the practical
//! subset needed by the pipeline; unknown YAML keys are ignored (forward
//! compatible) so newer config files can be loaded by older binaries without
//! a hard failure.
//!
//! Paths support tilde-expansion (`~`, `~/foo`) via [`expand_path`].
//!
//! # Example
//!
//! ```ignore
//! use std::path::Path;
//! use tga::core::config::Config;
//!
//! let cfg = Config::load(Path::new("config.yaml")).expect("load");
//! println!("repos: {}", cfg.repositories.len());
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::classify::taxonomy::SubcategoryDef;
use crate::core::errors::{Result, TgaError};

pub mod aliases;
pub mod azdo;
pub mod validator;

pub use aliases::{AliasFile, DeveloperAliasEntry};
pub use azdo::AzureDevOpsConfig;
pub use validator::{ConfigError, ConfigValidator};

/// Top-level configuration root.
///
/// Mirrors the YAML schema from the Python predecessor. All top-level
/// sections are optional except `repositories`, which must contain at
/// least one entry to be useful.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Repositories to analyze.
    #[serde(default)]
    pub repositories: Vec<RepositoryConfig>,

    /// Team / member roster and aliases.
    #[serde(default)]
    pub team: Option<TeamConfig>,

    /// Output destination and format flags.
    #[serde(default)]
    pub output: Option<OutputConfig>,

    /// Classification cascade settings.
    #[serde(default)]
    pub classification: Option<ClassificationConfig>,

    /// GitHub API credentials and scope.
    #[serde(default)]
    pub github: Option<GithubConfig>,

    /// Bitbucket Cloud API credentials and scope.
    #[serde(default)]
    pub bitbucket: Option<BitbucketConfig>,

    /// JIRA API credentials and scope.
    #[serde(default)]
    pub jira: Option<JiraConfig>,

    /// Linear integration settings.
    #[serde(default)]
    pub linear: Option<LinearConfig>,

    /// Project management integrations. Canonical location for ADO,
    /// and future PM tools.
    #[serde(default)]
    pub pm: Option<PmConfig>,

    /// DORA-metrics configuration: deployment ingestion sources, failure
    /// signals for change-failure-rate detection, and incident ingestion.
    /// See [`DoraConfig`].
    #[serde(default)]
    pub dora: Option<DoraConfig>,

    /// Tag and release-branch reachability scanner settings (issue #279).
    ///
    /// Controls whether `tga collect` populates the `on_any_tag`,
    /// `reachable_from_tags`, `on_release_branch`, and `release_branches`
    /// columns of `fact_commit_reachability`. When absent, defaults to
    /// tracking both tags and release branches with the standard patterns.
    #[serde(default)]
    pub reachability: ReachabilityConfig,

    /// Schema version string (e.g. `"1.0"`).
    ///
    /// Stored for forward compatibility with the Python predecessor's YAML
    /// format. Not enforced by the Rust loader — present so files written
    /// for the Python tool deserialize cleanly.
    #[serde(default)]
    pub version: Option<String>,

    /// Named profile (e.g. `"balanced"`).
    ///
    /// Stored for forward compatibility with the Python predecessor. Not
    /// currently consumed by the Rust pipeline.
    #[serde(default)]
    pub profile: Option<String>,

    /// Python-compatible flat alias map: canonical name → list of email
    /// addresses or login aliases.
    ///
    /// When non-empty, takes precedence over [`TeamConfig::members`] for
    /// identity resolution (see [`Config::resolved_aliases`]).
    #[serde(default)]
    pub developer_aliases: HashMap<String, Vec<String>>,

    /// Path to an external aliases file (YAML). If set, entries are merged
    /// with any inline [`Config::developer_aliases`]. The external file takes
    /// precedence for entries with the same canonical name.
    ///
    /// Supports `~` home-directory expansion. Relative paths are resolved
    /// against the directory of the loaded config file when known (passed
    /// to [`Config::resolved_alias_map`]) and otherwise against the current
    /// working directory.
    #[serde(default)]
    pub aliases_file: Option<String>,

    /// Analysis settings (ML categorization, etc.).
    ///
    /// Parsed for forward compatibility; individual sub-features gate their
    /// own behavior on its presence.
    #[serde(default)]
    pub analysis: Option<AnalysisConfig>,

    /// Cache directory and related settings.
    #[serde(default)]
    pub cache: Option<CacheConfig>,

    /// Filesystem path to the loaded config file, if any.
    ///
    /// Populated by [`Config::load`] and used to resolve relative paths
    /// (notably [`Config::aliases_file`]). Not serialized to YAML.
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
}

/// Analysis pipeline configuration (forward-compat with Python schema).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalysisConfig {
    /// ML-based commit categorization settings.
    #[serde(default)]
    pub ml_categorization: Option<MlCategorizationConfig>,
}

/// ML categorization toggle and model selection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MlCategorizationConfig {
    /// Whether ML categorization is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Optional model identifier.
    #[serde(default)]
    pub model: Option<String>,
}

/// Cache layer configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Filesystem directory used for cached artifacts. Supports `~` expansion.
    #[serde(default)]
    pub directory: Option<PathBuf>,
}

/// A single repository to collect commits from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepositoryConfig {
    /// Local filesystem path to the repository (supports `~` expansion).
    pub path: PathBuf,

    /// Display name used in reports. Falls back to the directory basename.
    #[serde(default)]
    pub name: Option<String>,

    /// Branch override; if `None`, the default branch is auto-detected.
    #[serde(default)]
    pub branch: Option<String>,

    /// Inclusive start date for commit collection (ISO 8601).
    #[serde(default)]
    pub since_date: Option<String>,

    /// Inclusive end date for commit collection (ISO 8601).
    #[serde(default)]
    pub until_date: Option<String>,

    /// Optional GitHub organization / owner for this repository.
    ///
    /// When set, this is used by the GitHub PR fetcher to construct the
    /// `owner/name` slug for org-wide / multi-repo collection without
    /// requiring `github.repo` to be set. Accepts the alias `owner:` in YAML
    /// for human readability (`org` is canonical to match the existing
    /// `github.org` convention).
    #[serde(default, alias = "owner")]
    pub org: Option<String>,
}

/// Team roster and identity aliases.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamConfig {
    /// Canonical team members.
    #[serde(default)]
    pub members: Vec<TeamMember>,

    /// Free-form aliases map: alias → canonical name.
    #[serde(default)]
    pub aliases: HashMap<String, String>,
}

/// A canonical team member with optional alias list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamMember {
    /// Canonical display name.
    pub name: String,

    /// Primary email address (canonical).
    pub email: String,

    /// Alternative names/emails that map to this member.
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// Output / reporting configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputConfig {
    /// Single output format identifier (`csv`, `json`, `markdown`).
    ///
    /// Retained for backward compatibility; prefer [`OutputConfig::formats`].
    #[serde(default)]
    pub format: Option<String>,

    /// Destination directory for reports.
    ///
    /// Accepts both `directory` (Python-compat) and `output_path` (legacy
    /// Rust) keys in the YAML.
    #[serde(default, alias = "output_path")]
    pub directory: Option<PathBuf>,

    /// Output format list (e.g. `["csv", "markdown"]`).
    #[serde(default)]
    pub formats: Vec<String>,

    /// Include unclassified commits in output.
    #[serde(default)]
    pub include_unclassified: bool,

    /// Include merge commits in output.
    #[serde(default)]
    pub include_merges: bool,

    /// Include file-level details in output.
    #[serde(default)]
    pub include_files: bool,
}

/// Classification cascade configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationConfig {
    /// Path to user-supplied rules YAML/JSON.
    #[serde(default)]
    pub rules_file: Option<PathBuf>,

    /// Whether to engage the LLM fallback tier.
    #[serde(default)]
    pub use_llm: bool,

    /// LLM model identifier (provider-specific).
    #[serde(default)]
    pub llm_model: Option<String>,

    /// LLM provider: `"openrouter"`, `"openai"`, or `"auto"` (default `"auto"`).
    ///
    /// `"auto"` prefers OpenRouter when `OPENROUTER_API_KEY` is set, then
    /// falls back to OpenAI when `OPENAI_API_KEY` is set.
    #[serde(default = "default_llm_provider")]
    pub llm_provider: String,

    /// Optional OpenRouter API key. If unset the environment variable
    /// `OPENROUTER_API_KEY` is consulted.
    #[serde(default)]
    pub openrouter_api_key: Option<String>,

    /// Minimum confidence required to accept a classification.
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,

    /// User-defined subcategories. Each entry must declare a `parent`
    /// top-level category. These extend the built-in subcategory registry;
    /// entries whose `name` matches an existing built-in replace it.
    ///
    /// Example YAML:
    /// ```yaml
    /// classification:
    ///   custom_categories:
    ///     - name: "payments"
    ///       parent: "integrations"
    ///       display_name: "Payments Integration"
    ///     - name: "auth"
    ///       parent: "feature"
    /// ```
    #[serde(default)]
    pub custom_categories: Vec<SubcategoryDef>,

    /// Minimum acceptable classification coverage percentage (0–100).
    ///
    /// After a classification run, the pipeline computes the share of
    /// commits that received a non-null, non-`"uncategorized"` verdict and
    /// emits a `tracing::warn!` if the result falls below this threshold.
    #[serde(default = "default_min_coverage_pct")]
    pub min_coverage_pct: f64,

    /// Confidence threshold at or below which the LLM fallback tier is invoked.
    ///
    /// After tiers 1–3 produce a verdict, the LLM fallback fires for any
    /// commit whose `confidence <= llm_fallback_threshold` (and only when
    /// [`Self::use_llm`] is true). The catch-all rule emits `confidence = 0.3`,
    /// so a value of `0.35` will route catch-all hits through the LLM while
    /// a value of `0.0` preserves the legacy behaviour of only invoking the
    /// LLM on truly empty (`confidence == 0.0`) verdicts.
    ///
    /// Defaults to `0.0` for backwards compatibility.
    #[serde(default)]
    pub llm_fallback_threshold: f64,

    /// Maximum number of concurrent in-flight LLM fallback requests.
    ///
    /// The LLM fallback tier issues one HTTP request per commit whose
    /// confidence is at or below [`Self::llm_fallback_threshold`]. Issuing
    /// these serially yields ~1 second per commit, which is intolerable on
    /// large corpora (e.g. 1000+ commits → 15+ minutes). Running them through
    /// `buffer_unordered(llm_fallback_concurrency)` typically cuts wall-clock
    /// time by an order of magnitude.
    ///
    /// Defaults to `8`. Increase for higher-throughput providers; decrease if
    /// you hit upstream rate limits.
    #[serde(default = "default_llm_fallback_concurrency")]
    pub llm_fallback_concurrency: usize,

    /// When `true`, all external classification sources (JIRA, GitHub Issues)
    /// are disabled for this run, regardless of `sources:` configuration in
    /// the rules file.
    ///
    /// Set via the `--no-external` CLI flag or in `config.yaml` for permanent
    /// offline / CI mode. Defaults to `false`.
    #[serde(default)]
    pub no_external: bool,

    /// External classification sources to consult before commit-message rules.
    ///
    /// Each entry describes one external system (JIRA or GitHub Issues).
    /// When a commit message contains a ticket key matching the source's
    /// pattern, the source is queried for issue type/labels which are then
    /// used to derive a classification category via the configured mappings.
    ///
    /// Populated from the `sources:` block in the rules YAML or from
    /// `config.yaml`. Ignored when `no_external` is `true`.
    #[serde(default)]
    pub sources: Vec<crate::classify::sources::SourceConfig>,
}

fn default_confidence_threshold() -> f64 {
    0.7
}

fn default_min_coverage_pct() -> f64 {
    20.0
}

fn default_llm_provider() -> String {
    "auto".to_string()
}

fn default_llm_fallback_concurrency() -> usize {
    8
}

impl Default for ClassificationConfig {
    fn default() -> Self {
        Self {
            rules_file: None,
            use_llm: false,
            llm_model: None,
            llm_provider: default_llm_provider(),
            openrouter_api_key: None,
            confidence_threshold: default_confidence_threshold(),
            custom_categories: Vec::new(),
            min_coverage_pct: default_min_coverage_pct(),
            llm_fallback_threshold: 0.0,
            llm_fallback_concurrency: default_llm_fallback_concurrency(),
            no_external: false,
            sources: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Tag and release-branch reachability configuration (issue #279).
///
/// Why: teams using cherry-pick-to-release deployment patterns need to
/// distinguish "deployed via tag" from "abandoned WIP".  This block controls
/// whether and how the reachability scanner runs.
/// What: enables/disables the tag scan, the release-branch scan, and the set
/// of branch-name glob patterns (e.g. `release/*`, `hotfix/*`) to match.
/// Test: loaded from YAML alongside the top-level Config; consumed by
/// `collect::git::reachability::scan_and_persist`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReachabilityConfig {
    /// If `true`, walk all git tags and populate `on_any_tag` /
    /// `reachable_from_tags`. Defaults to `true`.
    #[serde(default = "default_true")]
    pub track_tags: bool,

    /// If `true`, walk branches matching `release_branch_patterns` and
    /// populate `on_release_branch` / `release_branches`. Defaults to `true`.
    #[serde(default = "default_true")]
    pub track_release_branches: bool,

    /// Glob patterns for branch names treated as release branches.
    /// Supports a single `*` wildcard. Defaults to
    /// `["release/*", "hotfix/*", "chore/release-*", "v*"]`.
    #[serde(default = "default_release_branch_patterns")]
    pub release_branch_patterns: Vec<String>,
}

fn default_release_branch_patterns() -> Vec<String> {
    vec![
        "release/*".to_string(),
        "hotfix/*".to_string(),
        "chore/release-*".to_string(),
        "v*".to_string(),
    ]
}

impl Default for ReachabilityConfig {
    fn default() -> Self {
        Self {
            track_tags: true,
            track_release_branches: true,
            release_branch_patterns: default_release_branch_patterns(),
        }
    }
}

/// Linear project management integration settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinearConfig {
    /// Linear API key (personal or workspace).
    ///
    /// Supports `${LINEAR_API_KEY}` env-var substitution.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Only fetch issues from these team keys (e.g. `["ENG", "FE"]`).
    /// Empty = all teams.
    #[serde(default)]
    pub team_keys: Vec<String>,

    /// Fetch issue details when a commit message references a Linear issue ID.
    #[serde(default = "default_true")]
    pub fetch_on_reference: bool,

    /// Optional override regex for detecting Linear ticket references in
    /// commit messages.
    ///
    /// Must contain at least one capture group; capture group 1 is treated
    /// as the ticket ID. When `None`, the default pattern is used:
    /// `\b([A-Z][A-Z0-9]{0,9})-(\d+)\b` (same shape as JIRA keys —
    /// configurable here because Linear team prefixes are user-defined per
    /// workspace, so the default ten-character upper limit can be too
    /// restrictive).
    ///
    /// Validated at config-load time: invalid patterns cause [`Config::load`]
    /// to return an error.
    #[serde(default)]
    pub ticket_regex: Option<String>,
}

/// Project management integrations config block.
///
/// Located at `pm:` in YAML (clean namespace, avoids jira/jira_integration
/// dual-stack). Each member is independently optional; presence of the `pm`
/// block does not require any specific integration to be configured.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PmConfig {
    /// Azure DevOps integration (Phase 1: config + stub client).
    #[serde(default)]
    pub azure_devops: Option<AzureDevOpsConfig>,
}

/// DORA-metrics configuration (issues #207, #208, #212, #213).
///
/// Why: DORA metrics (deployment frequency, lead time, change failure
/// rate, mean time to recovery) require ingesting deployment events and
/// incidents from external sources and tying them back to commits.
/// What: groups deployment-source selection, failure-signal patterns,
/// and incident-source paths in one block.
/// Test: parsed by the YAML loader; consumed by the
/// `tga deployments collect` and `tga dora` commands.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DoraConfig {
    /// Source for deployment ingestion.
    ///
    /// One of: `github_releases`, `git_tags`, `github_actions`, `manual`.
    /// Defaults to `git_tags` because it works against any local checkout
    /// without external credentials.
    #[serde(default = "default_deployment_source")]
    pub deployment_source: String,

    /// Regex matching git tags that represent a production deployment.
    /// Default matches semver tags like `v1.2.3`.
    #[serde(default = "default_deployment_tag_pattern")]
    pub deployment_tag_pattern: String,

    /// Default branch that production deployments are cut from.
    #[serde(default = "default_production_branch")]
    pub production_branch: String,

    /// Optional GitHub Actions workflow file name (e.g.
    /// `deploy-production.yml`). Consumed when `deployment_source` is
    /// `github_actions` — currently stubbed; see source for details.
    #[serde(default)]
    pub deployment_workflow: Option<String>,

    /// Failure-signal patterns used by `tga dora` to identify deploys
    /// that were followed by a change-failure-rate event (issue #208).
    #[serde(default)]
    pub failure_signals: Vec<FailureSignal>,

    /// Path to a directory of incident JSON / CSV files (Datadog dump,
    /// etc.) consumed by `tga deployments collect` (issue #213). When
    /// `None`, the Datadog path is skipped and only JIRA SRE-derived
    /// incidents (if configured) populate `fact_incidents`.
    #[serde(default)]
    pub datadog_dir: Option<PathBuf>,
}

fn default_deployment_source() -> String {
    "git_tags".to_string()
}

fn default_deployment_tag_pattern() -> String {
    // Matches `v1.2.3`, `v1.2.3-rc.4`, `1.2.3`.
    r"^v?[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.\-]+)?$".to_string()
}

fn default_production_branch() -> String {
    "main".to_string()
}

/// One failure-signal rule used by `tga dora` to flag a deploy as a
/// change-failure-rate event (issue #208).
///
/// Why: change-failure-rate is "what % of production deploys were
/// followed by a failure event within N hours". The signal can be a
/// classification verdict (`work_type: bug_fix`) or a raw commit-message
/// pattern; either way the operator owns the policy.
/// What: combines a `work_type` predicate, an optional branch filter,
/// an optional regex pattern, and a within-hours window.
/// Test: parsed alongside `DoraConfig` and consumed by
/// `commands::dora`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FailureSignal {
    /// Match classification `category` (case-sensitive). `None` = match
    /// any classification.
    #[serde(default)]
    pub work_type: Option<String>,

    /// Restrict matches to commits on this branch. `None` = no filter.
    #[serde(default)]
    pub on_branch: Option<String>,

    /// Regex over the commit message. `None` = no regex filter.
    /// Validated at load via [`Config::validate_dora_signals`].
    #[serde(default)]
    pub commit_message_pattern: Option<String>,

    /// Time window after a deploy in which this signal counts as a
    /// failure. Defaults to 48 hours.
    #[serde(default = "default_failure_window_hours")]
    pub within_hours: u32,
}

fn default_failure_window_hours() -> u32 {
    48
}

/// GitHub API integration settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GithubConfig {
    /// Personal access token (often sourced from `GITHUB_TOKEN`).
    #[serde(default)]
    pub token: Option<String>,

    /// Organization slug for org-wide queries.
    #[serde(default)]
    pub org: Option<String>,

    /// Single-repository slug (`owner/name`).
    #[serde(default)]
    pub repo: Option<String>,

    /// Whether to fetch pull request metadata.
    #[serde(default)]
    pub fetch_prs: bool,

    /// Optional override regex for detecting GitHub issue / PR references
    /// in commit messages.
    ///
    /// Must contain at least one capture group; capture group 1 is treated
    /// as the ticket reference (e.g. `#42`). When `None`, the default
    /// pattern is used: `(?m)(?:^|\s)(#\d+)\b` — this requires a leading
    /// whitespace or start-of-line to avoid matching hex colors. Override
    /// when you need to detect `Fix:#123`, `(#123)`, `closes#42`, etc.
    ///
    /// Validated at config-load time: invalid patterns cause [`Config::load`]
    /// to return an error.
    #[serde(default)]
    pub ticket_regex: Option<String>,
}

/// Bitbucket Cloud API integration settings.
///
/// Auth must be supplied via **either** an access token (Bearer) **or** a
/// `username` + `app_password` pair (Basic auth). The validator enforces
/// "at least one usable mode populated" when `fetch_prs == true` — a wholly
/// auth-less config is rejected, partially-filled Basic auth (username
/// without password, or vice versa) is rejected, but populating both modes
/// at once is *accepted* and resolved by the client via Bearer-wins
/// precedence (token > username+password). This is intentional: it lets
/// operators set both during a migration from App Password to access token
/// without a transient failure window.
///
/// Tokens / passwords may also be sourced from the environment variables
/// `BITBUCKET_TOKEN` and `BITBUCKET_APP_PASSWORD` — the validator treats
/// either source as satisfying the requirement.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BitbucketConfig {
    /// Bitbucket account / workspace member username (required for Basic auth).
    #[serde(default)]
    pub username: Option<String>,

    /// Bitbucket App Password (Basic auth secret).
    ///
    /// Falls back to the `BITBUCKET_APP_PASSWORD` env var when unset.
    #[serde(default)]
    pub app_password: Option<String>,

    /// Workspace / repository access token (Bearer auth).
    ///
    /// Falls back to the `BITBUCKET_TOKEN` env var when unset. If both
    /// `token` and `app_password` are set the token wins.
    #[serde(default)]
    pub token: Option<String>,

    /// Workspace slug, e.g. the `myteam` in
    /// `bitbucket.org/myteam/myrepo`.
    #[serde(default)]
    pub workspace: Option<String>,

    /// Repository slug, e.g. the `myrepo` in
    /// `bitbucket.org/myteam/myrepo`.
    #[serde(default)]
    pub repo_slug: Option<String>,

    /// Whether to fetch pull request metadata.
    #[serde(default)]
    pub fetch_prs: bool,

    /// Override the Bitbucket API base URL.
    ///
    /// Defaults to `https://api.bitbucket.org/2.0`. This is primarily a
    /// test seam so `wiremock::MockServer::uri()` can stand in for the
    /// real API; production users should not need to set it.
    #[serde(default)]
    pub api_base_url: Option<String>,
}

/// JIRA Cloud / Server integration settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JiraConfig {
    /// Base URL of the JIRA instance.
    #[serde(default)]
    pub url: Option<String>,

    /// API username (typically an email address for Cloud).
    #[serde(default)]
    pub username: Option<String>,

    /// API token.
    #[serde(default)]
    pub token: Option<String>,

    /// Project key for filtering issues (e.g. `API`).
    #[serde(default)]
    pub project_key: Option<String>,

    /// Maps JIRA project keys to canonical work types (subcategory names).
    ///
    /// Used by the Tier 1.6
    /// [`crate::classify::tiers::jira_project_tier::JiraProjectTier`]
    /// classifier (issue #206). The tier fires between exact-keyword and
    /// regex matching, so project mappings outrank the generic
    /// `jira-ticket` regex rule but still defer to Tier-0 manual
    /// overrides and exact conventional-commit prefixes.
    ///
    /// Accepts the `jira_project_mapping` (singular) alias for parity
    /// with the issue-#206 spec.
    ///
    /// Example YAML:
    /// ```yaml
    /// jira:
    ///   jira_project_mappings:
    ///     TQL: bug_fix
    ///     APEX: integration
    ///     INFRA: platform_infrastructure
    ///     SEC: security
    /// ```
    #[serde(default, alias = "jira_project_mapping")]
    pub jira_project_mappings: HashMap<String, String>,

    /// Per-verdict confidence emitted by the JIRA project mapping tier.
    ///
    /// Defaults to
    /// [`crate::classify::tiers::jira_project_tier::DEFAULT_PROJECT_MAPPING_CONFIDENCE`]
    /// (0.88). Tune downward to make exact-keyword rules win more often,
    /// upward to crowd out manual overrides less aggressively.
    #[serde(default)]
    pub jira_project_mapping_confidence: Option<f64>,

    /// Optional override regex for detecting JIRA ticket references in
    /// commit messages.
    ///
    /// Must contain at least one capture group; capture group 1 is treated
    /// as the ticket key (e.g. `PROJ-123`). When `None`, the default pattern
    /// is used: `\b([A-Z][A-Z0-9]{0,9})-(\d+)\b` (uppercase keys, max
    /// 10-char prefix). Override to support lowercase keys (`proj-123`) or
    /// project prefixes longer than 10 characters.
    ///
    /// Validated at config-load time: invalid patterns cause [`Config::load`]
    /// to return an error.
    #[serde(default)]
    pub ticket_regex: Option<String>,
}

/// Expand a leading `~` in a path to the current user's home directory.
///
/// Returns the path unchanged if it does not start with `~`. If `~` is
/// present but the home directory cannot be determined, the path is also
/// returned unchanged.
pub fn expand_path(path: &Path) -> PathBuf {
    let s = match path.to_str() {
        Some(s) => s,
        None => return path.to_path_buf(),
    };
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    } else if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    path.to_path_buf()
}

impl Config {
    /// Load a YAML configuration from disk.
    ///
    /// # Errors
    ///
    /// - [`TgaError::IoError`] if the file cannot be read.
    /// - [`TgaError::SerdeYamlError`] if YAML parsing fails.
    /// - [`TgaError::ConfigError`] if any user-supplied `ticket_regex`
    ///   (JIRA, GitHub, Linear) is not a valid regular expression.
    pub fn load(path: &Path) -> Result<Config> {
        let resolved = expand_path(path);
        tracing::debug!(path = %resolved.display(), "loading config");
        let text = std::fs::read_to_string(&resolved)?;
        let mut cfg: Config = serde_yaml::from_str(&text)?;
        cfg.source_path = Some(resolved);
        cfg.validate_ticket_regexes()?;
        Ok(cfg)
    }

    /// Validate every user-supplied `ticket_regex` in the config.
    ///
    /// Why: surfaces invalid regexes immediately at load time rather than
    /// at first use deep inside the pipeline, with a clear error message
    /// naming the offending section.
    /// What: compiles `jira.ticket_regex`, `github.ticket_regex`,
    /// `linear.ticket_regex`, and `pm.azure_devops.ticket_regex` if present;
    /// returns the first failure.
    /// Test: load a config with `jira.ticket_regex: "["` and assert the
    /// returned error is `TgaError::ConfigError` mentioning `jira`.
    fn validate_ticket_regexes(&self) -> Result<()> {
        fn check(section: &str, pat: &Option<String>) -> Result<()> {
            if let Some(p) = pat {
                regex::Regex::new(p).map_err(|e| {
                    TgaError::ConfigError(format!(
                        "{section}.ticket_regex is not a valid regular expression: {e}"
                    ))
                })?;
            }
            Ok(())
        }
        if let Some(jira) = &self.jira {
            check("jira", &jira.ticket_regex)?;
        }
        if let Some(gh) = &self.github {
            check("github", &gh.ticket_regex)?;
        }
        if let Some(linear) = &self.linear {
            check("linear", &linear.ticket_regex)?;
        }
        // `pm.azure_devops.ticket_regex` is a non-Option String (serde
        // applies `default_ticket_regex` when omitted), so check it as
        // `Some(_)` regardless of whether the user customised it. Issue
        // #90: this used to be unchecked and bad patterns failed in the
        // middle of collection.
        if let Some(adc) = self.azure_devops_config() {
            check("pm.azure_devops", &Some(adc.ticket_regex.clone()))?;
        }
        // DORA failure signals (issue #208) and deployment-tag pattern
        // (#207) are user-supplied regexes; reject malformed values at
        // load time rather than mid-pipeline.
        if let Some(dora) = &self.dora {
            check(
                "dora.deployment_tag_pattern",
                &Some(dora.deployment_tag_pattern.clone()),
            )?;
            for (i, sig) in dora.failure_signals.iter().enumerate() {
                let label = format!("dora.failure_signals[{i}].commit_message_pattern");
                check(&label, &sig.commit_message_pattern)?;
            }
        }
        Ok(())
    }

    /// Directory containing the loaded config file, if known.
    ///
    /// Returns the parent of [`Config::source_path`]; used to resolve
    /// relative paths declared inside the config (e.g.
    /// [`Config::aliases_file`]).
    pub fn config_dir(&self) -> Option<&Path> {
        self.source_path.as_deref().and_then(|p| p.parent())
    }

    /// Resolve identity aliases from either the Python-compatible
    /// [`Config::developer_aliases`] map or from [`TeamConfig::members`].
    ///
    /// `developer_aliases` (when non-empty) takes precedence. The returned
    /// map is keyed by canonical name; values are the list of email
    /// addresses or login aliases that should resolve to that name.
    pub fn resolved_aliases(&self) -> HashMap<String, Vec<String>> {
        // Fall back to whatever we can resolve without surfacing errors;
        // callers that need to fail loudly on a bad `aliases_file` should
        // use [`Config::resolved_alias_map`] directly.
        match self.resolved_alias_map(self.config_dir()) {
            Ok(map) if !map.is_empty() => map,
            _ => {
                if let Some(team) = &self.team {
                    team.members
                        .iter()
                        .map(|m| (m.name.clone(), m.aliases.clone()))
                        .collect()
                } else {
                    HashMap::new()
                }
            }
        }
    }

    /// Resolve the full alias map by merging inline [`Config::developer_aliases`]
    /// with entries loaded from an external [`Config::aliases_file`] (if set).
    ///
    /// Merge semantics: external file entries **override** inline entries
    /// with the same canonical name. Entries in inline that are not in the
    /// external file are kept as-is.
    ///
    /// Path resolution for `aliases_file`:
    /// 1. Leading `~` is expanded to the user's home directory.
    /// 2. If still relative, resolved against `config_dir` if provided,
    ///    otherwise against the current working directory.
    ///
    /// # Errors
    ///
    /// Returns [`TgaError::ConfigError`] if [`Config::aliases_file`] is set
    /// but cannot be loaded or parsed.
    pub fn resolved_alias_map(
        &self,
        config_dir: Option<&Path>,
    ) -> Result<HashMap<String, Vec<String>>> {
        let mut merged = self.developer_aliases.clone();

        if let Some(rel) = &self.aliases_file {
            let expanded = expand_path(Path::new(rel));
            let resolved = if expanded.is_absolute() {
                expanded
            } else if let Some(dir) = config_dir {
                dir.join(expanded)
            } else {
                expanded
            };

            let external = AliasFile::load(&resolved).map_err(|e| {
                TgaError::ConfigError(format!(
                    "failed to load aliases_file {}: {e}",
                    resolved.display()
                ))
            })?;
            for (name, list) in external.to_alias_map() {
                // External overrides inline for matching canonical names.
                merged.insert(name, list);
            }
        }

        Ok(merged)
    }

    /// Convenience accessor for the Azure DevOps integration config, if any.
    ///
    /// Returns `Some(&AzureDevOpsConfig)` when `pm.azure_devops` is set in
    /// the YAML, otherwise `None`.
    pub fn azure_devops_config(&self) -> Option<&AzureDevOpsConfig> {
        self.pm.as_ref().and_then(|p| p.azure_devops.as_ref())
    }

    /// Validate cross-field invariants of the config.
    ///
    /// # Errors
    ///
    /// Returns [`TgaError::ValidationError`] if any invariant is violated,
    /// or [`TgaError::ConfigError`] propagated from per-integration
    /// validators (e.g. Azure DevOps URL checks).
    pub fn validate(&self) -> Result<()> {
        if self.repositories.is_empty() {
            return Err(TgaError::ValidationError(
                "at least one repository must be configured".into(),
            ));
        }
        for r in &self.repositories {
            if r.path.as_os_str().is_empty() {
                return Err(TgaError::ValidationError(
                    "repository.path must not be empty".into(),
                ));
            }
        }
        if let Some(adzo_config) = self.azure_devops_config() {
            adzo_config.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn azdo_cfg_with_regex(pat: &str) -> AzureDevOpsConfig {
        AzureDevOpsConfig {
            organization_url: "https://dev.azure.com/myorg".into(),
            pat: "secret".into(),
            project: Some("MyProject".into()),
            projects: vec![],
            ticket_regex: pat.into(),
            team_keys: vec![],
            fetch_on_reference: true,
            fetch_prs: false,
        }
    }

    fn cfg_with_ado_regex(pat: &str) -> Config {
        Config {
            pm: Some(PmConfig {
                azure_devops: Some(azdo_cfg_with_regex(pat)),
            }),
            ..Config::default()
        }
    }

    #[test]
    fn validate_ticket_regexes_accepts_valid_ado_pattern() {
        // Regression test for #90: a configured pm.azure_devops.ticket_regex
        // is reachable from validate_ticket_regexes and accepted when valid.
        cfg_with_ado_regex(r"#(\d{4,8})\b")
            .validate_ticket_regexes()
            .expect("valid ADO regex accepted");
    }

    #[test]
    fn validate_ticket_regexes_rejects_bad_ado_pattern() {
        // Regression test for #90: a malformed pm.azure_devops.ticket_regex
        // must fail at config load, not deep inside the collector.
        let err = cfg_with_ado_regex("[unclosed")
            .validate_ticket_regexes()
            .expect_err("malformed ADO ticket_regex must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("pm.azure_devops"),
            "error should name the section: {msg}"
        );
        assert!(
            msg.contains("ticket_regex"),
            "error should name the field: {msg}"
        );
    }
}
