//! Configuration types deserialized from YAML.
//!
//! The full configuration schema is documented in
//! `docs/trusty-git-analytics/requirements/configuration.md`. This module implements the practical
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

use serde::{Deserialize, Deserializer, Serialize};

use crate::classify::taxonomy::SubcategoryDef;
use crate::core::errors::{Result, TgaError};

pub mod aliases;
pub mod azdo;
pub mod validator;

pub use aliases::{AliasFile, DeveloperAliasEntry};
pub use azdo::AzureDevOpsConfig;
pub use validator::{ConfigError, ConfigValidator};

/// LLM provider selection for the classification LLM tier.
///
/// Why: operators need to switch between OpenRouter, AWS Bedrock, and the
/// direct Anthropic API without changing binary flags. An enum keeps the set
/// of valid values closed and type-safe.
/// What: three variants — `Openrouter`, `Bedrock`, and `AnthropicApi`.
/// Serde renames map to lowercase kebab-case strings matching the YAML schema.
/// Test: deserialization is covered by `llm_config_*` unit tests in this
/// module. Provider-specific behaviour is covered by `classify::tiers::llm`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlmSource {
    /// Route through the OpenRouter API (OpenAI-compatible schema).
    ///
    /// Requires a key stored in the environment variable named by
    /// [`LlmConfig::api_key_env`] (default: `OPENROUTER_API_KEY`).
    #[default]
    Openrouter,
    /// Route through AWS Bedrock (IAM credential-chain auth, no API key).
    ///
    /// Only available when the binary is compiled with `--features bedrock`.
    /// Requires valid AWS credentials in the default chain (env vars, profile,
    /// SSO, IMDS, etc.). No secret is stored in the config; region and model
    /// are the only Bedrock-specific fields.
    Bedrock,
    /// Route through the Anthropic Messages API directly (scaffold only).
    ///
    /// Recognized enum value; returns a clear "not yet implemented" error at
    /// construction time.
    #[serde(rename = "anthropic-api")]
    AnthropicApi,
}

/// Top-level LLM configuration section (`llm:` in YAML).
///
/// Why: the previous design placed LLM credentials inside
/// `classification.openrouter_api_key` and `classification.llm_provider`,
/// mixing transport concerns with classification tuning. The `llm:` section
/// separates *how to reach an LLM* from *when to use it*, and enables
/// first-class AWS Bedrock support (region + model, no stored secret).
/// What: groups provider selection, the environment-variable name holding
/// any required API key (never the key itself), an optional AWS region
/// override (Bedrock only), and the model id. The section is optional;
/// when absent the pipeline falls back to legacy `classification.*` fields.
/// Test: `llm_config_parses_from_yaml` and `llm_source_defaults_to_openrouter`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// LLM provider to use.
    ///
    /// Valid values (YAML): `openrouter`, `bedrock`, `anthropic-api`.
    /// Defaults to `openrouter`.
    #[serde(default)]
    pub source: LlmSource,

    /// Name of the environment variable holding the API key.
    ///
    /// For `openrouter` and `anthropic-api` this is required. The value
    /// stored here is the **variable name** (e.g. `OPENROUTER_API_KEY`),
    /// never the secret itself. At use time the pipeline reads the env var.
    /// If the variable is unset or empty when a key-based source is in use,
    /// the LLM tier fails loudly with an actionable error — no silent no-ops.
    ///
    /// For `bedrock` this field is ignored; AWS credentials are resolved via
    /// the SDK's default credential chain.
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,

    /// AWS region for Bedrock invocations (Bedrock only).
    ///
    /// Ignored for `openrouter` and `anthropic-api`. When absent, the AWS SDK
    /// reads the region from the environment (`AWS_DEFAULT_REGION`,
    /// `AWS_REGION`, or the active profile) as usual.
    #[serde(default)]
    pub region: Option<String>,

    /// Model identifier (provider-specific).
    ///
    /// Examples:
    /// - OpenRouter: `"gpt-4o-mini"`, `"anthropic/claude-3-5-sonnet"`
    /// - Bedrock: `"anthropic.claude-3-5-sonnet-20241022-v2:0"`
    /// - Anthropic API: `"claude-3-5-sonnet-20241022"`
    ///
    /// When absent, a provider-appropriate default is used.
    #[serde(default)]
    pub model: Option<String>,
}

