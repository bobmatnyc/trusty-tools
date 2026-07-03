//! Graceful-shutdown helpers for the session manager.
//!
//! Why: `manager.rs` is near the 500-SLOC production cap; placing the
//! shutdown operation in a sibling file keeps both files well under the limit
//! while keeping related lifecycle logic co-located in the `session_manager`
//! module tree.
//! What: `impl SessionManager { shutdown }` — gracefully stops every live
//! (non-terminal) managed session. For each session it resolves the live
//! `claude` PID (a single quick `find_claude_pid_in_tmux` probe), waits 2 s
//! asynchronously so the claude process can flush state, then calls
//! `graceful_stop(name, pid)` ONCE — SIGTERM when the PID is known (#1595 PR B),
//! else a single Ctrl-C fallback. There is exactly one signal per session: the
//! earlier double-Ctrl-C (a pre-signal loop *plus* `graceful_stop`'s own
//! fallback interrupt) is gone. The 2 s wait is done with `tokio::time::sleep` —
//! never a blocking `std::thread::sleep` — so it does not starve the Tokio
//! thread pool.
//! Test: `shutdown_calls_graceful_stop_for_active_sessions` (integration test
//! in session_manager/tests.rs), `fake_driver_graceful_stop_with_pid`,
//! `fake_driver_graceful_stop_without_pid`, `default_graceful_stop_sends_sigterm`.

use std::time::Duration;

use tracing::{info, warn};

use super::manager::SessionManager;
use super::record::ManagedSessionState;

/// Grace window given to each session between signal and tmux kill.
///
/// Why: 2 s is the industry convention (matches systemd's default
/// `TimeoutStopSec` and launchd's `ExitTimeOut`) and is enough for claude to
/// flush its in-memory state to disk without stalling the overall shutdown.
/// In test builds, the grace window is 0 s so unit tests are not blocked by
/// the sleep (eliminating the need for tokio's `test-util` feature).
#[cfg(not(test))]
const SIGTERM_GRACE_SECS: u64 = 2;
#[cfg(test)]
const SIGTERM_GRACE_SECS: u64 = 0;

impl SessionManager {
    /// Gracefully stop all live managed sessions on daemon shutdown.
    ///
    /// Why: the graceful-shutdown path in `daemon/mod.rs` needs to give every
    /// running session a chance to persist its state before the process exits.
    /// `kill_session` is abrupt; `shutdown` separates signal from kill: it
    /// resolves each session's live `claude` PID, awaits a [`SIGTERM_GRACE_SECS`]
    /// async grace window so the processes can flush state, then calls
    /// `graceful_stop(name, pid)` ONCE per session. `graceful_stop` sends SIGTERM
    /// when the PID is known (#1595 PR B) and falls back to a single Ctrl-C only
    /// when it is not — so each session receives exactly ONE termination signal.
    /// The previous implementation pre-signalled every session with Ctrl-C *and*
    /// then let `graceful_stop(None)` send another, double-interrupting the pane;
    /// that pre-signal loop is removed. Using `tokio::time::sleep` (never
    /// `std::thread::sleep`) keeps the Tokio thread pool free; the one-sleep-for-
    /// all approach bounds total shutdown time at O(1).
    /// What: collects all Active/Provisioning records; resolves each session's
    /// PID via a single [`crate::core::process::find_claude_pid_in_tmux`] probe
    /// (one attempt — at shutdown we do not retry); awaits the grace window once;
    /// calls `graceful_stop(name, pid)` per session, failing open. Logs a summary.
    /// Test: `shutdown_calls_graceful_stop_for_active_sessions` in tests.rs.
    pub async fn shutdown(&self) {
        let records = self.list().await;
        // Only stop sessions that have a live runtime: Active and Provisioning.
        // Stopped, Errored, and Decommissioned sessions have no running process.
        let live: Vec<_> = records
            .into_iter()
            .filter(|r| {
                matches!(
                    r.state,
                    ManagedSessionState::Active | ManagedSessionState::Provisioning
                )
            })
            .collect();

        if live.is_empty() {
            info!("shutdown: no live managed sessions to stop");
            return;
        }

        // Resolve each session's live `claude` PID up front (a single quick probe
        // per session — no retry budget at shutdown). A known PID lets
        // `graceful_stop` send SIGTERM directly instead of a tmux Ctrl-C.
        let pids: Vec<Option<u32>> = live
            .iter()
            .map(|r| {
                crate::core::process::find_claude_pid_in_tmux(
                    &r.tmux_name,
                    1,
                    Duration::from_millis(0),
                )
            })
            .collect();

        // Single async grace window for ALL sessions — bounds shutdown at O(1)
        // and lets the claude processes flush state before the kill.
        tokio::time::sleep(Duration::from_secs(SIGTERM_GRACE_SECS)).await;

        // Exactly one signal + kill per session via `graceful_stop`: SIGTERM when
        // the PID is known, else a single Ctrl-C fallback (no double interrupt).
        let mut stopped = 0usize;
        for (record, pid) in live.iter().zip(pids) {
            match self.tmux.graceful_stop(&record.tmux_name, pid) {
                Ok(()) => {
                    stopped += 1;
                }
                Err(e) => {
                    warn!(
                        name = %record.tmux_name,
                        "shutdown: graceful_stop failed (may already be gone): {e}"
                    );
                }
            }
        }
        info!("shutdown: gracefully stopped {stopped} live managed session(s)");
    }

