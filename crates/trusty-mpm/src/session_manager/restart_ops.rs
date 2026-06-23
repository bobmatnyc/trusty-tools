//! Graceful-shutdown helpers for the session manager.
//!
//! Why: `manager.rs` is near the 500-SLOC production cap; placing the
//! shutdown operation in a sibling file keeps both files well under the limit
//! while keeping related lifecycle logic co-located in the `session_manager`
//! module tree.
//! What: `impl SessionManager { shutdown }` — gracefully stops every live
//! (non-terminal) managed session by calling the driver's `graceful_stop`
//! (SIGTERM → 2 s wait → kill) before returning.
//! Test: `cancel_token_stops_reap_loop_cleanly` (in daemon/mod.rs tests),
//! `graceful_stop_sends_sigterm_then_kill`, `graceful_stop_skips_sigterm_when_no_pid`
//! (in session_manager/tests.rs).

use tracing::{info, warn};

use super::manager::SessionManager;
use super::record::ManagedSessionState;

impl SessionManager {
    /// Gracefully stop all live managed sessions on daemon shutdown.
    ///
    /// Why: the graceful-shutdown path in `daemon/mod.rs` needs to give every
    /// running session a chance to persist its state before the process exits.
    /// `kill_session` is abrupt; `graceful_stop` sends SIGTERM first, waits 2 s,
    /// then hard-kills — the same pattern launchd/systemd use for service cleanup.
    /// What: collects all non-Decommissioned, non-Failed session records, then
    /// calls `tmux.graceful_stop(tmux_name, None)` for each (PID is not yet
    /// tracked at this layer; the default-impl will fall back to `C-c`). Fails
    /// open per session: a single stop failure never aborts the rest. Logs a
    /// summary line (count stopped) at info level.
    /// Test: wired into `daemon/mod.rs` graceful-shutdown path; daemon-level
    /// coverage via `reap_all_live_sessions_is_safe_when_empty`.
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

        let mut stopped = 0usize;
        for record in &live {
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
