//! `tga` — the trusty-git-analytics command-line binary.
//!
//! Wires together the library modules (`core`, `collect`, `classify`,
//! `report`) behind a clap subcommand interface.

#![warn(missing_docs)]

mod commands;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use tracing_subscriber::EnvFilter;

use tga::core::config::{Config, ConfigValidator};
use tga::core::db::Database;

use crate::commands::aliases::AliasesArgs;
use crate::commands::backfill::BackfillArgs;
use crate::commands::deployments::DeploymentsCollectArgs;
use crate::commands::dora::DoraArgs;
use crate::commands::incidents::IncidentsCollectArgs;
use crate::commands::install::InstallArgs;
use crate::commands::override_cmd::OverrideArgs;
use crate::commands::pr_metrics::PrMetricsArgs;
use crate::commands::rules::RulesArgs;

/// Top-level CLI parser.
#[derive(Parser, Debug)]
#[command(
    name = "tga",
    about = "trusty-git-analytics — developer productivity analytics",
    long_about = "trusty-git-analytics — developer productivity analytics.\n\n\
        Three-stage pipeline: collect → classify → report. Run `tga analyze` \
        for the full pipeline, or invoke each stage individually.\n\n\
        Architecture decisions are documented in docs/adr/. See \
        docs/adr/README.md for the format and process.",
    version,
    propagate_version = true
)]
struct Cli {
    /// Path to config file (default: ./config.yaml).
    #[arg(short, long, default_value = "config.yaml", global = true)]
    config: PathBuf,

    /// Path to SQLite database (default: ./tga.db).
    #[arg(short, long, default_value = "tga.db", global = true)]
    database: PathBuf,

    /// Verbosity level (-v, -vv, -vvv). Shortcut for `--log`.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Log level: error, warn, info, debug, trace. Overrides `-v`.
    /// The `RUST_LOG` environment variable, if set, takes precedence
    /// over this flag.
    #[arg(long, value_name = "LEVEL", global = true)]
    log: Option<LogLevel>,

    #[command(subcommand)]
    command: Commands,
}

/// Log level values accepted by the `--log` global flag.
#[derive(Copy, Clone, Debug, ValueEnum)]
#[clap(rename_all = "lower")]
enum LogLevel {
    /// Errors only.
    Error,
    /// Warnings and errors.
    Warn,
    /// Informational messages and above.
    Info,
    /// Debug messages and above.
    Debug,
    /// Trace (most verbose).
    Trace,
}

impl From<LogLevel> for tracing::Level {
    fn from(l: LogLevel) -> Self {
        match l {
            LogLevel::Error => tracing::Level::ERROR,
            LogLevel::Warn => tracing::Level::WARN,
            LogLevel::Info => tracing::Level::INFO,
            LogLevel::Debug => tracing::Level::DEBUG,
            LogLevel::Trace => tracing::Level::TRACE,
        }
    }
}

/// Top-level subcommands.
#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the full pipeline: collect → classify → report.
    Analyze(AnalyzeArgs),
    /// Collect commits from git repositories into the database (Stage 1).
    Collect(CollectArgs),
    /// Classify collected commits using the four-tier cascade (Stage 2).
    Classify(ClassifyArgs),
    /// Generate productivity reports from classified commits (Stage 3).
    Report(ReportArgs),
    /// Aggregate pull-request metrics per engineer.
    PrMetrics(PrMetricsArgs),
    /// Interactive configuration wizard for first-time setup.
    Install(InstallArgs),
    /// List, merge, or manage developer identity aliases.
    Aliases(AliasesArgs),
    /// Retroactive maintenance: re-run ticket-id extraction, effort scoring, or reachability.
    Backfill(BackfillArgs),
    /// Manage manual classification overrides (Tier 0 of the cascade).
    Override(OverrideArgs),
    /// Introspect or validate the active classification rule set.
    Rules(RulesArgs),
    /// DORA deployment-event ingestion and management.
    Deployments(DeploymentsSubcommandArgs),
    /// DORA incident ingestion and management.
    Incidents(IncidentsSubcommandArgs),
    /// Compute and display DORA metrics (lead time, deployment frequency, MTTR, CFR).
    Dora(DoraArgs),
}