fn default_api_key_env() -> String {
    "OPENROUTER_API_KEY".to_string()
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            source: LlmSource::default(),
            api_key_env: default_api_key_env(),
            region: None,
            model: None,
        }
    }
}

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

    /// SQLite database path override.
    ///
    /// Why: setting the database path in YAML lets teams commit a shared
    /// config that points to a team-shared DB location without every operator
    /// having to remember a `--database` flag.
    /// What: when set, the pipeline uses this path as the DB location. The
    /// `--database` CLI flag takes precedence over this value; the hardcoded
    /// default (`tga.db`) is used only when neither is supplied.
    /// Supports `~` home-directory expansion (same as other path fields).
    /// Test: see `config_database_field_parsed` in the unit tests below.
    #[serde(default)]
    pub database: Option<PathBuf>,

    /// Top-level LLM configuration section.
    ///
    /// Why: separates LLM transport concerns (provider, credentials,
    /// region/model) from classification tuning (when to invoke the LLM,
    /// thresholds). When present, this section takes precedence over the
    /// legacy `classification.openrouter_api_key` / `classification.llm_provider`
    /// fields. When absent the pipeline falls back to those fields with a
    /// deprecation warning.
    /// What: holds [`LlmConfig`] deserialized from the `llm:` YAML key.
    /// Test: see `llm_config_parses_from_yaml` in the unit tests below.
    #[serde(default)]
    pub llm: Option<LlmConfig>,

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

    /// Per-repo opt-out of all-branch walking.
    ///
    /// Why: the 2.0.0 default changed to walk all local branches and remote
    /// tracking refs (`refs/heads/*` + `refs/remotes/origin/*`).  Setting
    /// `head_only: true` restores the legacy HEAD-only walk for a specific
    /// repository while keeping all-branch coverage for every other repo.
    /// Global opt-out is available via the `--head-only` CLI flag on
    /// `tga collect`.
    /// What: when `true`, the `GitCollector` seeds the revwalk from HEAD only
    /// (same behaviour as tga ≤ 1.5.4).  When `false` (the default), all
    /// local heads and `refs/remotes/origin/*` are pushed.
    /// Test: see `tests::multi_branch_coverage` and related in
    /// `collect::git::extractor`.
    #[serde(default)]
    pub head_only: bool,

    /// Optional timeout (in seconds) for the pre-walk `git fetch origin`.
    ///
    /// Why: a single slow or unresponsive remote can stall an entire
    /// `tga collect` run when the default system timeout is very long.
    /// Providing a per-repo cap lets one repo time out without blocking others.
    /// What: when `Some(n)`, the fetch is limited to `n` seconds; when `None`
    /// (the default), the system / git2 transport defaults are used.
    /// Test: exercised indirectly by end-to-end tests that pass `no_fetch=true`.
    #[serde(default)]
    pub fetch_timeout_secs: Option<u64>,
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

    /// Preferred email domain when selecting the canonical email for a
    /// newly-discovered identity (issue #349).
    ///
    /// Why: by default `tga collect` records the first-seen `author_email`
    /// as the canonical identity. For organisations where most people commit
    /// under multiple email domains (work / personal / GitHub noreply), this
    /// produces inconsistent canonical addresses. Setting `canonical_domain`
    /// prefers any email whose domain matches over a first-seen non-matching
    /// address.
    /// What: when set, the resolver prefers an email whose domain equals this
    /// value (case-insensitive) over the raw observed email, falling back to
    /// the raw value if no domain-matching candidate is available.
    /// Test: see `collect::identity::resolver::tests::canonical_domain_preferred`.
    #[serde(default)]
    pub canonical_domain: Option<String>,
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
///
/// `deny_unknown_fields` closes the class of silent-drop bugs seen in
/// issues #259 and #286. Any YAML key under `classification:` that is not
/// a recognised field is rejected at load time with a clear error message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassificationConfig {
    /// Supplemental rule files to load and merge in order (#445 batch C).
    ///
    /// Why: operators often want to layer project-specific rules on top of a
    /// shared base file without editing it. Listing multiple files here loads
    /// them in order — later files extend or override earlier ones (same-id
    /// rules from a later file win).
    ///
    /// Accepts either a single path string (backward-compatible alias
    /// `rules_file`) or a YAML list of paths. An absent key yields an empty
    /// vec, meaning no user rules are loaded and the built-in defaults are
    /// used exclusively.
    ///
    /// Example (YAML):
    /// ```yaml
    /// classification:
    ///   rules_files:
    ///     - ~/shared/base-rules.yaml
    ///     - ./project-overrides.yaml
    /// ```
    ///
    /// Single-path back-compat (equivalent to the old `rules_file:`):
    /// ```yaml
    /// classification:
    ///   rules_file: ~/my-rules.yaml
    /// ```
    #[serde(
        default,
        alias = "rules_file",
        deserialize_with = "deserialize_rules_files"
    )]
    pub rules_files: Vec<PathBuf>,

    /// Per-repo default subcategory fallback (#445 batch C).
    ///
    /// Why: reduces the 'uncategorized' rate for well-known repositories
    /// without LLM cost. After the full classification cascade (including the
    /// LLM tier), commits that are still uncategorized (or below the
    /// confidence threshold) and whose repository matches an entry here are
    /// assigned the configured default subcategory.
    ///
    /// Keys are repository names or simple glob patterns (single `*`
    /// wildcard). Values are subcategory names (resolved through the taxonomy
    /// to set `top_level_category`).
    ///
    /// **Precedence**: this is the last-resort fallback (Tier 5). A
    /// confidently-classified commit is NEVER overridden. Literal key matches
    /// take precedence over glob matches.
    ///
    /// Example (YAML):
    /// ```yaml
    /// classification:
    ///   repo_categories:
    ///     infra-api: platform_infrastructure
    ///     "data-*": data_engineering
    ///     legacy-monolith: maintenance
    /// ```
    #[serde(default)]
    pub repo_categories: HashMap<String, String>,

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
    /// [`Self::use_llm`] is true).
    ///
    /// **Default (1.3.0+): `0.65`** — The weighted-sum tier (Tier 2.5) emits
    /// calibrated verdicts in `[0.55, 0.95]`. The legacy fuzzy tier emits
    /// `0.40` (short-message chore) and `0.60` (bare-ticket feature). With
    /// the 0.65 default, any deterministic verdict below 0.65 routes to the
    /// LLM when `use_llm: true`, which includes the low-confidence fuzzy
    /// outputs as well as low-scoring weighted-sum verdicts.
    ///
    /// **Migration note**: users with `use_llm: false` (the default) see no
    /// behaviour change — the threshold only matters when LLM is enabled.
    /// Users with `use_llm: true` who relied on the 0.0 default to skip the
    /// LLM for fuzzy-tier verdicts should pin `llm_fallback_threshold: 0.0`
    /// explicitly to restore the previous behaviour.
    ///
    /// The legacy catch-all rule emits `confidence = 0.3`, so setting this
    /// to `0.35` still routes those through the LLM. The new default of `0.65`
    /// is a broader net that covers fuzzy-tier verdicts (0.40, 0.60) as well.
    #[serde(default = "default_llm_fallback_threshold")]
    pub llm_fallback_threshold: f64,

    /// Configuration for the weighted-sum tier (Tier 2.5).
    ///
    /// Tier 2.5 sits between the regex tier (Tier 2) and the fuzzy tier (Tier
    /// 3). It composes five cheap signals — keyword density, ticket-prefix
    /// presence, message-length bucket, merge indicator, and file-path bucket
    /// — into per-category scores and emits a calibrated verdict when the
    /// argmax exceeds `min_confidence` (default 0.55).
    ///
    /// Set `weighted_sum.enabled: false` to disable the tier entirely and
    /// restore pre-1.3.0 behaviour (regex falls directly to fuzzy).
    /// This tier is intentionally active even when `extend_defaults: false`
    /// because it composes signals rather than emitting hardcoded built-in
    /// category strings; it respects user taxonomies through the engine's
    /// taxonomy registry.
    #[serde(default)]
    pub weighted_sum: crate::classify::tiers::weighted_sum::WeightedSumConfig,

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

    /// Issue a `PRAGMA wal_checkpoint(PASSIVE)` every N commits during
    /// classification to limit crash-window data loss (issue #298).
    ///
    /// On a 22-minute classify run with no intermediate checkpoints, a crash
    /// can lose all classifications since the last automatic SQLite checkpoint.
    /// Setting this to a positive value periodically flushes the WAL so the
    /// maximum data-loss window is bounded.
    ///
    /// - `0` (default) — no periodic checkpoints (only the on-exit
    ///   `TRUNCATE` checkpoint runs).
    /// - `> 0` — checkpoint every N commits written.
    ///
    /// Recommended for corpora > 5000 commits: set to `5000`.
    /// This field is also accepted at `dora.classify.checkpoint_every` in
    /// the YAML for backward-compat, but `classification.checkpoint_every`
    /// is the canonical location.
    #[serde(default)]
    pub checkpoint_every: usize,

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

