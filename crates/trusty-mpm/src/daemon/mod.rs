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
pub mod idle_nudge;
pub mod idle_reaper;
pub mod llm_overseer;
pub mod lock;
pub mod managed_routes;
pub mod manager;
pub mod mcp_backend;
pub mod mcp_bugreport;
pub mod mcp_console;
pub mod mcp_project;
pub mod mcp_proxy;
pub mod mcp_session;
pub mod openapi;
pub mod optimizer;
pub mod orphan_gc;
pub mod overseer_compose;
pub mod pairing_store;
pub mod provisioning;
pub mod runtime_reap;
pub mod services;
pub(crate) mod session_record_kind;
pub mod sm_stdio;
pub(crate) mod spawn_command;
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

    // Create a cancellation token so background loops exit cleanly on shutdown.
    // Each spawned loop receives a child token; the parent is cancelled inside
    // `with_graceful_shutdown` AFTER axum drains in-flight requests.
    let cancel = tokio_util::sync::CancellationToken::new();

    // Spawn the multi-session file watcher as a background task.
    // Pass a child cancel token so the watcher exits cleanly on daemon shutdown.
    let fw = watcher::FileWatcher::new(Arc::clone(&state));
    tokio::spawn(fw.spawn(cancel.child_token()));

    // Spawn the periodic dead-session reaper with a cancel token.
    tokio::spawn(reap_loop(Arc::clone(&state), cancel.child_token()));

    // Spawn the idle auto-stop reaper if enabled (#1816).
    // Gated entirely behind `idle_auto_stop.enabled`; no-op when disabled.
    spawn_idle_reaper_if_enabled(Arc::clone(&state), cancel.child_token()).await;

    // Orphan-GC: clean accumulated leaked managed sessions. Runs ONE sweep now
    // (boot cleanup of orphans inherited across restarts) and then periodically.
    // Default ON; set TRUSTY_MPM_ORPHAN_GC=0 to disable entirely.
    if orphan_gc_enabled() {
        tokio::spawn(orphan_gc_loop(Arc::clone(&state), cancel.child_token()));
    } else {
        info!("orphan-GC disabled via TRUSTY_MPM_ORPHAN_GC");
    }

    // Run inproject-hygiene for all managed base clones (#1709):
    // fetch, safety-gated hard-reset (#2177) to default branch, prune stale
    // worktrees. This is intentionally synchronous (git subprocess calls) so
    // we offload it to a blocking thread. Failures are logged as warnings;
    // they do not block the daemon from starting.
    // Default ON (the #2177 guard makes it safe); set
    // TRUSTY_MPM_INPROJECT_HYGIENE=0 to disable the whole sweep (#2190).
    if inproject_hygiene_enabled() {
        let repos_root = managed_routes::inproject::repos_root();
        tokio::task::spawn_blocking(move || {
            managed_routes::inproject_hygiene::run_hygiene_for_all_bases(&repos_root);
        });
    } else {
        info!("inproject-hygiene disabled via TRUSTY_MPM_INPROJECT_HYGIENE");
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
    // they track leak. The shutdown future: waits for signal, cancels all
    // background actor loops (so they exit cleanly rather than being dropped
    // mid-sweep), then walks BOTH registries and kills each live session,
    // fail-open per session. Also calls `manager.shutdown()` to graceful-stop
    // all live managed sessions via SIGTERM-before-kill.
    let reaper_state = Arc::clone(&state);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        // Signal all background loops to stop before we walk the session list.
        cancel.cancel();
        // Graceful-stop live SessionManager sessions (SIGTERM → 2s wait → kill).
        // This is the single authority for SM-tracked sessions; the legacy-registry
        // reap that follows (`reap_all_live_sessions`) is intentionally scoped to
        // the DaemonState legacy registry only — it does NOT re-iterate SM sessions
        // so there is no double-kill (see `reap_all_live_sessions` doc comment).
        let mgr = reaper_state.session_manager().await;
        mgr.shutdown().await;
        // Reap remaining legacy-registry sessions not covered by mgr.shutdown().
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

/// Kill every live tmux session in the LEGACY DaemonState registry on shutdown.
///
/// Why: on graceful shutdown no session may be left fire-and-forget (#1452,
/// #1455). `SessionManager::shutdown` (called before this in the shutdown
/// sequence) is the single authority for SM-tracked sessions; this function
/// covers only the LEGACY `DaemonState` registry so there is no double-kill.
/// What: discovers tmux once (a no-op early-return if tmux is unavailable —
/// nothing to kill), then collects the `tmux_name` of every entry in the legacy
/// [`DaemonState`] registry only (NOT the `SessionManager` store — that was
/// already handled by `mgr.shutdown()`), and issues a best-effort `kill_session`
/// for each. One kill failing (the session may already be gone) never aborts the
/// rest — every name is attempted. Idempotent: re-running it after a clean sweep
/// simply finds nothing live. Logs a one-line summary at info level to stderr.
/// Test: `reap_all_live_sessions_is_safe_when_empty` (empty / tmux-absent no-op).
async fn reap_all_live_sessions(state: Arc<DaemonState>) {
    let Ok(driver) = tmux::TmuxDriver::discover() else {
        info!("graceful shutdown: tmux unavailable; no sessions to reap");
        return;
    };

    // Legacy DaemonState registry only — SM sessions were handled by mgr.shutdown().
    let names: std::collections::HashSet<String> = state
        .list_sessions()
        .into_iter()
        .map(|s| s.tmux_name)
        .collect();

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
    info!("graceful shutdown: reaped {reaped} legacy session(s)");
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

/// Spawn the idle auto-stop background loop if `idle_auto_stop.enabled = true`.
///
/// Why: the feature is opt-in and must have ZERO overhead when disabled — this
/// function is a thin gate that reads config and early-returns with a debug log
/// when the feature flag is off. When enabled it builds a verdict provider from
/// the daemon's `ActivityMonitor` and hands off to `idle_reaper_loop`.
/// What: reads `MpmConfig::load_default().idle_auto_stop`; if disabled, returns
/// immediately. If enabled, acquires the session manager and spawns
/// `idle_reaper::idle_reaper_loop` as a background task with the provided
/// `cancel` token.
/// Test: the loop itself is tested in `idle_reaper::tests`; this wrapper is
/// covered by the integration smoke-test (daemon starts without the feature).
async fn spawn_idle_reaper_if_enabled(
    state: Arc<DaemonState>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let cfg = crate::core::config::MpmConfig::load_default().idle_auto_stop;
    if !cfg.enabled {
        tracing::debug!("idle-reaper: disabled (idle_auto_stop.enabled = false); skipping");
        return;
    }
    info!(
        dry_run = cfg.dry_run,
        poll_secs = cfg.poll_interval_secs,
        idle_threshold = cfg.idle_consecutive_threshold,
        done_threshold = cfg.done_consecutive_threshold,
        "idle-reaper: enabled; spawning background loop{}",
        if cfg.dry_run {
            " (report-only — set idle_auto_stop.dry_run=false to enact teardown)"
        } else {
            ""
        }
    );
    let mgr = state.session_manager().await;
    // Cache mgr in the provider so verdict() does not re-acquire the lock on every poll.
    let provider = Arc::new(DaemonVerdictProvider {
        mgr: Arc::clone(&mgr),
        state,
    });
    tokio::spawn(idle_reaper::idle_reaper_loop(mgr, cfg, provider, cancel));
}

/// Production `IdleVerdictProvider` backed by the daemon's `ActivityMonitor`.
///
/// Why: the idle reaper trait is injectable so unit tests can avoid LLM calls;
/// the production implementation simply calls `ActivityMonitor::check` with the
/// pane content captured from `DaemonState`.
/// What: holds a cached `Arc<SessionManager>` (avoiding a lock acquisition per
/// poll) plus `Arc<DaemonState>` for the shared `ActivityMonitor`. Converts
/// `ActivityState` to a stable string via enum match — never via `{:?}` Debug
/// repr which is not a stable API contract.
/// Test: the trait is tested with `MockVerdictProvider` in `idle_reaper::tests`.
struct DaemonVerdictProvider {
    /// Cached session manager handle — acquired once at construction.
    mgr: Arc<crate::session_manager::SessionManager>,
    state: Arc<DaemonState>,
}

impl idle_reaper::IdleVerdictProvider for DaemonVerdictProvider {
    async fn verdict(&self, session_name: &str) -> Option<String> {
        use crate::activity::ActivityState;
        // Capture the most recent output of the session's RECORDED harness pane
        // (300 lines is enough for the LLM classifier). Routing through
        // `capture_pane_by_tmux_name` rather than the raw session-scoped
        // `tmux_driver().capture()` (#2515) ensures we classify the pane this
        // record actually owns, not whichever sibling window an operator left
        // active — otherwise a busy session can be auto-stopped as idle (or a
        // quiet one kept alive). `None` when no live record matches the name, the
        // recorded pane is confirmed gone, or the capture is empty: in every case
        // we skip this poll rather than act on ambiguous content.
        let pane_text = self
            .mgr
            .capture_pane_by_tmux_name(session_name, 300)
            .await?;
        // Use the shared ActivityMonitor (hash-cached — no LLM call if content unchanged).
        let monitor = self.state.activity_monitor();
        match monitor.check(session_name, &pane_text).await {
            Ok(result) => {
                // Match on the enum directly — Debug repr is not a stable contract.
                let s = match result.verdict.state {
                    ActivityState::Idle => "idle",
                    ActivityState::Done => "done",
                    ActivityState::Working => "working",
                    ActivityState::BlockedOnPermission => "blocked-on-permission",
                    ActivityState::Errored => "errored",
                    ActivityState::Unknown => "unknown",
                };
                Some(s.to_string())
            }
            Err(e) => {
                tracing::debug!(name = %session_name, "idle-reaper: activity check failed: {e}");
                None
            }
        }
    }
}

/// Interval between dead-session reap sweeps.
const REAP_INTERVAL_SECS: u64 = 60;

/// Periodically prune registry entries whose tmux session has exited.
///
/// Why: without housekeeping, dead sessions accumulate in `DaemonState`
/// forever; a slow background sweep keeps the registry honest.
/// What: every [`REAP_INTERVAL_SECS`] seconds, discovers tmux and calls
/// [`DaemonState::reap_dead_sessions`]; exits cleanly when `cancel` fires
/// rather than being dropped mid-sweep on SIGTERM.
/// Test: the reaping rule is unit-tested via `DaemonState::reap_against`;
/// `cancel_token_stops_reap_loop_cleanly` asserts the loop exits on cancel.
async fn reap_loop(state: Arc<DaemonState>, cancel: tokio_util::sync::CancellationToken) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(REAP_INTERVAL_SECS));
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("reap_loop: cancel signal received; exiting");
                break;
            }
            _ = tick.tick() => {
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
                    // #1744: also reap managed sessions whose tmux session has gone.
                    state.reap_dead_managed_sessions(&driver).await;
                    // #1814: transition managed sessions to Stopped when their tmux
                    // pane is still alive but the inner `claude` process has exited
                    // (the pane fell back to a bare shell). The #1744 reap above only
                    // covers a vanished tmux *session*; the orphan-GC deliberately
                    // KEEPS tracked sessions — so without this a runtime-exited
                    // managed session would stay `Active` forever.
                    let mgr = state.session_manager().await;
                    let stopped = runtime_reap::reap_runtime_exited_managed(
                        &mgr,
                        &driver,
                        &orphan_gc::ProcessTreeProbe,
                    )
                    .await;
                    if stopped > 0 {
                        info!(
                            "marked {stopped} managed session(s) stopped (runtime process exited, #1814)"
                        );
                    }
                }
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

