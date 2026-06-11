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

use crate::mcp_handle::McpServiceHandle;

pub mod bind;
pub mod connector;
pub mod detect;
pub mod mcp_handle;
pub mod metrics_poller;
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
/// lets operators tune the background health-poll frequency; `--tailscale`
/// enables durable tailnet exposure without requiring `--http 0.0.0.0`.
/// What: Optional `--http` (default `127.0.0.1:7788`), `--open`,
/// `--poll-interval`, and `--tailscale` flags.
/// Env overrides: `TRUSTY_CONSOLE_BIND` sets the default bind mode so a
/// supervised/relaunched daemon stays tailnet-reachable without extra flags.
/// Test: Default address tested in `test_serve_args_defaults` below.
#[derive(Debug, Parser)]
pub struct ServeArgs {
    /// Address to listen on (default: 127.0.0.1:7788).
    ///
    /// Takes precedence over --tailscale and TRUSTY_CONSOLE_BIND when set to a
    /// non-default value.
    #[arg(long, default_value = "127.0.0.1:7788")]
    pub http: String,

    /// Expose the console on both 127.0.0.1 and the machine's Tailscale IPv4,
    /// enabling tailnet clients to reach the console without LAN exposure.
    ///
    /// The Tailscale IP is detected via `tailscale ip -4`. If Tailscale is not
    /// running, prints a warning and falls back to localhost-only.
    ///
    /// Can also be set persistently via the TRUSTY_CONSOLE_BIND=tailscale env
    /// var so a supervised/relaunched console stays tailnet-reachable without
    /// manually passing this flag.
    #[arg(long, default_value_t = false)]
    pub tailscale: bool,

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
/// What: Resolves bind addresses (respecting `--tailscale`, `--http`, and
/// `TRUSTY_CONSOLE_BIND`), builds the router, binds TCP listener(s), writes
/// the discovery file, starts the background health-poll task, optionally opens
/// a browser, then serves until SIGTERM/SIGINT with graceful shutdown.
/// Additional addresses beyond the primary get their own spawned `axum::serve`
/// task that runs concurrently until the shared shutdown signal fires.
/// Test: Server integration tests in `server.rs` cover the router directly
/// without exercising this function (to avoid real TCP binding in unit tests).
pub async fn run_serve(args: ServeArgs) -> Result<()> {
    const DEFAULT_HTTP: &str = "127.0.0.1:7788";

    // ── resolve bind mode ───────────────────────────────────────────────────
    let mode = bind::BindMode::from_env_and_flags(&args.http, DEFAULT_HTTP, args.tailscale);
    let port = bind::port_from_addr(&args.http, 7788);
    let addrs = bind::resolve_bind_addrs(&mode, port, bind::detect_tailscale_ipv4);

    // ── service setup ───────────────────────────────────────────────────────
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

    // ── metrics MCP poll (trusty-analyze) ───────────────────────────────────
    // Spawn a supervised stdio MCP connection to trusty-analyze and poll its
    // console_metrics tool every poll_interval seconds. The poller writes
    // into state.metrics_cache(), which the route handler reads directly.
    // On machines where trusty-analyze is absent the McpServiceHandle marks
    // it Absent immediately — every poll returns Err, the cache stays None,
    // and /api/console/metrics/analyze returns 503 (graceful degradation).
    //
    // Why "mcp" not "serve --mcp":
    // `serve --mcp` starts BOTH the HTTP daemon and an MCP stdio loop; it
    // requires trusty-search to be reachable at startup and tries to open the
    // redb facts store (which may already be locked by the running daemon).
    // `mcp` only runs a pure stdio bridge pointing at the running HTTP daemon;
    // if the HTTP daemon is not yet up, `ensure_mcp_daemon_up` in analyze's
    // `mcp` subcommand starts it automatically. This is the correct invocation
    // for a lightweight stdio-only console_metrics child.
    {
        let handle = McpServiceHandle::new("trusty-analyze", vec!["mcp".to_string()]);
        metrics_poller::start(
            handle,
            state.metrics_cache().clone(),
            Duration::from_secs(args.poll_interval),
        );
    }

    let router = server::build_router(state.clone());

    // ── bind primary listener ───────────────────────────────────────────────
    let primary_addr = *addrs.first().context("bind address list is empty")?;
    let primary_listener = bind::bind_listener(primary_addr).await?;
    let primary_local = primary_listener.local_addr().context("get local addr")?;
    let addr_string = primary_local.to_string();
    info!("trusty-console listening on http://{primary_local}");

    // ── bind additional listeners (Tailscale mode: secondary addr) ──────────
    for &extra_addr in addrs.get(1..).unwrap_or(&[]) {
        let extra_listener = bind::bind_listener(extra_addr).await?;
        let extra_local = extra_listener
            .local_addr()
            .context("get extra local addr")?;
        info!("trusty-console also listening on http://{extra_local}");
        eprintln!("trusty-console (tailnet): http://{extra_local}");
        let r = router.clone();
        tokio::spawn(async move {
            if let Err(e) = axum::serve(extra_listener, r)
                .with_graceful_shutdown(trusty_common::shutdown_signal())
                .await
            {
                tracing::warn!("extra listener {extra_local} exited: {e}");
            }
        });
    }

    // ── write discovery file (primary address) ──────────────────────────────
    // Best-effort: log a warning on failure but do not abort the serve.
    if let Err(e) = write_daemon_addr("trusty-console", &addr_string) {
        tracing::warn!("could not write trusty-console discovery file: {e}");
    }

    let console_url = format!("http://{primary_local}");
    eprintln!("trusty-console: {console_url}");

    if args.open {
        // Best-effort browser open; ignore errors.
        let _ = open::that(&console_url);
    }

    axum::serve(primary_listener, router)
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

    /// Why: default http address must be 127.0.0.1:7788 and tailscale off.
    /// What: parses `serve` with no flags and checks all defaults.
    /// Test: this test itself.
    #[test]
    fn test_serve_args_defaults() {
        let cli = Cli::parse_from(["trusty-console", "serve"]);
        match cli.command {
            Commands::Serve(args) => {
                assert_eq!(args.http, "127.0.0.1:7788");
                assert!(!args.open);
                assert!(!args.tailscale);
                assert_eq!(args.poll_interval, 15);
            }
        }
    }

    /// Why: --tailscale flag must be parsed correctly.
    /// What: parses `serve --tailscale`; asserts tailscale=true.
    /// Test: this test itself.
    #[test]
    fn test_serve_args_tailscale_flag() {
        let cli = Cli::parse_from(["trusty-console", "serve", "--tailscale"]);
        match cli.command {
            Commands::Serve(args) => {
                assert!(args.tailscale);
                assert_eq!(args.http, "127.0.0.1:7788");
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
