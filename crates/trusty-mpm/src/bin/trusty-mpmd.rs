//! trusty-mpm daemon entry point (`trusty-mpmd`).
//!
//! Why: claude-mpm spawns a fresh Python process per hook invocation; a single
//! long-lived daemon removes that per-call cost and enables shared state. This
//! binary is kept as a backward-compatible shim — the primary entry point is
//! now `trusty-mpm daemon`, which calls the same library functions.
//! What: boots tracing, parses CLI flags, builds the shared [`DaemonState`],
//! and delegates to [`trusty_mpm::daemon::run_http`] or
//! [`trusty_mpm::daemon::run_mcp`].
//! Test: `cargo run -p trusty-mpm-daemon` logs "trusty-mpm daemon starting" and
//! `curl localhost:7880/health` returns `ok`.

use std::net::SocketAddr;

use clap::{Parser, Subcommand};
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

use trusty_mpm::daemon::{DaemonState, run_http, run_mcp};

/// trusty-mpm daemon command-line options.
#[derive(Debug, Parser)]
#[command(name = "trusty-mpmd", version, about = "trusty-mpm daemon")]
struct Args {
    /// Run mode (defaults to the resident HTTP daemon).
    #[command(subcommand)]
    mode: Option<Mode>,
}

/// Daemon run modes.
#[derive(Debug, Subcommand)]
enum Mode {
    /// Run the resident HTTP API and universal hook relay.
    Http {
        /// Address the daemon HTTP API binds to.
        #[arg(long, env = "TRUSTY_MPM_ADDR", default_value = "127.0.0.1:7880")]
        addr: SocketAddr,
    },
    /// Run as an MCP server over stdio (launched by a Claude Code session).
    Mcp,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // File logging: write daily-rotated logs to ~/.trusty-mpm/logs/ in addition
    // to the existing stderr stream. The non-blocking writer's `_guard` must
    // outlive `main` so buffered records flush before the process exits.
    let log_dir = dirs::home_dir()
        .expect("home dir")
        .join(".trusty-mpm")
        .join("logs");
    std::fs::create_dir_all(&log_dir)?;

    let file_appender = tracing_appender::rolling::daily(&log_dir, "trusty-mpm.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let env_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let file_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());

    // Bug-reporting #478 (Phase 1 wire-up): compose the bug-capture layer into
    // the same registry so ERROR events are captured to
    // <data_dir>/trusty-mpm/errors.jsonl and the in-memory ring without
    // modifying any existing call sites. The `_error_store` handle is prefixed
    // to mark it intentionally unused here; Phase 2 will stash it in
    // `DaemonState` and surface it through HTTP / MCP tools.
    //
    // Capture writes ONLY to a JSONL file + in-memory ring — never stdout —
    // so this is safe for both the HTTP daemon and the MCP stdio path. Opt-out
    // via TRUSTY_NO_BUG_CAPTURE=1 is honoured by the layer on every event.
    let (capture_layer, _error_store) = trusty_common::error_capture::bug_capture_layer(
        "trusty-mpm",
        trusty_common::error_capture::DEFAULT_CAPTURE_CAPACITY,
        env!("CARGO_PKG_VERSION"),
    );

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                // MCP mode speaks JSON-RPC on stdout — keep tracing on stderr.
                .with_writer(std::io::stderr)
                .with_filter(env_filter),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_filter(file_filter),
        )
        .with(capture_layer)
        .init();

    let args = Args::parse();
    let state = DaemonState::shared();

    match args.mode.unwrap_or(Mode::Http {
        addr: "127.0.0.1:7880".parse().expect("valid default addr"),
    }) {
        Mode::Http { addr } => run_http(state, addr).await,
        Mode::Mcp => run_mcp(state).await,
    }
}