/// Deserialize `rules_files` from either a single path string or a list of paths.
///
/// Why: back-compat with the legacy `rules_file: Option<PathBuf>` field.
/// The `#[serde(alias = "rules_file")]` attribute handles the rename; this
/// deserializer handles the scalar-vs-list duality.
/// What: accepts an absent key → `[]`, a single string → `[path]`, or a
/// YAML list → `[path, …]`.
/// Test: `tests::rules_files_single_string_back_compat` and
/// `tests::rules_files_list_parses`.
fn deserialize_rules_files<'de, D>(deserializer: D) -> std::result::Result<Vec<PathBuf>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(PathBuf),
        Many(Vec<PathBuf>),
        Null,
    }
    match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(p) => Ok(vec![p]),
        OneOrMany::Many(v) => Ok(v),
        OneOrMany::Null => Ok(vec![]),
    }
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

/// Default LLM fallback threshold (1.3.0+).
///
/// Why: raised from 0.0 to 0.65 in 1.3.0 so that low-confidence deterministic
/// verdicts (fuzzy tier: 0.40 chore, 0.60 feature; weighted-sum tier: anything
/// below 0.65) automatically route to the LLM when `use_llm: true`. Users with
/// `use_llm: false` see no behaviour change.
/// What: returns the default threshold value of 0.65.
/// Test: see `pipeline_writes_complexity_to_db` (pipeline test) which pins
/// `llm_fallback_threshold: 1.0` explicitly to force LLM routing.
fn default_llm_fallback_threshold() -> f64 {
    0.65
}