/// Args wrapper for the `tga deployments` subcommand tree.
#[derive(Args, Debug)]
pub struct DeploymentsSubcommandArgs {
    /// `tga deployments` operation.
    #[command(subcommand)]
    pub subcommand: DeploymentsSubcommand,
}

/// `tga deployments` subcommand variants.
#[derive(Subcommand, Debug)]
pub enum DeploymentsSubcommand {
    /// Ingest deployment events into `fact_deployments`.
    Collect(DeploymentsCollectArgs),
}

/// Args wrapper for the `tga incidents` subcommand tree.
#[derive(Args, Debug)]
pub struct IncidentsSubcommandArgs {
    /// `tga incidents` operation.
    #[command(subcommand)]
    pub subcommand: IncidentsSubcommand,
}

/// `tga incidents` subcommand variants.
#[derive(Subcommand, Debug)]
pub enum IncidentsSubcommand {
    /// Ingest incidents into `fact_incidents`.
    Collect(IncidentsCollectArgs),
}

/// Arguments for `tga analyze`.
#[derive(Args, Debug)]
#[command(
    about = "Run the full pipeline: collect → classify → report.",
    long_about = "Run all three stages (collect, classify, report) in sequence against the\n\
configured repositories.\n\n\
This is the normal production command for routine analytics runs. Individual\n\
stages can be invoked separately (tga collect / tga classify / tga report) when\n\
you need surgical control over a single step.\n\n\
NOTE: --branch is available via `tga collect` when running stages individually.\n\
Use `tga analyze --skip-collect && tga collect --branch main` for branched runs.",
    after_help = "EXAMPLES:\n\
  # Standard weekly run (collect last 4 weeks, classify, report)\n\
  tga analyze --weeks 4\n\n\
  # Full history refresh after upgrading (re-collects all weeks)\n\
  tga analyze --force\n\n\
  # Skip slow collection; re-classify and regenerate reports only\n\
  tga analyze --skip-collect\n\n\
TIPS:\n\
  - Run `tga collect --branch main --force` first to restrict the corpus.\n\
  - Use --dry-run to preview collection work without database writes."
)]
pub struct AnalyzeArgs {
    /// Skip collection (use existing DB data).
    #[arg(long)]
    pub skip_collect: bool,
    /// Skip classification.
    #[arg(long)]
    pub skip_classify: bool,
    /// Output directory override.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Re-collect all weeks even if already present in the database.
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,
    /// Limit collection to the last N weeks of commits (overrides config start_date).
    #[arg(long, value_name = "N", conflicts_with_all = ["from", "to"])]
    pub weeks: Option<u32>,
    /// Start date for collection (ISO8601: YYYY-MM-DD). Mutually exclusive with --weeks.
    #[arg(long, value_name = "DATE", conflicts_with = "weeks")]
    pub from: Option<String>,
    /// End date for collection (ISO8601: YYYY-MM-DD). Defaults to today.
    #[arg(long, value_name = "DATE", conflicts_with = "weeks")]
    pub to: Option<String>,
    /// Skip the pre-walk `git fetch` step (use only local refs).
    #[arg(long, default_value_t = false)]
    pub no_fetch: bool,
    /// Perform all steps except writing to the database (log intent only).
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
    /// Run configuration validation and exit (0 on success, 1 on errors).
    #[arg(long, default_value_t = false)]
    pub validate_only: bool,
    /// Skip pre-flight configuration validation (use when paths are mounted
    /// dynamically by CI).
    #[arg(long, default_value_t = false)]
    pub no_validate: bool,
}