/// Whether the startup inproject-hygiene sweep is enabled (default ON).
///
/// Why: operators need an escape hatch to disable the hygiene sweep entirely
/// (e.g. while investigating an unexpected git state in a base clone).
/// Defaulting ON is safe now that the reset step is gated by the #2177
/// dirty/ahead-of-origin guard, so the fetch + prune + guarded-reset behaviour
/// still applies out of the box.
/// What: reads `TRUSTY_MPM_INPROJECT_HYGIENE` and delegates to the pure
/// [`parse_inproject_hygiene_enabled`]; a thin wrapper so the parsing is
/// testable without mutating process-global env.
/// Test: parsing covered by `parse_inproject_hygiene_enabled_*` below.
fn inproject_hygiene_enabled() -> bool {
    parse_inproject_hygiene_enabled(
        std::env::var("TRUSTY_MPM_INPROJECT_HYGIENE")
            .ok()
            .as_deref(),
    )
}

/// Pure parse of the `TRUSTY_MPM_INPROJECT_HYGIENE` raw value into an
/// on/off decision.
///
/// Why: mirrors [`parse_orphan_gc_enabled`] — env vars are process-global, so
/// testing the policy by mutating them is racy under the multi-threaded test
/// harness. Taking the raw value as a parameter makes the decision a pure
/// function that needs no env mutation.
/// What: returns `false` only for an explicit `0`/`false`/`off`/`no`
/// (case-insensitive, trimmed); `true` for any other value, including `None`
/// (unset) — i.e. default ON.
/// Test: `parse_inproject_hygiene_enabled_default_and_overrides`.
fn parse_inproject_hygiene_enabled(raw: Option<&str>) -> bool {
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
/// Why: leaked managed (`tm-*`/`tmpm-*`) tmux sessions (from crashes or pre-RAII builds) must be
/// cleaned up self-healingly. One sweep runs immediately at boot to clear
/// accumulated orphans, then the loop runs on the configured interval.
/// What: owns a single [`orphan_gc::OrphanGc`] so the two-pass debounce persists
/// across sweeps; exits cleanly when `cancel` fires rather than being dropped
/// mid-sweep on SIGTERM. Each tick gathers the live managed panes (via
/// [`tmux::TmuxDriver::list_managed_panes`]) and both registries' tracked names
/// (via [`DaemonState::gather_tracked_names`]), then calls
/// [`orphan_gc::run_sweep`]. A tmux-listing failure skips the pass (reaps
/// nothing) rather than risking a wrong kill; likewise, if
/// [`DaemonState::gather_tracked_names`] returns a *degraded* snapshot (a
/// registry read failed, so the protected set is incomplete), `run_sweep`
/// skips its reap phase for that tick — both paths fail CLOSED.
/// Test: the reconciliation rule and debounce are unit-tested in
/// [`orphan_gc`]; the end-to-end sweep is covered by `tests/orphan_gc_sweep.rs`.
async fn orphan_gc_loop(state: Arc<DaemonState>, cancel: tokio_util::sync::CancellationToken) {
    let interval_secs = orphan_gc_interval_secs();
    info!(
        interval_secs,
        "orphan-GC enabled; running startup sweep then periodic reconciliation"
    );
    let mut gc = orphan_gc::OrphanGc::new();
    // Cross-restart debounce for the PID-file sweep (#1918): owned here (not
    // reconstructed per-tick) so its two-pass candidate history persists across
    // sweeps, exactly like `gc` above.
    let mut pid_gc = crate::core::pid_registry::PidOrphanGc::new();
    let probe = orphan_gc::ProcessTreeProbe;
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("orphan_gc_loop: cancel signal received; exiting");
                break;
            }
            _ = tick.tick() => {
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

                // PID-file orphan-GC (§10.3): reap daemon-owned `claude` child
                // processes whose tmux pane is already gone — invisible to the
                // tmux-name sweep above. Live session-ids come from BOTH registries;
                // any recorded PID whose session-id is absent is a candidate, and
                // the registry SIGTERMs it only after verifying it is still a live
                // `claude` process (the §13.5 PID-reuse mitigation) AND the
                // candidate has survived the #1918 restart-safety debounce (`pid_gc`)
                // — a session-id must be an untracked-alive-claude candidate on TWO
                // consecutive sweeps before it is actually signalled, so a legacy
                // session that is alive across a daemon restart (when `self.sessions`
                // is always empty) gets one full GC interval to be re-learned before
                // any SIGTERM is sent.
                let live_ids = state.gather_live_session_ids().await;
                let pid_outcome = state.pid_registry().sweep_orphans(
                    &live_ids,
                    &crate::core::pid_registry::OsProcessProbe,
                    &mut pid_gc,
                );
                if pid_outcome.terminated > 0
                    || pid_outcome.removed_stale > 0
                    || pid_outcome.deferred > 0
                {
                    info!(
                        terminated = pid_outcome.terminated,
                        removed_stale = pid_outcome.removed_stale,
                        deferred = pid_outcome.deferred,
                        "orphan-GC PID-file sweep cleaned up orphaned claude process(es)"
                    );
                }
                // #1508: auto-reap STALE EPHEMERAL sessions as a backstop.
                let max_age = chrono::Duration::hours(crate::session_manager::MAX_EPHEMERAL_AGE_HOURS);
                match mgr.reap_aged_ephemeral(max_age).await {
                    Ok(n) if n > 0 => info!("ephemeral auto-reap decommissioned {n} stale session(s)"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("ephemeral auto-reap failed: {e}"),
                }

                // #1838: reap orphaned managed-clone worktree dirs whose session
                // records are gone. Same self-healing rationale as the tmux orphan
                // sweep above: the `.worktrees/<id>` tree under each managed base
                // clone otherwise grows without bound (94 dirs observed for one
                // project) because decommission-time cleanup (#1806/#1840) only
                // covers sessions torn down AFTER that fix landed and the manual
                // `tm session prune-worktrees` sweep is rarely run. Only leaf
                // worktree dirs with NO live record are removed — active worktrees
                // and the base clone are never touched. Inherits the orphan-GC env
                // gate (`TRUSTY_MPM_ORPHAN_GC`) since it runs inside this loop.
                let repos_root = managed_routes::inproject::repos_root();
                match mgr.reap_orphaned_worktrees(&repos_root).await {
                    Ok(removed) if !removed.is_empty() => {
                        info!("orphan-GC reaped {} orphaned worktree dir(s)", removed.len());
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("orphan-GC worktree sweep failed: {e}"),
                }

                // #2033: sweep orphaned / 0-chunk trusty-search indexes left
                // behind by managed worktrees — sessions decommissioned before
                // the decommission-time index-removal fix, or whose worktree
                // was never populated before teardown. Best-effort: a
                // discoverable-but-erroring daemon logs and is retried next tick.
                match mgr.reap_orphaned_search_indexes().await {
                    Ok(removed) if !removed.is_empty() => {
                        info!(
                            "orphan-GC removed {} orphaned trusty-search index(es)",
                            removed.len()
                        );
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("orphan-GC search-index sweep failed: {e}"),
                }
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

#[cfg(test)]
mod shutdown_reaper_tests {
    use super::{DaemonState, reap_all_live_sessions, reap_loop};
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

    /// Cancelling the token causes `reap_loop` to exit without panic.
    ///
    /// Why: the WI-4 PR A requirement is that background loops respond to the
    /// cancellation token rather than being dropped mid-sweep on SIGTERM. This
    /// test asserts that the loop task completes cleanly (join returns `Ok`)
    /// after the token is cancelled, and never panics.
    /// What: spawns `reap_loop` with a fresh token, immediately cancels it,
    /// then awaits the task handle and asserts the join succeeded.
    /// Test: this is the test.
    #[tokio::test]
    async fn cancel_token_stops_reap_loop_cleanly() {
        let state: Arc<DaemonState> = DaemonState::shared();
        let cancel = tokio_util::sync::CancellationToken::new();
        let token = cancel.clone();
        let handle = tokio::spawn(reap_loop(state, token));
        // Cancel immediately — the loop should exit on the next select! poll.
        cancel.cancel();
        // The task must complete without panicking (join returns Ok).
        handle
            .await
            .expect("reap_loop task must not panic after cancellation");
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

#[cfg(test)]
mod inproject_hygiene_config_tests {
    use super::parse_inproject_hygiene_enabled;

    /// `TRUSTY_MPM_INPROJECT_HYGIENE` defaults ON and only explicit falsey
    /// values disable (#2190).
    ///
    /// Tests the PURE parser with raw values — no process-global env mutation,
    /// so it is safe to run in parallel with any sibling test (the #1458 review
    /// fix for env-var test flakiness).
    #[test]
    fn parse_inproject_hygiene_enabled_default_and_overrides() {
        assert!(
            parse_inproject_hygiene_enabled(None),
            "unset must default ON"
        );

        for off in ["0", "false", "off", "no", "OFF", " false "] {
            assert!(
                !parse_inproject_hygiene_enabled(Some(off)),
                "{off:?} must disable"
            );
        }
        for on in ["1", "true", "yes", "anything", "  "] {
            assert!(
                parse_inproject_hygiene_enabled(Some(on)),
                "{on:?} must keep enabled"
            );
        }
    }
}
