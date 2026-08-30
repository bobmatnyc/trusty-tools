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
#[cfg(feature = "mcp")]
use commands::mcp_stdio::{McpArgs, cmd_mcp_stdio};
use commands::run::{RunArgs, cmd_run};

// ─── CLI top-level ────────────────────────────────────────────────────────────

/// trusty-review — fast local PR-review service
///
/// An LLM-backed code reviewer that fetches PR diffs, retrieves code context
/// from trusty-search, and produces structured review verdicts.
///
/// Reviews are dry-run by default (no comments posted to GitHub). `run` posts
/// live only when `--live` is passed explicitly — the ambient
/// `PR_INTELLIGENCE_DRY_RUN` env var alone can never enable posting for this
/// command (#4460).
///
/// #6290: there is no review daemon. Every review is one invocation of this
/// binary; `run --json` returns the same structured result the retired
/// `review.run` method did.
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
    /// Dry-run by default (no comment posted). Pass `--live` to post the
    /// review to the PR — the ambient `PR_INTELLIGENCE_DRY_RUN` env var alone
    /// can never enable posting (#4460); the resolved mode is printed before
    /// any work begins.
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

    /// Run the MCP JSON-RPC 2.0 stdio service.
    ///
    /// stdout is the JSON-RPC transport; every log line goes to stderr. Wire it
    /// into Claude Code via .mcp.json:
    ///   { "mcpServers": { "trusty-review": { "command": "trusty-review",
    ///                                        "args": ["mcp"] } } }
    ///
    /// Answers to `serve` as well, because that is what every `.mcp.json`
    /// written before #6290 spells. `serve` no longer starts a daemon: there is
    /// none. See `commands::mcp_stdio`.
    ///
    /// Requires the `mcp` Cargo feature (enabled by default).
    #[cfg(feature = "mcp")]
    #[command(alias = "serve")]
    Mcp(McpArgs),
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

    // #6290: no synchronous launchd pre-dispatch any more — there is no
    // `service` subcommand, because there is no unit for it to manage.
    let rt = tokio::runtime::Runtime::new().context("build tokio runtime")?;
    rt.block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> Result<()> {
    let Cli {
        config: config_path,
        command,
    } = cli;

    // #6135: `report` builds its own config, because the manifest it is given is
    // itself a config layer — it declares the provider and models of the run
    // that produced it, and those must be resolved before the rest of the chain.
    #[cfg(feature = "report")]
    let command = match command {
        Commands::Report(args) => {
            return cli_report::cmd_report(config_path.as_deref(), args).await;
        }
        other => other,
    };

    let config = ReviewConfig::from_env_and_file(config_path.as_deref(), None);

    match command {
        Commands::Run(args) => cmd_run(config, args).await,
        Commands::Compare(args) => cmd_compare(config, args).await,
        #[cfg(feature = "mcp")]
        Commands::Mcp(args) => cmd_mcp_stdio(config, args).await,
        // `report` is dispatched above, before the config is built.
        #[cfg(feature = "report")]
        Commands::Report(_) => unreachable!("report dispatched before the config is built"),
        Commands::Calibrate(args) => cmd_calibrate(config, args).await,
        Commands::WebhookListen => trusty_review::webhook_listener::run(config).await,
        Commands::Config(cmd) => cmd.run().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// REGRESSION (#6290): `serve` must keep parsing, and must land on `Mcp`.
    ///
    /// Why: every `.mcp.json` on every host that has ever installed
    /// trusty-review spells this `["serve", "--stdio"]`. clap exits 2 on an
    /// unknown subcommand, so renaming without the alias turns an upgrade into
    /// an MCP server that will not start, in a config file this repo does not
    /// own and cannot edit.
    /// What: both spellings parse to the same variant, and the daemon
    /// subcommands the alias replaced are gone.
    /// Test: this is the test.
    #[cfg(feature = "mcp")]
    #[test]
    fn serve_is_still_accepted_as_an_alias() {
        for argv in [
            vec!["trusty-review", "serve", "--stdio"],
            vec!["trusty-review", "mcp"],
        ] {
            let cli =
                Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"));
            assert!(
                matches!(cli.command, Commands::Mcp(_)),
                "{argv:?} must reach the MCP stdio handler"
            );
        }

        for retired in ["socket", "service"] {
            assert!(
                Cli::try_parse_from(["trusty-review", retired]).is_err(),
                "`{retired}` described the daemon and must be gone, not silently accepted"
            );
        }
    }
}
