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
use crate::commands::install::InstallArgs;
use crate::commands::override_cmd::OverrideArgs;
use crate::commands::pr_metrics::PrMetricsArgs;

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
    /// Stage 1: collect commits from git repositories.
    Collect(CollectArgs),
    /// Stage 2: classify collected commits.
    Classify(ClassifyArgs),
    /// Stage 3: generate reports from classified commits.
    Report(ReportArgs),
    /// Aggregate pull-request metrics per engineer.
    PrMetrics(PrMetricsArgs),
    /// Interactive configuration wizard.
    Install(InstallArgs),
    /// List or merge developer identities (aliases).
    Aliases(AliasesArgs),
    /// Retroactive maintenance operations on existing commit rows.
    Backfill(BackfillArgs),
    /// Manage manual classification overrides (Tier 0).
    Override(OverrideArgs),
}

/// Arguments for `tga analyze`.
#[derive(Args, Debug)]
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
pub struct CollectArgs {
    /// Only collect from these repository names (comma-separated).
    #[arg(long, value_delimiter = ',')]
    pub repos: Vec<String>,
    /// Legacy alias for --from, accepted for backwards compatibility with
    /// scripts written against the Python `gitflow-analytics` predecessor.
    /// If both --from and --since are supplied, --from takes precedence.
    #[arg(long)]
    pub since: Option<String>,
    /// Legacy alias for --to, accepted for backwards compatibility with
    /// scripts written against the Python `gitflow-analytics` predecessor.
    /// If both --to and --until are supplied, --to takes precedence.
    #[arg(long)]
    pub until: Option<String>,
    /// Start date for collection (ISO8601: YYYY-MM-DD). Mutually exclusive with --weeks.
    #[arg(long, value_name = "DATE", conflicts_with = "weeks")]
    pub from: Option<String>,
    /// End date for collection (ISO8601: YYYY-MM-DD). Defaults to today.
    #[arg(long, value_name = "DATE", conflicts_with = "weeks")]
    pub to: Option<String>,
    /// Re-collect all weeks even if already present in the database.
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,
    /// Limit collection to the last N weeks of commits (overrides config start_date).
    #[arg(long, value_name = "N", conflicts_with_all = ["from", "to"])]
    pub weeks: Option<u32>,
    /// Skip the pre-walk `git fetch` step (use only local refs).
    #[arg(long, default_value_t = false)]
    pub no_fetch: bool,
    /// Perform all steps except writing to the database (log intent only).
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
    /// Re-fetch ADO pull requests even if they are already in the database.
    /// Use this to backfill rows persisted before v1.0.9 that have
    /// `commit_shas = '[]'`.
    #[arg(long, default_value_t = false)]
    pub force_refresh_prs: bool,
    /// Run configuration validation and exit (0 on success, 1 on errors).
    #[arg(long, default_value_t = false)]
    pub validate_only: bool,
    /// Skip pre-flight configuration validation (use when paths are mounted
    /// dynamically by CI).
    #[arg(long, default_value_t = false)]
    pub no_validate: bool,
}

/// Arguments for `tga classify`.
#[derive(Args, Debug)]
pub struct ClassifyArgs {
    /// Rules file override.
    #[arg(long)]
    pub rules: Option<PathBuf>,
    /// Enable LLM fallback (overrides config).
    #[arg(long)]
    pub use_llm: bool,
    /// Backfill missing complexity scores (1–5) for already-classified
    /// commits via the LLM, without re-running the full classification.
    ///
    /// Only rows with `complexity IS NULL` and a non-`exact_rule` method
    /// are updated; category, confidence, and method are left untouched.
    #[arg(long)]
    pub backfill_complexity: bool,
}

/// Arguments for `tga report`.
#[derive(Args, Debug)]
pub struct ReportArgs {
    /// Output directory override.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Output formats (csv, json, markdown — comma-separated).
    #[arg(long, value_delimiter = ',')]
    pub formats: Vec<String>,
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
        // Handled above — match is exhaustive.
        Commands::Install(_) => unreachable!("install dispatched above"),
    }

    Ok(())
}