    /// Gracefully terminate ONE session's runtime: signal → grace → kill.
    ///
    /// Why: CLI-initiated `stop`/`decommission` tear down a single session at a
    /// time (unlike [`Self::shutdown`], which batches one grace window across the
    /// whole fleet) and were previously abrupt — a bare `kill_session` gave the
    /// inner `claude` process no chance to flush state (memory snapshot, git
    /// index, session record) before its pane was reclaimed. This drains one
    /// session the same way `shutdown` drains the fleet, honouring the repo's
    /// graceful-shutdown convention (SIGTERM drain, not an abrupt kill).
    /// What: returns early (no signal, no grace sleep, no kill) when the tmux
    /// session no longer exists; otherwise resolves the live `claude` PID via a
    /// single [`crate::core::process::find_claude_pid_in_tmux`] probe (no retry),
    /// calls `signal_terminate` (SIGTERM when the PID is known, else one Ctrl-C), awaits
    /// a [`SIGTERM_GRACE_SECS`] async grace window (0 s in tests) so the process
    /// can checkpoint, then `kill_session` to reclaim the pane. Every step is
    /// best-effort: a `kill_session` failure is logged, not returned, because the
    /// caller still needs to mark the record `Stopped` / decommissioned even when
    /// the runtime was already gone. Uses `tokio::time::sleep` — never a blocking
    /// `std::thread::sleep` — so it does not starve the Tokio thread pool.
    /// Test: `graceful_terminate_runtime_signals_then_kills` in tests.rs (fake
    /// driver records a Ctrl-C then a kill); exercised end-to-end by
    /// `manager_stop_keeps_workspace` and `manager_decommission_removes_workspace`.
    pub(crate) async fn graceful_terminate_runtime(&self, tmux_name: &str) {
        // Fast-path: nothing to drain if the pane is already gone. Skips the grace
        // sleep entirely so callers looping over dead sessions (e.g. prune-idle) do
        // not pay SIGTERM_GRACE_SECS per already-terminated session.
        if !self.tmux.session_exists(tmux_name) {
            return;
        }
        let pid =
            crate::core::process::find_claude_pid_in_tmux(tmux_name, 1, Duration::from_millis(0));
        self.tmux.signal_terminate(tmux_name, pid);
        tokio::time::sleep(Duration::from_secs(SIGTERM_GRACE_SECS)).await;
        if let Err(e) = self.tmux.kill_session(tmux_name) {
            warn!(
                name = %tmux_name,
                "graceful_terminate_runtime: kill_session failed (may already be gone): {e}"
            );
        }
    }
}