/// Arguments for `tga collect`.
#[derive(Args, Debug)]
#[command(
    about = "Collect commits from git repositories into the database (Stage 1).",
    long_about = "Walk configured git repositories and persist commit metadata, diff statistics,\n\
and ticket references into the SQLite database.\n\n\
tga 2.0.0 changed the default revwalk to cover ALL local branches and remote\n\
tracking refs (refs/heads/* + refs/remotes/origin/*). Use --head-only to restore\n\
the legacy HEAD-only walk, or --branch to restrict to specific branch names.\n\n\
Typical workflow:\n\
  tga collect --weeks 4          # collect last 4 weeks across all repos\n\
  tga collect --repos myrepo     # collect only one repo\n\
  tga collect --force            # re-collect all weeks (e.g. after upgrading)\n\
  tga classify                   # run Stage 2 after collection",
    after_help = "EXAMPLES:\n\
  # First-time setup: collect all history, then classify\n\
  tga collect && tga classify && tga report\n\n\
  # Incremental re-run scoped to a specific repo and branch\n\
  tga collect --repos my-service --branch main --weeks 2\n\n\
  # Recover missing branch commits after upgrading from tga <= 1.5.4\n\
  tga collect --force\n\n\
TIPS:\n\
  - Run `tga classify` immediately after `tga collect` to keep the DB in sync.\n\
  - Use `--weeks 4 --force` for routine weekly re-runs to refresh recent data.\n\
  - `--branch` is collect-only; commits in the DB do not carry branch attribution."
)]
pub struct CollectArgs {
    /// Only collect from these repository names (comma-separated, e.g. --repos api,frontend).
    ///
    /// When omitted, all repositories from the config file are processed.
    /// Repository names must match the `name` field in your YAML config
    /// (or the directory basename if `name` is absent).
    #[arg(long, value_delimiter = ',')]
    pub repos: Vec<String>,

