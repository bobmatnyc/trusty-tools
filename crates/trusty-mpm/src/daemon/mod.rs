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
pub mod bug_report;
pub mod claude_config;
pub mod coordinator;
pub mod discover;
pub mod discovery;
pub mod doctor;
pub mod error;
pub mod llm_overseer;
pub mod lock;
pub mod managed_routes;
pub mod mcp_backend;
pub mod mcp_console;
pub mod mcp_session;
pub mod openapi;
pub mod optimizer;
pub mod orphan_gc;
pub mod overseer_compose;
pub mod pairing_store;
pub mod services;
pub mod sm_stdio;
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

    // Orphan-GC: clean accumulated leaked managed sessions. Runs ONE sweep now
    // (boot cleanup of orphans inherited across restarts) and then periodically.
    // Default ON; set TRUSTY_MPM_ORPHAN_GC=0 to disable entirely.
    if orphan_gc_enabled() {
        tokio::spawn(orphan_gc_loop(Arc::clone(&state)));
    } else {
        info!("orphan-GC disabled via TRUSTY_MPM_ORPHAN_GC");
    }

    let app = api::router(Arc::clone(&state));
    info!("daemon listening; press Ctrl-C to stop");
    // `into_make_service_with_connect_info` makes the peer `SocketAddr` available
    // to handlers via `ConnectInfo` — the loopback-only `POST /rpc` gate (#1221)
    // depends on it. Without connect-info the `ConnectInfo` extractor would 500.
    //
    // `with_graceful_shutdown` lets the daemon reap every live tmux session it
    // owns before the process exits (#1455). Without it the `reap_loop` /
    // `orphan_gc_loop` tasks are simply abandoned on SIGTERM and the sessions
    // they track leak. The shutdown future first drains in-flight requests, then
    // walks BOTH registries and kills each live session, fail-open per session.
    let reaper_state = Arc::clone(&state);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        reap_all_live_sessions(reaper_state).await;
    })
    .await?;
    Ok(())
}

