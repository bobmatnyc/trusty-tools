//! trusty-console library entry point.
//!
//! Why: Expose the console daemon's startup sequence as a public `run()`
//! function so bundled shim binaries inside host crates (trusty-search,
//! trusty-memory, trusty-analyze, trusty-review, trusty-mpm) can call
//! `trusty_console::run()` without duplicating any logic. This mirrors the
//! exact pattern used by trusty-embedderd (bundled into trusty-search via
//! issue #187) and trusty-bm25-daemon (bundled into trusty-memory via PR #190).
//! What: Re-exports all public submodules and provides `run()` as the
//! canonical library entry point that parses argv and dispatches to subcommands.
//! Test: `cargo test -p trusty-console` exercises the CLI parsing tests defined
//! in the submodules.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing::info;
use trusty_common::{init_tracing, shutdown_signal, write_daemon_addr};

pub mod connector;
pub mod detect;
pub mod poller;
pub mod proxy;
pub mod server;

// ─── CLI ─────────────────────────────────────────────────────────────────────

/// trusty-console: web dashboard for trusty services.
///
/// Why: Provides a single entry point for all console subcommands so future
/// phases (status, doctor, open) can be added without breaking existing usage.
/// What: Parses top-level arguments and delegates to subcommand handlers.
/// Test: `cargo run -p trusty-console -- serve --help` must succeed.
#[derive(Debug, Parser)]
#[command(
    name = "trusty-console",
    version,
    about = "Web dashboard for trusty services"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// Available subcommands.
///
/// Why: P0/P1 only has `serve`; future phases add `status` (CLI-only) etc.
/// What: Clap enum; each variant carries its own args.
/// Test: Subcommand selection tested via `Cli::parse_from`.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Start the HTTP server and serve the console dashboard.
    Serve(ServeArgs),
}

/// Arguments for `trusty-console serve`.
///
/// Why: The bind address must be configurable so users can change the port when
/// 7788 is taken; `--open` is a convenience for developers; `--poll-interval`
/// lets operators tune the background health-poll frequency.
/// What: Optional `--http` (default `127.0.0.1:7788`), `--open`, and
/// `--poll-interval` flags.
/// Test: Default address tested in `test_serve_args_defaults` below.
#[derive(Debug, Parser)]
pub struct ServeArgs {
    /// Address to listen on (default: 127.0.0.1:7788).
    #[arg(long, default_value = "127.0.0.1:7788")]
    pub http: String,

    /// Open the console in the default browser after starting.
    #[arg(long, default_value_t = false)]
    pub open: bool,

    /// Background health-poll interval in seconds (default: 15).
    #[arg(long, default_value_t = 15u64)]
    pub poll_interval: u64,
}

// ─── public entry point ────────────────────────────────────────────────────

/// Library entry point for the trusty-console daemon.
///
/// Why: Bundled shim binaries inside host crates (trusty-search, trusty-memory,
/// trusty-analyze, trusty-review, trusty-mpm) call this function so all daemon
/// logic stays here in the library crate — no duplication. This mirrors the
/// pattern of `trusty_embedderd::run()` (issue #187) and
/// `trusty_bm25_daemon::run()` (PR #190).
/// What: Initialises tracing, parses argv via `Cli::parse()`, and dispatches
/// to the matching subcommand handler. Returns `Ok(())` after clean shutdown.
/// Test: Direct CLI-arg tests in `tests` module below; integration via
/// `cargo test -p trusty-console`.
pub async fn run() -> Result<()> {
    init_tracing(1);

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve(args) => run_serve(args).await,
    }
}

/// Run the `serve` subcommand.
///
/// Why: Separating the serve logic from `run()` keeps `run()` thin and allows
/// this function to be called from integration tests.
/// What: Builds the router, binds the TCP listener, writes the discovery file,
/// starts the background health-poll task, optionally opens a browser, then
/// serves until SIGTERM/SIGINT with graceful shutdown.
/// Test: Server integration tests in `server.rs` cover the router directly
/// without exercising this function (to avoid real TCP binding in unit tests).
pub async fn run_serve(args: ServeArgs) -> Result<()> {
    let connectors = detect::all_connectors();
    let state = server::AppState::new(connectors);

    // Kick off an eager first poll so the cache is warm before the first
    // HTTP request arrives.
    {
        let cache = state.poller_cache().clone();
        let c = state.connectors();
        cache.poll_once(c).await;
    }

    // Start the background poller that refreshes the cache on the configured
    // interval.
    poller::start(
        state.poller_cache().clone(),
        state.connectors(),
        Duration::from_secs(args.poll_interval),
    );

    let router = server::build_router(state.clone());

    let listener = tokio::net::TcpListener::bind(&args.http)
        .await
        .with_context(|| format!("failed to bind {}", args.http))?;

    let addr = listener.local_addr().context("get local addr")?;
    let addr_string = addr.to_string();
    info!("trusty-console listening on http://{addr}");

    // Write the discovery file so CLI commands and other services can find us.
    // Best-effort: log a warning on failure but do not abort the serve.
    if let Err(e) = write_daemon_addr("trusty-console", &addr_string) {
        tracing::warn!("could not write trusty-console discovery file: {e}");
    }

    let console_url = format!("http://{addr}");
    eprintln!("trusty-console: {console_url}");

    if args.open {
        // Best-effort browser open; ignore errors.
        let _ = open::that(&console_url);
    }

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    // Best-effort removal of the discovery file on clean shutdown.
    // Only remove the file if it still points to our address; another
    // instance may have already written a new one.
    //
    // RESIDUAL RACE: the read → compare → delete sequence is not atomic. A
    // second instance could write a new address between our read and our
    // remove_file, causing us to delete a file we should not. The window is
    // tiny (milliseconds) and the consequence is cosmetic (a stale `port`
    // invocation returns the default rather than the live address). No
    // behavior change is required — this comment documents the known race.
    if let Ok(Some(recorded)) = trusty_common::read_daemon_addr("trusty-console")
        && recorded == addr_string
        && let Ok(dir) = trusty_common::resolve_data_dir("trusty-console")
    {
        let _ = std::fs::remove_file(dir.join("http_addr"));
    }

    Ok(())
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: default http address must be 127.0.0.1:7788.
    /// What: parses `serve` with no flags and checks the default.
    /// Test: this test itself.
    #[test]
    fn test_serve_args_defaults() {
        let cli = Cli::parse_from(["trusty-console", "serve"]);
        match cli.command {
            Commands::Serve(args) => {
                assert_eq!(args.http, "127.0.0.1:7788");
                assert!(!args.open);
                assert_eq!(args.poll_interval, 15);
            }
        }
    }

    /// Why: custom --http flag must override the default.
    /// What: parses `serve --http 0.0.0.0:9000`.
    /// Test: this test itself.
    #[test]
    fn test_serve_args_custom_http() {
        let cli = Cli::parse_from(["trusty-console", "serve", "--http", "0.0.0.0:9000"]);
        match cli.command {
            Commands::Serve(args) => {
                assert_eq!(args.http, "0.0.0.0:9000");
            }
        }
    }

    /// Why: --poll-interval must override the default.
    /// What: parses `serve --poll-interval 30`.
    /// Test: this test itself.
    #[test]
    fn test_serve_args_custom_poll_interval() {
        let cli = Cli::parse_from(["trusty-console", "serve", "--poll-interval", "30"]);
        match cli.command {
            Commands::Serve(args) => {
                assert_eq!(args.poll_interval, 30);
            }
        }
    }
}
