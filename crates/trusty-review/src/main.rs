//! `trusty-review` CLI entry point.
//!
//! Why: provides the user-facing interface for running, comparing and
//! inspecting PR reviews, and running the calibration harness (#1422).
//!
//! What: parses flags via clap-derive, resolves config, and dispatches to the
//! appropriate subcommand handler.  All heavy logic lives in `commands/`.
//! STDOUT stays clean (only review output); all tracing goes to stderr.
//!
//! Test: `cargo run -p trusty-review -- --help` must succeed; each subcommand
//! is tested in its own module under `commands/`.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

#[cfg(feature = "report")]
mod cli_report;
mod cli_verify;
mod commands;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};

use trusty_review::config::ReviewConfig;

use commands::calibrate::{CalibrateArgs, cmd_calibrate};
use commands::compare::{CompareArgs, cmd_compare};
use commands::port::{PortFormat, handle_port};
use commands::run::{RunArgs, cmd_run};
#[cfg(feature = "http-server")]
use commands::serve::{ServeArgs, cmd_serve};

// ─── CLI top-level ────────────────────────────────────────────────────────────

/// trusty-review — fast local PR-review service
///
/// An LLM-backed code reviewer that fetches PR diffs, retrieves code context
/// from trusty-search, and produces structured review verdicts.
///
/// Reviews are dry-run by default (no comments posted to GitHub). `run` and
/// `serve` post live when posting is enabled — set `PR_INTELLIGENCE_DRY_RUN=false`
/// to allow live PR comments.
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
    /// Dry-run by default (no comment posted). Posts the review live to the PR
    /// when posting is enabled — set `PR_INTELLIGENCE_DRY_RUN=false` to allow it.
    ///
    /// Use --local-diff to review a local unified diff file without GitHub
    /// (pass `-` to read the diff from stdin instead of a file), or --base
    /// [--head] to review an arbitrary local git ref range (`git diff -M
    /// <base>...<head>`; --head defaults to HEAD). All three local sources
    /// are always dry-run — they can never post (#2993).
    Run(RunArgs),

    /// Compare the same PR across multiple models to evaluate speed/cost/quality.
    ///
    /// Runs the review pipeline once per model in the compare set (or --models
    /// override) and prints a comparison table.  Always dry-run.  Accepts the
    /// same --local-diff / --base [--head] local-diff flags as `run` (#2993).
    Compare(CompareArgs),

    /// Report the listening port of the running trusty-review daemon.
    ///
    /// Reads the `http_addr` discovery file written at daemon bind time and
    /// prints the address in one of three machine-readable formats:
    ///
    ///   trusty-review port          → bare port:  7891
    ///   trusty-review port --addr   → host:port:  127.0.0.1:7891
    ///   trusty-review port --json   → JSON:       {"addr":"127.0.0.1","port":7891}
    ///
    /// Exits non-zero when no daemon discovery file is found so shell
    /// substitution (`$(trusty-review port)`) fails safely.
    Port {
        /// Print `host:port` instead of the bare port number.
        #[arg(long, conflicts_with = "json")]
        addr: bool,

        /// Print a JSON object `{"addr":"…","port":…}`.
        #[arg(long, conflicts_with = "addr")]
        json: bool,
    },

    /// Start the long-lived HTTP server (port 7891 by default).
    ///
    /// Exposes:
    ///   GET  /health                  — liveness + dep status
    ///   GET  /status                  — in-flight count + last error
    ///   POST /review                  — synchronous on-demand review
    ///
    /// GitHub webhooks do not arrive here. Run `webhook-listen` for those:
    /// `trusty-console` verifies the delivery and relays it over UDS (#5181).
    ///
    /// Pass --stdio to run as a MCP JSON-RPC stdio service instead.
    ///
    /// Dry-run by default (no comments posted). Posts reviews live to the PR
    /// when posting is enabled — set `PR_INTELLIGENCE_DRY_RUN=false` to allow it.
    /// Graceful shutdown on SIGTERM/SIGINT (in-flight requests are drained).
    #[cfg(feature = "http-server")]
    Serve(ServeArgs),

    /// Generate a deterministic technical due-diligence report from a manifest.
    ///
    /// Loads a TOML manifest naming one or more target repositories, enriches
    /// each local checkout with git provenance, consumes pre-produced
    /// trusty-analyze metrics JSON, and fills a bundled report template — writing
    /// a `{slug}.md` / `{slug}.json` pair to the output directory.
    ///
    /// Deterministic only (M1): no LLM synthesis.  Any placeholder with no source
    /// value renders as `not stated in source data` (never invented).
    ///
    /// Requires the `report` Cargo feature (enabled by default).
    #[cfg(feature = "report")]
    Report(cli_report::ReportArgs),

    /// Run the calibration harness against a human-reviewed PR corpus (#1422).
    ///
    /// Loads a JSONL corpus (one CorpusEntry JSON object per line), runs the
    /// review pipeline in dry-run mode for each PR, fuzzy-matches trusty findings
    /// against human findings by (file, kind), and emits a JSON report:
    ///
    ///   {recall, precision, per_pr:[{pr, recall, precision, false_positives:[...]}],
    ///    rust_semantic_fp_rate}
    ///
    /// `rust_semantic_fp_rate` measures precision of logic-error/ownership findings
    /// on `.rs` files — the known Rust false-positive hotspot.
    ///
    /// Always dry-run safe: never posts to GitHub.
    Calibrate(CalibrateArgs),

    /// Manage inference provider configuration (API keys) — the universal
    /// `config keys set/list/test/unset` surface shared by every trusty-*
    /// binary (epic #2400 Wave 1, #2405).
    Config(trusty_common::inference::config::ConfigCommand),

    /// Serve the console webhook relay over a Unix socket, then exit (#5182).
    ///
    /// Binds `trusty-review-webhook.sock` in the hardened scratch directory —
    /// the socket `trusty-console` has been relaying to since #5089 step 3 —
    /// writes each verified delivery to a durable inbox, and acknowledges only
    /// after that write is fsync'd. The ack is what lets console delete its own
    /// copy, so acking earlier would lose the delivery outright.
    ///
    /// Not a resident daemon: console spawns this on demand and SIGTERMs it.
    /// Run it by hand only to inspect the socket.
    WebhookListen,

    /// Manage the macOS launchd LaunchAgent for the review daemon (#2557).
    ///
    /// `install` writes `~/Library/LaunchAgents/com.trusty.trusty-review.plist`
    /// (running `trusty-review serve`) and bootstraps it; `uninstall` unloads
    /// and removes it; `status` / `logs` inspect the running agent. macOS-only.
    /// `tctl install` / `tctl start` call `install` on the operator's behalf.
    /// Requires the `http-server` feature (enabled by default) since it
    /// daemonizes `trusty-review serve`.
    #[cfg(feature = "http-server")]
    Service {
        #[command(subcommand)]
        action: commands::service::ServiceAction,
    },
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

    // `port` is synchronous — dispatch before the async runtime to keep the
    // binary start-up cost minimal for machine-readable invocations.
    if let Commands::Port { addr, json } = &cli.command {
        let format = if *json {
            PortFormat::Json
        } else if *addr {
            PortFormat::Addr
        } else {
            PortFormat::Port
        };
        return handle_port(format);
    }

    // `service` drives macOS launchd (`launchctl`) synchronously — dispatch
    // before the async runtime so `tctl install`/`tctl start` hooks pay no
    // tokio start-up cost for a plist write.
    #[cfg(feature = "http-server")]
    if let Commands::Service { action } = &cli.command {
        return commands::service::run_service_action(action);
    }

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
        #[cfg(feature = "report")]
        Commands::Report(args) => cli_report::cmd_report(config, args).await,
        Commands::Calibrate(args) => cmd_calibrate(config, args).await,
        Commands::WebhookListen => trusty_review::webhook_listener::run(config).await,
        Commands::Config(cmd) => cmd.run().await,
        // Port is handled synchronously in `main` before this function is
        // called; this arm is unreachable at runtime but required for
        // exhaustive match.
        Commands::Port { .. } => unreachable!("port dispatched before async_main"),
        // `service` is likewise dispatched synchronously in `main`.
        #[cfg(feature = "http-server")]
        Commands::Service { .. } => unreachable!("service dispatched before async_main"),
    }
}