    /// Restrict the revwalk to specific branch names (comma-separated).
    ///
    /// For each name in the list, the walk seeds from both
    /// `refs/heads/<name>` and `refs/remotes/origin/<name>` so that the
    /// local and remote-tracking copies are covered. If a branch does not
    /// exist in a repository, a warning is emitted but collection continues.
    ///
    /// Examples:
    ///   --branch main
    ///   --branch main,release/1.0,feature/x
    ///
    /// Mutually exclusive with --head-only. Use --branch to scope the walk;
    /// --head-only is the global legacy escape hatch for all repos.
    #[arg(
        long,
        value_delimiter = ',',
        conflicts_with = "head_only",
        value_name = "NAME[,NAME…]"
    )]
    pub branch: Vec<String>,

    /// Legacy alias for --from, accepted for backwards compatibility with
    /// scripts written against the Python `gitflow-analytics` predecessor.
    /// If both --from and --since are supplied, --from takes precedence.
    #[arg(long, hide = true)]
    pub since: Option<String>,
    /// Legacy alias for --to, accepted for backwards compatibility with
    /// scripts written against the Python `gitflow-analytics` predecessor.
    /// If both --to and --until are supplied, --to takes precedence.
    #[arg(long, hide = true)]
    pub until: Option<String>,
    /// Start date for collection (ISO8601: YYYY-MM-DD). Mutually exclusive with --weeks.
    #[arg(long, value_name = "DATE", conflicts_with = "weeks")]
    pub from: Option<String>,
    /// End date for collection (ISO8601: YYYY-MM-DD). Defaults to today. [default: today]
    #[arg(long, value_name = "DATE", conflicts_with = "weeks")]
    pub to: Option<String>,
    /// Re-collect all weeks even if already present in the database.
    ///
    /// Without --force, weeks that already have a row in `collection_runs` are
    /// skipped. Pass --force to re-walk all weeks (useful after upgrading tga
    /// or after a known data bug). Combine with --repos or --weeks to limit scope.
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,
    /// Limit collection to the last N weeks of commits (overrides config start_date).
    #[arg(long, value_name = "N", conflicts_with_all = ["from", "to"])]
    pub weeks: Option<u32>,
    /// Skip the pre-walk `git fetch` step (use only local refs). [default: false]
    ///
    /// WARNING: skipping fetch means the walk operates on whatever is already in
    /// your local object store. Commits pushed to the remote after the last local
    /// fetch will be silently absent from the results.
    #[arg(long, default_value_t = false)]
    pub no_fetch: bool,
    /// Exit non-zero if any repository's fetch failed (default: failures are
    /// visible in the summary but collection still exits 0). Useful for CI.
    #[arg(long, default_value_t = false)]
    pub strict_fetch: bool,
    /// Print a success line for every fetched repo in the fetch summary
    /// (default: only failures are printed). [default: false]
    #[arg(long, default_value_t = false)]
    pub verbose_fetch: bool,
    /// Perform all steps except writing to the database (log intent only).
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
    /// Re-fetch ADO pull requests even if they are already in the database.
    ///
    /// Use this to backfill rows persisted before v1.0.9 that have
    /// `commit_shas = '[]'`. [default: false]
    #[arg(long, default_value_t = false)]
    pub force_refresh_prs: bool,
    /// Skip the post-collection tag and release-branch reachability scan.
    ///
    /// When set, `fact_commit_reachability` rows for `on_any_tag`,
    /// `reachable_from_tags`, `on_release_branch`, and `release_branches`
    /// are not populated. Useful for trunk-based repos or to cut collection
    /// time on repositories with thousands of tags. The scan defaults ON
    /// because its cost is bounded by the number of refs in the repo.
    #[arg(long, default_value_t = false)]
    pub skip_tag_reachability: bool,
    /// Run configuration validation and exit (0 on success, 1 on errors).
    #[arg(long, default_value_t = false)]
    pub validate_only: bool,
    /// Skip pre-flight configuration validation (use when paths are mounted
    /// dynamically by CI).
    #[arg(long, default_value_t = false)]
    pub no_validate: bool,
    /// Restore legacy HEAD-only revwalk for all repositories.
    ///
    /// tga 2.0.0 changed the default to walk ALL local branches and remote
    /// tracking refs (refs/heads/* + refs/remotes/origin/*), fixing a
    /// data-integrity bug where commits on non-default branches (PR branches,
    /// feature branches, hotfixes) were silently excluded (#331).
    ///
    /// Pass --head-only to revert to the 1.x HEAD-only walk for all repos.
    /// For a per-repo opt-out, set `head_only: true` in the repository entry
    /// in your YAML config.
    ///
    /// NOTE: existing tga.db files collected with tga <= 1.5.4 are missing
    /// commits from non-default branches.  Run `tga collect --force` after
    /// upgrading to 2.0.0 to recover that history.
    ///
    /// Mutually exclusive with --branch.
    #[arg(long, default_value_t = false, conflicts_with = "branch")]
    pub head_only: bool,
}