impl Default for ClassificationConfig {
    fn default() -> Self {
        Self {
            rules_files: Vec::new(),
            repo_categories: HashMap::new(),
            use_llm: false,
            llm_model: None,
            llm_provider: default_llm_provider(),
            openrouter_api_key: None,
            confidence_threshold: default_confidence_threshold(),
            custom_categories: Vec::new(),
            min_coverage_pct: default_min_coverage_pct(),
            llm_fallback_threshold: default_llm_fallback_threshold(),
            llm_fallback_concurrency: default_llm_fallback_concurrency(),
            no_external: false,
            sources: Vec::new(),
            weighted_sum: crate::classify::tiers::weighted_sum::WeightedSumConfig::default(),
            checkpoint_every: 0,
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

    /// Resolve the effective database path, applying `~` expansion.
    ///
    /// Why: callers (main.rs, command handlers) need a single resolved path
    /// that honors the `database:` YAML field. This method encapsulates the
    /// expansion so callers do not need to import `expand_path` directly.
    /// What: returns the expanded form of `self.database` when set, or `None`
    /// when the field is absent (callers apply the hardcoded default).
    /// Test: see `config_database_field_parsed` in the module unit tests.
    pub fn resolved_database_path(&self) -> Option<PathBuf> {
        self.database.as_deref().map(expand_path)
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

    /// Why: `deny_unknown_fields` on `ClassificationConfig` closes the
    /// silent-drop bug class. An unknown key under `classification:` (e.g. a
    /// YAML typo) must be rejected loudly at parse time.
    /// What: deserialize a `ClassificationConfig` fragment containing an
    /// unknown field and assert the result is `Err`.
    /// Test: pure deserialization regression guard.
    #[test]
    fn classification_config_unknown_field_is_rejected() {
        // `rules_path:` is a plausible typo for `rules_file:`.
        let yaml = "rules_path: ./my-rules.yaml\nuse_llm: false\n";
        let result: std::result::Result<ClassificationConfig, serde_yaml::Error> =
            serde_yaml::from_str(yaml);
        assert!(
            result.is_err(),
            "ClassificationConfig with unknown `rules_path:` must be rejected"
        );
    }

    /// Why: the `database:` field in YAML must deserialize into `Config.database`
    /// so operators can set the DB path without a CLI flag.
    /// What: parse a YAML snippet with `database:` set and assert the value.
    /// Test: pure deserialization; path expansion is not asserted here (that
    /// is tested by `resolved_database_path_expands_tilde`).
    #[test]
    fn config_database_field_parsed() {
        let yaml = "database: /var/data/tga.db\n";
        let cfg: Config = serde_yaml::from_str(yaml).expect("parse config");
        assert_eq!(
            cfg.database.as_deref(),
            Some(std::path::Path::new("/var/data/tga.db")),
            "database: field must be deserialized"
        );
        // resolved_database_path returns the same value (no tilde to expand).
        assert_eq!(
            cfg.resolved_database_path().as_deref(),
            Some(std::path::Path::new("/var/data/tga.db")),
        );
    }

    /// Why: `resolved_database_path` must return `None` when the field is
    /// absent so main.rs knows to fall back to the CLI flag or hardcoded default.
    /// What: deserialize an empty config and assert `None`.
    /// Test: pure deserialization.
    #[test]
    fn config_database_field_absent_returns_none() {
        let cfg = Config::default();
        assert!(
            cfg.resolved_database_path().is_none(),
            "absent database field must return None"
        );
    }

    /// Why: the `llm:` YAML section must deserialize into `Config.llm` with
    /// the correct provider, env-var name, and model.
    /// What: parse a YAML snippet with all four `llm:` fields set.
    /// Test: pure deserialization.
    #[test]
    fn llm_config_parses_from_yaml() {
        let yaml = "llm:\n  source: openrouter\n  api_key_env: MY_KEY\n  model: gpt-4o-mini\n";
        let cfg: Config = serde_yaml::from_str(yaml).expect("parse config");
        let llm = cfg.llm.expect("llm section");
        assert_eq!(llm.source, LlmSource::Openrouter);
        assert_eq!(llm.api_key_env, "MY_KEY");
        assert_eq!(llm.model.as_deref(), Some("gpt-4o-mini"));
        assert!(llm.region.is_none());
    }

    /// Why: `bedrock` must parse as `LlmSource::Bedrock` (not a variant of
    /// another name); a typo in the serde rename would silently fall through.
    /// What: parse `source: bedrock` and assert the variant.
    /// Test: pure deserialization.
    #[test]
    fn llm_source_bedrock_parses() {
        let yaml = "source: bedrock\napi_key_env: IGNORED\nregion: us-west-2\n";
        let llm: LlmConfig = serde_yaml::from_str(yaml).expect("parse llm config");
        assert_eq!(llm.source, LlmSource::Bedrock);
        assert_eq!(llm.region.as_deref(), Some("us-west-2"));
    }

    /// Why: `LlmConfig::default()` must produce `source: openrouter` and the
    /// canonical env-var name so the YAML-absent case works end-to-end.
    /// What: call `LlmConfig::default()` and assert the two key fields.
    /// Test: pure construction.
    #[test]
    fn llm_source_defaults_to_openrouter() {
        let llm = LlmConfig::default();
        assert_eq!(llm.source, LlmSource::Openrouter);
        assert_eq!(llm.api_key_env, "OPENROUTER_API_KEY");
    }

    /// Why: `anthropic-api` uses a hyphen in the YAML value; a missing serde
    /// rename would break parsing (serde would look for `anthropic_api`).
    /// What: parse `source: anthropic-api` and assert the variant.
    /// Test: pure deserialization.
    #[test]
    fn llm_source_anthropic_api_parses() {
        let yaml = "source: anthropic-api\n";
        let llm: LlmConfig = serde_yaml::from_str(yaml).expect("parse llm config");
        assert_eq!(llm.source, LlmSource::AnthropicApi);
    }

    /// Why: single-string `rules_file:` (old form) must still parse via the
    /// alias and coerce to a single-element Vec<PathBuf> (#445 batch C).
    /// What: parse `rules_file: ./my-rules.yaml` and assert one-element vec.
    /// Test: back-compat regression guard.
    #[test]
    fn rules_files_single_string_back_compat() {
        let yaml = "rules_file: ./my-rules.yaml\nuse_llm: false\n";
        let cfg: ClassificationConfig =
            serde_yaml::from_str(yaml).expect("parse classification config");
        assert_eq!(cfg.rules_files.len(), 1);
        assert_eq!(
            cfg.rules_files[0],
            std::path::PathBuf::from("./my-rules.yaml")
        );
    }

    /// Why: `rules_files:` (list form) must parse into a Vec<PathBuf> with
    /// all entries preserved in order (#445 batch C).
    /// What: parse a two-element list and assert both paths and their order.
    /// Test: pure deserialization.
    #[test]
    fn rules_files_list_parses() {
        let yaml = "rules_files:\n  - ~/base-rules.yaml\n  - ./project.yaml\n";
        let cfg: ClassificationConfig =
            serde_yaml::from_str(yaml).expect("parse classification config");
        assert_eq!(cfg.rules_files.len(), 2);
        assert_eq!(
            cfg.rules_files[0],
            std::path::PathBuf::from("~/base-rules.yaml")
        );
        assert_eq!(
            cfg.rules_files[1],
            std::path::PathBuf::from("./project.yaml")
        );
    }

    /// Why: `repo_categories:` must deserialize into a HashMap<String, String>
    /// (#445 batch C).
    /// What: parse a two-entry map and assert the values.
    /// Test: pure deserialization.
    #[test]
    fn repo_categories_parses() {
        let yaml =
            "repo_categories:\n  infra-api: platform_infrastructure\n  data-pipeline: data_engineering\n";
        let cfg: ClassificationConfig =
            serde_yaml::from_str(yaml).expect("parse classification config");
        assert_eq!(
            cfg.repo_categories.get("infra-api").map(|s| s.as_str()),
            Some("platform_infrastructure")
        );
        assert_eq!(
            cfg.repo_categories.get("data-pipeline").map(|s| s.as_str()),
            Some("data_engineering")
        );
    }

    /// Why: `ClassificationConfig::default()` must initialize both new fields
    /// to their empty states (#445 batch C).
    /// What: construct default and assert rules_files.is_empty() and
    /// repo_categories.is_empty().
    /// Test: construction test.
    #[test]
    fn classification_config_default_has_empty_new_fields() {
        let cfg = ClassificationConfig::default();
        assert!(
            cfg.rules_files.is_empty(),
            "rules_files defaults to empty vec"
        );
        assert!(
            cfg.repo_categories.is_empty(),
            "repo_categories defaults to empty map"
        );
    }
}
