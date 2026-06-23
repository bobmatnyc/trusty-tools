//! Graceful-shutdown helpers for the session manager.
//!
//! Why: `manager.rs` is near the 500-SLOC production cap; placing the
//! shutdown operation in a sibling file keeps both files well under the limit
//! while keeping related lifecycle logic co-located in the `session_manager`
//! module tree.
//! What: `impl SessionManager { shutdown }` — gracefully stops every live
//! (non-terminal) managed session. For each session it sends a best-effort
//! Ctrl-C interrupt (`send_interrupt`), waits 2 s asynchronously so the claude
//! process can flush state, then kills the tmux session via `graceful_stop`.
//! SIGTERM-with-known-PID is deferred to PR B (#1595) once the PID registry
//! exists. The 2 s wait is done with `tokio::time::sleep` — never a blocking
//! `std::thread::sleep` — so it does not starve the Tokio thread pool.
//! Test: `shutdown_calls_graceful_stop_for_active_sessions` (integration test
//! in session_manager/tests.rs), `fake_driver_graceful_stop_with_pid`,
//! `fake_driver_graceful_stop_without_pid`.

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
    /// first interrupts all sessions (best-effort Ctrl-C via `send_interrupt`),
    /// then awaits a [`SIGTERM_GRACE_SECS`]-second async sleep so the claude
    /// processes can flush state, then hard-kills each session via
    /// `tmux.graceful_stop`. Using `tokio::time::sleep` (never
    /// `std::thread::sleep`) keeps the Tokio thread pool free during the wait.
    /// The one-sleep-for-all approach bounds total shutdown time at O(1).
    /// What: collects all Active/Provisioning records; sends send_interrupt to
    /// each; awaits the grace window; calls graceful_stop on each. Currently
    /// passes `None` as the PID (Ctrl-C + grace + kill path); the SIGTERM-with-
    /// known-PID branch of `graceful_stop` activates once the PID registry
    /// lands in PR B (#1595). Fails open per session. Logs a summary line.
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

        // Phase 1: send termination signal to all live sessions synchronously.
        // `graceful_stop` on the trait does signal + kill in one step; to honour
        // the grace window without blocking a thread, we split into two phases:
        // signal via send_interrupt (best-effort), sleep, then graceful_stop
        // (which re-signals and kills). For simplicity in the default-impl path,
        // we call graceful_stop directly — it does its own signal + kill without
        // sleeping. The async grace window here covers the gap between our first
        // signal and the final kill.
        for record in &live {
            // Best-effort pre-signal (Ctrl-C / SIGTERM not possible without PID
            // at this layer, so we use the driver's interrupt seam).
            let _ = self.tmux.send_interrupt(&record.tmux_name);
        }

        // Phase 2: async grace window — await without blocking a thread.
        tokio::time::sleep(Duration::from_secs(SIGTERM_GRACE_SECS)).await;

        // Phase 3: hard-kill any sessions still alive via graceful_stop
        // (which will re-signal and call kill_session; the session may already
        // be gone, in which case kill_session is a quiet no-op for the session).
        let mut stopped = 0usize;
        for record in &live {
            // TODO(#1595 PR B): thread record.claude_pid here once the PID registry exists
            match self.tmux.graceful_stop(&record.tmux_name, None) {
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
}
