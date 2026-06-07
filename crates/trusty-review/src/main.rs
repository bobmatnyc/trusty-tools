//! `trusty-review` CLI entry point.
//!
//! Why: provides the user-facing interface for running, comparing, inspecting
//! PR reviews, and generating longitudinal contributor profiles.
//!
//! What: parses flags via clap-derive, resolves config, and dispatches to the
//! appropriate subcommand handler.  All heavy logic lives in `commands/`.
//! STDOUT stays clean (only review output); all tracing goes to stderr.
//!
//! Test: `cargo run -p trusty-review -- --help` must succeed; each subcommand
//! is tested in its own module under `commands/`.

#[cfg(feature = "profile")]
mod cli_profile;
mod cli_verify;
mod commands;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};

use trusty_review::config::ReviewConfig;

use commands::compare::{CompareArgs, cmd_compare};
use commands::run::{RunArgs, cmd_run};
#[cfg(feature = "http-server")]
use commands::serve::{ServeArgs, cmd_serve};

// ─── CLI top-level ────────────────────────────────────────────────────────────

/// trusty-review — fast local PR-review service
///
/// An LLM-backed code reviewer that fetches PR diffs, retrieves code context
/// from trusty-search, and produces structured review verdicts.
///
/// All reviews are dry-run by default (no comments posted to GitHub).
#[derive(Debug, Parser)]
#[command(
    name = "trusty-review",
    version = env!("CARGO_PKG_VERSION"),
    about = "Fast local PR-review service — LLM-backed code review",
    long_about = None,
)]
struct Cli {
    /// Path to the TOML configuration file.
    /// Default: $XDG_CONFIG_HOME/trusty-review/config.toml
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

// ─── Subcommands ──────────────────────────────────────────────────────────────

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run a single PR review with the default (or overridden) reviewer model.
    ///
    /// Fetches the PR diff from GitHub and runs the LLM review pipeline.
    /// Always dry-run in the MVP (no comment posted to GitHub).
    ///
    /// Use --local-diff to review a local unified diff file without GitHub.
    Run(RunArgs),

    /// Compare the same PR across multiple models to evaluate speed/cost/quality.
    ///
    /// Runs the review pipeline once per model in the compare set (or --models
    /// override) and prints a comparison table.  Always dry-run.
    Compare(CompareArgs),

    /// Start the long-lived HTTP webhook server (port 7880 by default).
    ///
    /// Exposes:
    ///   GET  /health                  — liveness + dep status
    ///   GET  /status                  — in-flight count + last error
    ///   POST /review                  — synchronous on-demand review (dry-run)
    ///   POST /pr/github/webhook       — GitHub PR webhook (HMAC-validated)
    ///
    /// Pass --stdio to run as a MCP JSON-RPC stdio service instead.
    ///
    /// All reviews are dry-run (no comments posted to GitHub).
    /// Graceful shutdown on SIGTERM/SIGINT (in-flight requests are drained).
    #[cfg(feature = "http-server")]
    Serve(ServeArgs),

    /// Generate a longitudinal contributor-quality profile.
    ///
    /// Aggregates commit history from a tga SQLite DB into period batches,
    /// samples representative diffs, and uses an LLM to identify recurring
    /// findings and write a narrative.  Output: profile.json + profile.md.
    ///
    /// Always dry-run safe: never posts PR comments.  Use --github-issue to
    /// opt-in to creating/updating a per-contributor GitHub issue thread.
    ///
    /// Requires the `profile` Cargo feature (enabled by default).
    #[cfg(feature = "profile")]
    Profile(cli_profile::ProfileArgs),
}

// ─── Entry point ──────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    // Tracing to stderr — never stdout (stdout is reserved for review output
    // and, in --stdio mode, for the MCP JSON-RPC transport).
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    let rt = tokio::runtime::Runtime::new().context("build tokio runtime")?;
    rt.block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> Result<()> {
    let config = ReviewConfig::from_env_and_file(cli.config.as_deref(), None);

    match cli.command {
        Commands::Run(args) => cmd_run(config, args).await,
        Commands::Compare(args) => cmd_compare(config, args).await,
        #[cfg(feature = "http-server")]
        Commands::Serve(args) => cmd_serve(config, args).await,
        #[cfg(feature = "profile")]
        Commands::Profile(args) => cli_profile::cmd_profile(config, args).await,
    }
}