/// Arguments for `tga classify`.
///
/// # Rule / category interaction (issue #259)
///
/// When `--rules` is supplied, the provided YAML file is treated as a
/// **complete** ruleset — built-in TGA rules are NOT merged unless the file
/// explicitly sets `extend_defaults: true`. Custom rules default to
/// `priority: 110`, which places them above the built-in conventional-commit
/// tier (`priority: 100`) so they win without requiring an explicit
/// `priority:` on every entry.
///
/// To register custom category names for rollup reporting, add a
/// `custom_categories:` block to `config.yaml`:
///
/// ```yaml
/// classification:
///   custom_categories:
///     - name: "bug_fix"
///       parent: "bugfix"
///     - name: "new_feature"
///       parent: "feature"
///     - name: "tech_debt_refactoring"
///       parent: "maintenance"
/// ```
///
/// # Multi-source classification (issue #260)
///
/// External ticket sources (JIRA, GitHub Issues) are configured under the
/// `classification.sources:` block in `config.yaml`. They are consulted
/// between the manual-override tier and custom rules. Pass `--no-external`
/// to disable all external lookups (useful in CI or offline environments).
#[derive(Args, Debug)]
#[command(
    about = "Classify collected commits using the four-tier cascade (Stage 2).",
    long_about = "Run the classification cascade over commits already in the database.\n\n\
The cascade applies rules in this order:\n\
  Tier 0 -- manual overrides (tga override add)\n\
  Tier 1 -- external ticket sources (JIRA, GitHub Issues, Linear, ADO)\n\
  Tier 2 -- commit-message regex rules (built-in + custom --rules file)\n\
  Tier 3 -- LLM fallback (requires --use-llm or config.classification.use_llm)\n\n\
By default, commits that already have a classification are skipped for\n\
efficiency. Pass --force to re-classify a slice (e.g. after a rule update).\n\n\
NOTE: --branch is collect-only. Commits in the DB do not carry branch\n\
attribution after the walk, so there is no branch filter here.",
    after_help = "EXAMPLES:\n\
  # Classify all unclassified commits (normal incremental run)\n\
  tga classify\n\n\
  # Re-classify commits in the last 8 weeks after updating rules\n\
  tga classify --force --since 2026-01-01\n\n\
  # Re-classify only the last 4 weeks for one repo\n\
  tga classify --force --repos my-service --weeks 4\n\n\
TIPS:\n\
  - After updating your rules file, run `tga classify --force` to reprocess.\n\
  - Use `--no-external` in CI to skip network calls to JIRA/GitHub Issues."
)]
pub struct ClassifyArgs {
    /// Only re-classify commits from these repository names (comma-separated).
    ///
    /// When omitted, all repositories in the database are processed.
    /// Matches against the `repository` column in the `commits` table.
    #[arg(long, value_delimiter = ',')]
    pub repos: Vec<String>,

    /// Scope re-classification to commits in the last N ISO weeks.
    ///
    /// Combines with --force: only already-classified commits in the N-week
    /// window are rewritten. Without --force, restricts the set of unclassified
    /// commits considered. Mutually exclusive with --since/--until.
    #[arg(long, value_name = "N", conflicts_with_all = ["since", "until"])]
    pub weeks: Option<u32>,

    /// Scope re-classification to commits on or after this date (ISO8601: YYYY-MM-DD).
    ///
    /// Restricts the set of commits processed. With --force, only already-classified
    /// commits in the date range are rewritten. Mutually exclusive with --weeks.
    #[arg(long, value_name = "DATE", conflicts_with = "weeks")]
    pub since: Option<String>,

    /// Scope re-classification to commits on or before this date (ISO8601: YYYY-MM-DD).
    ///
    /// Upper bound on the author timestamp window. Mutually exclusive with --weeks.
    #[arg(long, value_name = "DATE", conflicts_with = "weeks")]
    pub until: Option<String>,

    /// Rules file override.
    ///
    /// Custom classification rules file. Set `extend_defaults: true` in the
    /// file to merge built-in TGA rules alongside your custom rules.
    ///
    /// Note: external sources (JIRA/GitHub Issues) are configured separately
    /// under `classification.sources` in `config.yaml`, NOT in this rules
    /// file. Use `--no-external` to suppress all external lookups at runtime.
    #[arg(long)]
    pub rules: Option<PathBuf>,
    /// Enable LLM fallback (overrides config). [default: false]
    #[arg(long)]
    pub use_llm: bool,
    /// Backfill missing complexity scores (1–5) for already-classified
    /// commits via the LLM, without re-running the full classification.
    ///
    /// Only rows with `complexity IS NULL` and a non-`exact_rule` method
    /// are updated; category, confidence, and method are left untouched.
    #[arg(long)]
    pub backfill_complexity: bool,
    /// Re-classify commits that already have a `classification_id`.
    ///
    /// Without this flag, `tga classify` skips any commit that already
    /// carries a verdict — useful for incremental runs but a footgun when
    /// the rule set is updated. With `--force`, every matching commit is
    /// re-classified and its existing `classifications` row is replaced
    /// (no orphan rows). Combine with --since/--until or --weeks to bound
    /// the rewrite to a specific window.
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,
    /// Disable all external classification sources (JIRA, GitHub Issues).
    ///
    /// When set, external ticket lookups configured under `sources:` in the
    /// rules file are skipped. Useful for CI environments without network
    /// access or when API credentials are not available. The classification
    /// cascade falls through to commit-message rules and LLM as normal.
    #[arg(long, default_value_t = false)]
    pub no_external: bool,
}

