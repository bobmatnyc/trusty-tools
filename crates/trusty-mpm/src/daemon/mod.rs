//! trusty-mpm daemon library.
//!
//! Why: the daemon's HTTP API and shared state are useful beyond the `trusty-mpmd`
//! binary — sibling crates (e.g. the Telegram bot's test suite) reuse the real
//! `api::router` and `DaemonState` to drive in-process integration tests without
//! a live daemon. Exposing the modules as a library makes that possible.
//! What: re-exports the daemon's modules as `pub` so both `main.rs` and external
//! consumers can build against them.
//! Test: the modules carry their own `#[cfg(test)]` suites; `cargo test
//! -p trusty-mpm-daemon` exercises them.

pub mod api;
pub mod audit;
pub mod claude_config;
pub mod coordinator;
pub mod discover;
pub mod discovery;
pub mod doctor;
pub mod error;
pub mod llm_overseer;
pub mod lock;
pub mod mcp_backend;
pub mod openapi;
pub mod optimizer;
pub mod overseer_compose;
pub mod pairing_store;
pub mod services;
pub mod state;
pub mod tmux;
pub mod watcher;

use std::net::SocketAddr;
use std::sync::Arc;

use tracing::info;

pub use state::DaemonState;

/// Run the resident HTTP daemon: API, hook relay, dashboard feed.
///
/// Why: the HTTP boot sequence is shared by both the standalone `trusty-mpmd`
/// shim and the unified `trusty-mpm daemon` subcommand; living in the library
/// keeps a single source of truth.
/// What: announces tmux availability, discovers the trusty sidecars, spawns the
/// file watcher, then serves the axum router until the socket closes.
/// Test: `cargo run -p trusty-mpm-cli -- daemon` logs "trusty-mpm daemon
/// starting" and `curl localhost:7880/health` returns `ok`.
pub async fn run_http(state: Arc<DaemonState>, addr: SocketAddr) -> anyhow::Result<()> {
    info!("trusty-mpm daemon starting on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_http(state, listener).await
}

/// Run the daemon's background tasks and HTTP API on an already-bound listener.
///
/// Why: the CLI performs auto port selection (binding an ephemeral port when
/// the configured one is busy) and needs the daemon to serve on that exact
/// listener; passing a pre-bound `TcpListener` lets it own the bind decision
/// while the daemon still owns sidecar discovery, the watcher, and the reaper.
/// What: discovers the trusty sidecars, spawns the file watcher and the
/// dead-session reaper, then serves the axum router on `listener` until close.
/// Test: covered indirectly by the e2e suite, which boots the daemon on a
/// loopback port and drives it over HTTP.
pub async fn serve_http(
    state: Arc<DaemonState>,
    listener: tokio::net::TcpListener,
) -> anyhow::Result<()> {
    if tmux::TmuxDriver::is_available() {
        info!("tmux control model available");
    } else {
        info!("tmux not found — sessions will need the PTY or SDK control model");
    }

    // Discover the trusty sidecar addresses and record them in shared state.
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let addrs = discover::discover_all(&home).await;
    info!(
        "trusty-memory at {}, trusty-search at {}",
        addrs.memory, addrs.search
    );
    state.set_trusty_addrs(addrs);

    // Auto-discover existing Claude Code sessions — both tmux panes and native
    // Terminal.app processes — so they appear in the dashboard and the Telegram
    // bot without a manual `/adopt`.
    let discovered = discovery::discover_all(&state);
    if discovered.adopted > 0 {
        info!(
            "auto-discovered {} Claude Code session(s)",
            discovered.adopted
        );
    }

    // Spawn the multi-session file watcher as a background task.
    let fw = watcher::FileWatcher::new(Arc::clone(&state));
    tokio::spawn(fw.spawn());

    // Spawn the periodic dead-session reaper.
    tokio::spawn(reap_loop(Arc::clone(&state)));

    let app = api::router(state);
    info!("daemon listening; press Ctrl-C to stop");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Spawn a background axum server for a secondary listener (e.g. Tailscale).
///
/// Why: the CLI binds an extra listener for Tailscale external access but does
/// not depend on `axum`; the daemon owns the router and the `axum::serve`
/// call, so the secondary server is spawned here on shared `DaemonState`.
/// What: builds a router over `state` and spawns an `axum::serve` task on
/// `listener`, logging if the server exits with an error.
/// Test: covered indirectly — the primary listener path is exercised by the
/// e2e suite and the secondary path reuses the same `api::router`.
pub fn spawn_secondary_listener(state: Arc<DaemonState>, listener: tokio::net::TcpListener) {
    let app = api::router(state);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::warn!("secondary listener failed: {e}");
        }
    });
}

/// Interval between dead-session reap sweeps.
const REAP_INTERVAL_SECS: u64 = 60;

/// Periodically prune registry entries whose tmux session has exited.
///
/// Why: without housekeeping, dead sessions accumulate in `DaemonState`
/// forever; a slow background sweep keeps the registry honest.
/// What: every [`REAP_INTERVAL_SECS`] seconds, discovers tmux and calls
/// [`DaemonState::reap_dead_sessions`]; logs how many entries were reaped.
/// Test: the reaping rule is unit-tested via `DaemonState::reap_against`.
async fn reap_loop(state: Arc<DaemonState>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(REAP_INTERVAL_SECS));
    loop {
        tick.tick().await;
        if let Ok(driver) = tmux::TmuxDriver::discover() {
            let result = state.reap_dead_sessions(&driver);
            if result.reaped > 0 {
                info!("reaped {} dead session(s)", result.reaped);
            }
            if result.stopped > 0 {
                info!(
                    "marked {} session(s) stopped (claude process exited)",
                    result.stopped
                );
            }
        }
    }
}

/// Run the MCP server over stdio so a Claude Code session can call the
/// orchestration tools (`session_list`, `agent_delegate`, ...).
///
/// Why: shared by `trusty-mpmd mcp` and `trusty-mpm daemon --mcp`.
/// What: wraps [`DaemonState`] in a [`mcp_backend::StateBackend`] and pumps the
/// trusty-mcp-core stdio JSON-RPC loop.
/// Test: pipe a JSON-RPC `initialize` request to the process and observe a
/// well-formed response on stdout.
pub async fn run_mcp(state: Arc<DaemonState>) -> anyhow::Result<()> {
    info!("trusty-mpm MCP server starting on stdio");
    let backend = mcp_backend::StateBackend::new(state);
    trusty_common::mcp::run_stdio_loop(move |req| {
        let backend = backend.clone();
        async move { crate::mcp::dispatch(&backend, req).await }
    })
    .await
}
