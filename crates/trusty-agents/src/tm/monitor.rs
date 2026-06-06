//! Background idle monitor for TM-managed sessions.
//!
//! Why: Issue #318 — without periodic observation the registry's session
//! statuses go stale (a Running session that becomes idle never transitions),
//! and orphaned tmux sessions linger as "Running" until the next manual
//! `/tm reconcile`. A small ticker that polls each Running session keeps
//! the dashboard honest.
//! What: `TmMonitor::start` spawns a tokio task that calls
//! `TmManager::poll_sessions` on a fixed interval; `stop` (and `Drop`)
//! aborts the task. The monitor is opt-in: callers construct it explicitly.
//! Test: `tests::monitor_drop_stops_task` exercises lifecycle on a no-op
//! manager handle.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::interval;
use tracing::{info, warn};

use crate::tm::manager::TmManager;

/// Owns the background polling task.
///
/// Why: Wrapping the JoinHandle in a struct lets us implement `Drop` so the
/// task is canceled automatically when the REPL tears down — no leaked
/// tickers between sessions.
/// What: Holds `Option<JoinHandle<()>>`; `stop()` aborts and clears it.
/// Test: covered by `tests::monitor_drop_stops_task`.
pub struct TmMonitor {
    handle: Option<JoinHandle<()>>,
}

impl TmMonitor {
    /// Spawn the monitor task.
    ///
    /// Why: Hands the manager to a long-lived task so callers don't have to
    /// own the polling loop themselves.
    /// What: Ticks at `poll_interval`; on every tick, locks the manager and
    /// calls `poll_sessions`, logging any state transitions. Errors are
    /// logged but never abort the loop.
    /// Test: `tests::monitor_drop_stops_task`.
    pub fn start(manager: Arc<Mutex<TmManager>>, poll_interval: Duration) -> Self {
        // #319: TM is always-on infrastructure, so `start` is called from
        // `TrustyAgentsRepl::new`. That constructor runs from sync (`#[test]`)
        // contexts as well as the async REPL boot. When no tokio runtime is
        // active, return an idle monitor instead of panicking — the REPL's
        // async `run()` path is gated by `tokio::main`, so production never
        // hits this branch.
        let handle = match tokio::runtime::Handle::try_current() {
            Ok(_) => Some(tokio::spawn(async move {
                let mut ticker = interval(poll_interval);
                // Skip the first immediate tick so we don't fire instantly on start.
                ticker.tick().await;
                loop {
                    ticker.tick().await;
                    let mgr = manager.lock().await;
                    match mgr.poll_sessions().await {
                        Ok(transitions) => {
                            for (name, old, new) in &transitions {
                                info!("TM monitor: {} {} → {}", name, old, new);
                            }
                        }
                        Err(e) => warn!("TM monitor poll error: {e:#}"),
                    }
                }
            })),
            Err(_) => {
                tracing::debug!(
                    "TM monitor: no tokio runtime active; monitor created in idle state"
                );
                None
            }
        };
        Self { handle }
    }

    /// Cancel the polling task.
    ///
    /// Why: Lets the REPL (or tests) stop the monitor without dropping the
    /// whole struct.
    /// What: Aborts the JoinHandle if present and clears it.
    /// Test: `tests::monitor_drop_stops_task`.
    pub fn stop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.abort();
        }
    }
}

impl Drop for TmMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: a freshly-started monitor can be stopped without panicking.
    ///
    /// Why: Verifies the lifecycle wiring (spawn + abort) without needing a
    /// live tmux server.
    /// What: Builds a TmManager only when tmux is available; otherwise the
    /// test exits early (it's still useful as a compile check).
    /// Test: itself.
    #[tokio::test]
    async fn monitor_drop_stops_task() {
        if !crate::tmux::TmuxOrchestrator::is_available() {
            return;
        }
        let dir = tempfile::TempDir::new().unwrap();
        let mgr = TmManager::new(dir.path()).unwrap();
        let arc = Arc::new(Mutex::new(mgr));
        let mut monitor = TmMonitor::start(Arc::clone(&arc), Duration::from_millis(50));
        // Give the ticker a chance to schedule (but not fire — the first tick
        // is skipped).
        tokio::time::sleep(Duration::from_millis(20)).await;
        monitor.stop();
        // Subsequent stop is a no-op.
        monitor.stop();
    }
}