/// Arguments for `tga report`.
#[derive(Args, Debug)]
#[command(
    about = "Generate productivity reports from classified commits (Stage 3).",
    long_about = "Produce CSV, JSON, and/or Markdown reports from the classified commits\n\
already in the database. Reports aggregate metrics by author, week, and category.\n\n\
Use --author to drill down to a single engineer's output.\n\
Use --formats to select one or more output formats.\n\n\
NOTE: --branch and --repos are collect-level concepts. The report reads\n\
whatever is in the database; filter at collection time if needed.",
    after_help = "EXAMPLES:\n\
  # Generate all formats for the full team\n\
  tga report --formats csv,json,markdown\n\n\
  # Per-engineer drill-down\n\
  tga report --author alice@example.com\n\n\
  # Write reports to a custom directory\n\
  tga report --output ./reports --formats markdown\n\n\
TIPS:\n\
  - Use `tga aliases list` to find canonical email addresses for --author.\n\
  - Reports cover all classified data in the DB; re-run classify first if\n\
    recent commits are unclassified."
)]
pub struct ReportArgs {
    /// Output directory override. [default: ./output]
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Output formats (csv, json, markdown — comma-separated). [default: all formats]
    #[arg(long, value_delimiter = ',')]
    pub formats: Vec<String>,
    /// Scope the report to a single canonical identity.
    ///
    /// Must match the `canonical_email` field in the `authors` table
    /// (case-insensitive). If the email is not found, `tga report`
    /// exits non-zero and suggests `tga aliases list`.
    #[arg(long, value_name = "EMAIL")]
    pub author: Option<String>,
}

/// Run config validation and decide whether the caller should exit.
///
/// Returns `Ok(true)` when the caller should exit cleanly after this call
/// — i.e. `--validate-only` was passed and validation succeeded. Returns
/// `Ok(false)` to continue with command execution. Returns `Err` when
/// validation produced errors *and* `--no-validate` was not set; the
/// errors are also printed to stderr for the user.
fn run_validation(config: &Config, no_validate: bool, validate_only: bool) -> anyhow::Result<bool> {
    if no_validate {
        if validate_only {
            tracing::warn!("--no-validate overrides --validate-only; exiting without checks");
            return Ok(true);
        }
        tracing::debug!("--no-validate: skipping configuration pre-flight checks");
        return Ok(false);
    }

    let errors = ConfigValidator::new(config).validate();
    if errors.is_empty() {
        if validate_only {
            println!("Configuration OK.");
            return Ok(true);
        }
        return Ok(false);
    }

    eprintln!("Configuration validation found {} error(s):", errors.len());
    for e in &errors {
        eprintln!("  - {e}");
    }
    Err(anyhow::anyhow!(
        "configuration validation failed ({} error(s)); use --no-validate to skip",
        errors.len()
    ))
}

