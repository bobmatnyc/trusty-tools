//! `tga` — the trusty-git-analytics command-line binary.
//!
//! Wires together the library modules (`core`, `collect`, `classify`,
//! `report`) behind a clap subcommand interface.

#![warn(missing_docs)]

mod commands;

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use tracing_subscriber::EnvFilter;

use tga::core::config::{database_path, Config, ConfigValidator};
use tga::core::db::Database;

use crate::commands::aliases::AliasesArgs;
use crate::commands::args::{
    AnalyzeArgs, ClassifyArgs, CollectArgs, DeploymentsSubcommand, DeploymentsSubcommandArgs,
    IncidentsSubcommand, IncidentsSubcommandArgs, ReportArgs,
};
use crate::commands::author::AuthorArgs;
use crate::commands::backfill::BackfillArgs;
use crate::commands::dora::DoraArgs;
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
        Architecture decisions are documented in docs/trusty-git-analytics/decisions/. See \
        docs/trusty-git-analytics/decisions/README.md for the format and process.",
    version,
    propagate_version = true
)]
struct Cli {
    /// Path to config file (default: ./config.yaml).
    #[arg(short, long, default_value = "config.yaml", global = true)]
    config: PathBuf,

    /// Path to SQLite database.
    ///
    /// Precedence (highest first):
    /// 1. This flag, when explicitly supplied.
    /// 2. `database:` field in the YAML config file.
    /// 3. Default `tga.db`, resolved relative to the config file's directory
    ///    (or the current directory when no config file is loaded).
    ///
    /// Relative paths in options 2 and 3 are anchored to the config file's
    /// directory, not to cwd — this ensures cron/launchd jobs running from an
    /// arbitrary working directory still open the correct database.
    /// Absolute paths and `~`-prefixed paths are never modified.
    #[arg(short, long, global = true)]
    database: Option<PathBuf>,

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
    /// Per-engineer drill-down report for a single canonical identity.
    Author(AuthorArgs),
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

/// Process entry point.
///
/// Why: a plain `#[tokio::main]` builds a multi-threaded runtime and, on
/// return, drops it — which blocks until every background task the runtime
/// spawned has finished. `reqwest`'s default `Client` keeps idle HTTP
/// keep-alive connections in a pool whose reaper runs as such a background
/// task; against a live server (e.g. JIRA/Atlassian) those sockets linger up
/// to the pool idle timeout (~90s). The result is that `tga classify` with
/// external sources enabled finishes all work, prints its summary, then hangs
/// ~120s at exit waiting on the runtime drop before the process finally
/// terminates (issue #397, bug 1).
/// What: builds the multi-threaded runtime explicitly, runs the async body to
/// completion, then calls [`tokio::runtime::Runtime::shutdown_timeout`] with a
/// zero deadline so any still-idle connection-pool tasks are dropped
/// immediately instead of blocking the process exit. All real work is already
/// awaited inside `run`, so nothing useful is discarded.
/// Test: `cargo test -p tga` (the existing suite) plus the manual repro in the
/// issue; clean-exit timing is host-dependent so it is not unit-asserted.
fn main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run());
    // Drop idle background tasks (e.g. reqwest's keep-alive connection pool)
    // immediately rather than waiting on the runtime's default drop, which
    // would block until those tasks wind down on their own.
    runtime.shutdown_timeout(std::time::Duration::from_secs(0));
    result
}

/// Async program body, invoked by [`main`] on an explicit runtime.
///
/// Why: split out from `main` so the runtime can be shut down with a bounded
/// timeout after the body completes (see [`main`]).
/// What: parses CLI args, initializes tracing/config, opens the DB, and
/// dispatches the chosen subcommand.
/// Test: exercised end-to-end by every CLI invocation; per-command behavior is
/// covered by each command module's tests.
async fn run() -> anyhow::Result<()> {
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

    // Update check: tga has no MCP stdio transport — all subcommands are
    // human-facing interactive CLI commands where a release notice is
    // appropriate. The check is throttled to once per 24 h (on-disk cache) so
    // on a typical run this costs only a sub-millisecond cache file read.
    if let Some(info) =
        trusty_common::update::check_throttled(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
            .await
    {
        eprintln!("{}", trusty_common::update::notice(&info));
    }

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

    // Resolve the effective database path (issues #406, #620):
    //   1. Explicit --database CLI flag wins (absolute path from user).
    //   2. `database:` field in the YAML config, anchored to config dir.
    //   3. Default `tga.db`, anchored to config dir when known; falls back to
    //      cwd-relative only when no config file was loaded.
    //
    // Anchoring to config dir (not cwd) is critical for cron/launchd jobs
    // that run with cwd=/ or some unrelated directory — without anchoring
    // the binary silently opens/creates a ghost db at the wrong path.
    let db_path = cli
        .database
        .or_else(|| config.resolved_database_path())
        .unwrap_or_else(|| database_path::default_path(config.config_dir()));

    // Open SQLite database (runs migrations on open).
    tracing::info!(path = %db_path.display(), "opening database");
    let mut db = Database::open(&db_path)?;

    match cli.command {
        Commands::Author(args) => commands::author::run(config, &db, args)?,
        Commands::Analyze(args) => commands::analyze::run(config, &mut db, args).await?,
        Commands::Collect(args) => commands::collect::run(config, &mut db, args).await?,
        Commands::Classify(args) => commands::classify::run(config, &mut db, args).await?,
        Commands::Report(args) => commands::report::run(config, &db, args)?,
        Commands::PrMetrics(args) => commands::pr_metrics::run(config, &db, args)?,
        Commands::Aliases(args) => commands::aliases::run(config, &mut db, args)?,
        Commands::Backfill(args) => commands::backfill::run(config, &mut db, args).await?,
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