/// Block until the process receives a shutdown signal (SIGINT or SIGTERM).
///
/// Why: the graceful-shutdown reaper (#1455) must fire on the SAME signals the
/// daemon is stopped with. `tm restart` / `launchctl bootout` deliver SIGTERM;
/// trapping only Ctrl-C (SIGINT) would skip the reap on the common stop path and
/// leak every live session. This mirrors the binary's `wait_for_shutdown_signal`
/// so the library boot path (used by external in-process consumers) reaps too.
/// What: on Unix, races `ctrl_c()` against a SIGTERM stream; if registering the
/// SIGTERM handler fails it falls back to awaiting Ctrl-C alone. On non-Unix
/// (no SIGTERM) it simply awaits Ctrl-C.
/// Test: signal delivery is environment-bound and not unit-tested; the reap it
/// triggers is covered by `reap_all_live_sessions`'s own behaviour.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to register SIGTERM handler: {e}");
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Kill every live tmux session owned by the daemon across BOTH registries.
///
/// Why: on graceful shutdown no session may be left fire-and-forget (#1452,
/// #1455). The periodic `reap_loop` / `orphan_gc_loop` tasks are abandoned when
/// the process exits, so a dedicated shutdown sweep is the last line that keeps
/// the host clean of leaked `tmpm-*` sessions.
/// What: discovers tmux once (a no-op early-return if tmux is unavailable —
/// nothing to kill), then collects the `tmux_name` of every entry in the legacy
/// [`DaemonState`] registry and every non-terminal record in the
/// [`SessionManager`](crate::session_manager::SessionManager) store, de-dupes,
/// and issues a best-effort `kill_session` for each. One kill failing (the
/// session may already be gone) never aborts the rest — every name is attempted.
/// Idempotent: re-running it after a clean sweep simply finds nothing live.
/// Logs a one-line summary (count reaped) at info level to stderr.
/// Test: `reap_all_live_sessions_is_safe_when_empty` (empty/ tmux-absent no-op);
/// the per-session kill loop reuses the unit-tested `kill_session` seam.
async fn reap_all_live_sessions(state: Arc<DaemonState>) {
    let Ok(driver) = tmux::TmuxDriver::discover() else {
        info!("graceful shutdown: tmux unavailable; no sessions to reap");
        return;
    };

    // Union the tmux names from both registries so a session tracked by EITHER
    // is reaped. De-dupe via a set: the same name can appear in both.
    let mut names: std::collections::HashSet<String> = state
        .list_sessions()
        .into_iter()
        .map(|s| s.tmux_name)
        .collect();

    let mgr = state.session_manager().await;
    for record in mgr.list().await {
        if matches!(
            record.state,
            crate::session_manager::ManagedSessionState::Decommissioned
        ) {
            continue;
        }
        names.insert(record.tmux_name);
    }

    let mut reaped = 0usize;
    for name in names {
        match driver.kill_session(&name) {
            Ok(()) => reaped += 1,
            Err(e) => {
                // Fail-open: a kill failing (already gone, or never had a host)
                // must not stop us reaping the rest. stderr only via tracing.
                tracing::warn!(
                    "graceful shutdown: kill_session({name}) failed (may already be gone): {e}"
                );
            }
        }
    }
    info!("graceful shutdown: reaped {reaped} live session(s)");
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
        // Match the primary listener: expose ConnectInfo so the loopback-only
        // `POST /rpc` gate (#1221) works on this listener too. The Tailscale
        // listener is non-loopback, so `/rpc` here will correctly 403 — the
        // bridge only ever talks to the loopback listener.
        if let Err(e) = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        {
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

/// Default interval between orphan-GC sweeps, in seconds.
const ORPHAN_GC_INTERVAL_SECS: u64 = 60;

/// Whether the orphan-GC is enabled (default ON).
///
/// Why: operators need an escape hatch to disable the reaper entirely (e.g. on a
/// host where session tracking is known-incomplete and false positives would be
/// catastrophic). Defaulting ON keeps the self-healing behaviour without opt-in.
/// What: reads `TRUSTY_MPM_ORPHAN_GC` and delegates to the pure
/// [`parse_orphan_gc_enabled`]; a thin wrapper so the parsing is testable
/// without mutating process-global env.
/// Test: parsing covered by `parse_orphan_gc_enabled_*` below.
fn orphan_gc_enabled() -> bool {
    parse_orphan_gc_enabled(std::env::var("TRUSTY_MPM_ORPHAN_GC").ok().as_deref())
}

/// Pure parse of the `TRUSTY_MPM_ORPHAN_GC` raw value into an on/off decision.
///
/// Why: env vars are process-global, so testing the policy by mutating them is
/// racy under the multi-threaded test harness. Taking the raw value as a
/// parameter makes the decision a pure function that needs no env mutation.
/// What: returns `false` only for an explicit `0`/`false`/`off`/`no`
/// (case-insensitive, trimmed); `true` for any other value, including `None`
/// (unset) — i.e. default ON.
/// Test: `parse_orphan_gc_enabled_default_and_overrides`.
fn parse_orphan_gc_enabled(raw: Option<&str>) -> bool {
    match raw {
        Some(v) => {
            let v = v.trim().to_ascii_lowercase();
            !matches!(v.as_str(), "0" | "false" | "off" | "no")
        }
        None => true,
    }
}

/// Resolve the orphan-GC sweep interval, honouring an env override.
///
/// Why: long-running hosts may want a slower (or faster) cadence than the
/// 60-second default; the debounce is expressed in *passes*, so the interval
/// also sets the effective grace window (a fresh orphan survives ≥ one interval).
/// What: reads `TRUSTY_MPM_ORPHAN_GC_INTERVAL_SECS` and delegates to the pure
/// [`parse_orphan_gc_interval`]; a thin wrapper so the parsing is testable
/// without mutating process-global env.
/// Test: parsing covered by `parse_orphan_gc_interval_*` below.
fn orphan_gc_interval_secs() -> u64 {
    parse_orphan_gc_interval(
        std::env::var("TRUSTY_MPM_ORPHAN_GC_INTERVAL_SECS")
            .ok()
            .as_deref(),
    )
}

/// Pure parse of the `TRUSTY_MPM_ORPHAN_GC_INTERVAL_SECS` raw value to seconds.
///
/// Why: as with [`parse_orphan_gc_enabled`], keeping the parsing pure avoids
/// racy process-global env mutation in tests.
/// What: parses `raw` as a positive `u64`, falling back to
/// [`ORPHAN_GC_INTERVAL_SECS`] when `None`, unparsable, or non-positive (a zero
/// interval would busy-loop).
/// Test: `parse_orphan_gc_interval_override`.
fn parse_orphan_gc_interval(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(ORPHAN_GC_INTERVAL_SECS)
}

/// Periodically reconcile live tmux against the registries and reap orphans.
///
/// Why: leaked `tmpm-*` tmux sessions (from crashes or pre-RAII builds) must be
/// cleaned up self-healingly. One sweep runs immediately at boot to clear
/// accumulated orphans, then the loop runs on the configured interval.
/// What: owns a single [`orphan_gc::OrphanGc`] so the two-pass debounce persists
/// across sweeps; each tick gathers the live managed panes (via
/// [`tmux::TmuxDriver::list_managed_panes`]) and both registries' tracked names
/// (via [`DaemonState::gather_tracked_names`]), then calls
/// [`orphan_gc::run_sweep`]. A tmux-listing failure skips the pass (reaps
/// nothing) rather than risking a wrong kill; likewise, if
/// [`DaemonState::gather_tracked_names`] returns a *degraded* snapshot (a
/// registry read failed, so the protected set is incomplete), `run_sweep`
/// skips its reap phase for that tick — both paths fail CLOSED.
/// Test: the reconciliation rule and debounce are unit-tested in
/// [`orphan_gc`]; the end-to-end sweep is covered by `tests/orphan_gc_sweep.rs`.
async fn orphan_gc_loop(state: Arc<DaemonState>) {
    let interval_secs = orphan_gc_interval_secs();
    info!(
        interval_secs,
        "orphan-GC enabled; running startup sweep then periodic reconciliation"
    );
    let mut gc = orphan_gc::OrphanGc::new();
    let probe = orphan_gc::ProcessTreeProbe;
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    loop {
        tick.tick().await;
        let Ok(driver) = tmux::TmuxDriver::discover() else {
            continue;
        };
        let panes = match driver.list_managed_panes() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("orphan-GC: list_managed_panes failed: {e}");
                continue;
            }
        };
        // NOTE: `gather_tracked_names()` and `session_manager()` are acquired in
        // separate awaits, so this snapshot is intentionally NON-atomic — a
        // session could register in the gap between the two. That race is safe:
        // the two-pass debounce in `OrphanGc` means a just-registered session
        // missed by this sweep is only ever a reap *candidate* this pass and
        // must be seen orphaned again next sweep before it can be reaped, by
        // which point `gather_tracked_names()` will include it. No lock spanning
        // both calls is needed.
        let tracked = state.gather_tracked_names().await;
        let mgr = state.session_manager().await;
        let mtmux = mgr.tmux_driver();
        let reaped = orphan_gc::run_sweep(&mut gc, &panes, &tracked, &probe, mtmux.as_ref());
        if reaped > 0 {
            info!("orphan-GC reaped {reaped} orphaned managed session(s)");
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

#[cfg(test)]
mod shutdown_reaper_tests {
    use super::{DaemonState, reap_all_live_sessions};
    use std::sync::Arc;

    /// The shutdown reaper is a safe no-op on an empty daemon.
    ///
    /// Why: graceful shutdown must never panic, whether or not tmux is present
    /// and whether or not any session is tracked (#1455). With no registered
    /// sessions there is nothing to kill, so the sweep must complete cleanly —
    /// returning early when tmux is unavailable, or reaping zero when it is.
    /// What: builds a fresh in-memory `DaemonState` (no sessions) and runs the
    /// reaper, asserting only that it returns without panicking.
    /// Test: this is the test.
    #[tokio::test]
    async fn reap_all_live_sessions_is_safe_when_empty() {
        let state: Arc<DaemonState> = DaemonState::shared();
        // Must not panic regardless of tmux availability on the host.
        reap_all_live_sessions(state).await;
    }
}

#[cfg(test)]
mod orphan_gc_config_tests {
    use super::{ORPHAN_GC_INTERVAL_SECS, parse_orphan_gc_enabled, parse_orphan_gc_interval};

    /// `TRUSTY_MPM_ORPHAN_GC` defaults ON and only explicit falsey values disable.
    ///
    /// Tests the PURE parser with raw values — no process-global env mutation,
    /// so it is safe to run in parallel with any sibling test (the #1458 review
    /// fix for env-var test flakiness).
    #[test]
    fn parse_orphan_gc_enabled_default_and_overrides() {
        assert!(parse_orphan_gc_enabled(None), "unset must default ON");

        for off in ["0", "false", "off", "no", "OFF", " false "] {
            assert!(!parse_orphan_gc_enabled(Some(off)), "{off:?} must disable");
        }
        for on in ["1", "true", "yes", "anything", "  "] {
            assert!(
                parse_orphan_gc_enabled(Some(on)),
                "{on:?} must keep enabled"
            );
        }
    }

    /// The interval override parses positive seconds and falls back otherwise.
    ///
    /// Tests the PURE parser with raw values — no env mutation (see #1458).
    #[test]
    fn parse_orphan_gc_interval_override() {
        assert_eq!(parse_orphan_gc_interval(None), ORPHAN_GC_INTERVAL_SECS);
        assert_eq!(parse_orphan_gc_interval(Some("30")), 30);
        assert_eq!(parse_orphan_gc_interval(Some("  45 ")), 45);

        // Zero and garbage fall back to the default (a zero interval would busy-loop).
        for bad in ["0", "nonsense", "-5", ""] {
            assert_eq!(
                parse_orphan_gc_interval(Some(bad)),
                ORPHAN_GC_INTERVAL_SECS,
                "{bad:?}"
            );
        }
    }
}