/// Bundled declarative help config (issue #216). Loaded once per process.
///
/// Why: every standalone trusty-* binary embeds its `help.yaml` via
/// `include_str!` so the workspace-shared `trusty_common::help::suggest`
/// helper can propose corrections for typos in unknown subcommands.
/// What: `LazyLock<HelpConfig>` parsed from `help.yaml` at first access.
/// Test: parse coverage lives in `trusty-common`; this site is exercised
/// manually via `tga analize`.
static HELP: std::sync::LazyLock<trusty_common::help::HelpConfig> =
    std::sync::LazyLock::new(|| {
        trusty_common::help::load_help(include_str!("../help.yaml"))
            .expect("tga help.yaml is bundled and valid")
    });

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Why: parse via `try_parse` so we can attach the workspace-shared
    // "did you mean?" suggestion (issue #216) before exiting on a clap error.
    let argv: Vec<String> = std::env::args().collect();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            e.print().ok();
            if matches!(
                e.kind(),
                clap::error::ErrorKind::InvalidSubcommand | clap::error::ErrorKind::UnknownArgument
            ) {
                trusty_common::help::print_suggestion_hint(&argv, &HELP);
            }
            std::process::exit(e.exit_code());
        }
    };

    // Initialize tracing. Precedence: RUST_LOG env var > --log flag > -v count.
    // Default (no flags) is WARN.
    let level: tracing::Level = if let Some(l) = cli.log {
        l.into()
    } else {
        match cli.verbose {
            0 => tracing::Level::WARN,
            1 => tracing::Level::INFO,
            2 => tracing::Level::DEBUG,
            _ => tracing::Level::TRACE,
        }
    };
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level.to_string()));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    // Load configuration (fall back to default if file is missing).
    let config = if cli.config.exists() {
        tracing::info!(path = %cli.config.display(), "loading config");
        Config::load(&cli.config)?
    } else {
        tracing::warn!(
            "config file {} not found, using defaults",
            cli.config.display()
        );
        Config::default()
    };

    // `tga install` does not require an open database — it just writes a
    // config file. Handle it before the DB open call so a missing/locked
    // `tga.db` cannot block bootstrapping a fresh project.
    if let Commands::Install(args) = cli.command {
        return commands::install::run(config, args);
    }

    // Pre-flight validation for the long-running commands. `--validate-only`
    // exits with status 0 on success or 1 on errors before opening the DB.
    // `--no-validate` skips the check entirely (for CI environments that
    // mount paths dynamically).
    let should_short_circuit = match &cli.command {
        Commands::Analyze(args) => run_validation(&config, args.no_validate, args.validate_only)?,
        Commands::Collect(args) => run_validation(&config, args.no_validate, args.validate_only)?,
        _ => false,
    };
    if should_short_circuit {
        return Ok(());
    }

    // Open SQLite database (runs migrations on open).
    tracing::info!(path = %cli.database.display(), "opening database");
    let mut db = Database::open(&cli.database)?;

    match cli.command {
        Commands::Analyze(args) => commands::analyze::run(config, &mut db, args).await?,
        Commands::Collect(args) => commands::collect::run(config, &mut db, args).await?,
        Commands::Classify(args) => commands::classify::run(config, &mut db, args).await?,
        Commands::Report(args) => commands::report::run(config, &db, args)?,
        Commands::PrMetrics(args) => commands::pr_metrics::run(config, &db, args)?,
        Commands::Aliases(args) => commands::aliases::run(config, &mut db, args)?,
        Commands::Backfill(args) => commands::backfill::run(config, &mut db, args)?,
        Commands::Override(args) => commands::override_cmd::run(config, &mut db, args)?,
        Commands::Rules(args) => commands::rules::run(config, &db, args)?,
        Commands::Deployments(args) => match args.subcommand {
            DeploymentsSubcommand::Collect(a) => {
                commands::deployments::run(config, &mut db, a).await?
            }
        },
        Commands::Incidents(args) => match args.subcommand {
            IncidentsSubcommand::Collect(a) => commands::incidents::run(config, &mut db, a)?,
        },
        Commands::Dora(args) => commands::dora::run(config, &mut db, args)?,
        // Handled above — match is exhaustive.
        Commands::Install(_) => unreachable!("install dispatched above"),
    }

    Ok(())
}
