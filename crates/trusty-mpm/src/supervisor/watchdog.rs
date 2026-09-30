//! Liveness guards for the supervisor sweep loop (#8335).
//!
//! Why: on 2026-09-21 the supervisor's sweep future stopped being polled while
//! the process stayed up. The heartbeat froze for hours and nothing was logged,
//! because no step had a bound, nothing watched the heartbeat, and nothing
//! reported the loop ending. See #8335.
//! What: [`bounded`] caps one loop step and logs at `error` when it expires;
//! [`spawn_heartbeat_watchdog`] runs a separate task that logs at `error` once
//! the published `written_at` passes its staleness window; [`LoopExitGuard`]
//! logs at `error` when the loop ends without a shutdown signal.
//! Test: `a_wedged_sweep_times_out_and_the_loop_continues`,
//! `heartbeat_watchdog_flags_a_stale_snapshot_once`,
//! `loop_exit_is_reported_unless_shutdown_disarmed_it` in `super::tests`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::task::JoinHandle;
use tracing::{error, info};

use super::publish::{self, SupervisorMetricsStatus};

/// Await `fut` for at most `limit`; `None` means it was abandoned.
///
/// Why: an unbounded await on a future that never wakes wedged the loop with no
/// log line (#8335). Dropping the future is safe for the sweep: the resume
/// in-flight marker is an RAII guard, so an abandoned resume releases it.
/// What: wraps [`tokio::time::timeout`]; on expiry logs `what` and the bound at
/// `error` and returns `None`.
/// Test: `a_wedged_sweep_times_out_and_the_loop_continues`.
pub(super) async fn bounded<F: Future>(limit: Duration, what: &str, fut: F) -> Option<F::Output> {
    match tokio::time::timeout(limit, fut).await {
        Ok(out) => Some(out),
        Err(_) => {
            error!(
                limit_secs = limit.as_secs(),
                "supervisor: {what} did not finish within its bound; abandoning it so \
                 the sweep loop continues (#8335)"
            );
            None
        }
    }
}

/// Judge the published heartbeat once; returns whether it is now stale.
///
/// Why: the daemon reports a stale snapshot only when someone asks; the
/// supervisor's own log carried nothing while its heartbeat froze (#8335).
/// What: reads `path` via [`publish::read_status_at`]. On the transition into
/// `Stale` logs at `error`; on the transition back to `Current` logs at
/// `info`. An `Unavailable` read keeps the previous verdict, because the publish
/// path already logs its own failure. Logs once per episode, not per check.
/// Test: `heartbeat_watchdog_flags_a_stale_snapshot_once`.
pub(super) fn check_heartbeat(path: &Path, now: DateTime<Utc>, was_stale: bool) -> bool {
    match publish::read_status_at(path, now) {
        SupervisorMetricsStatus::Stale {
            snapshot,
            age_secs,
            stale_after_secs,
        } => {
            if !was_stale {
                error!(
                    path = %path.display(),
                    written_at = %snapshot.written_at,
                    age_secs,
                    stale_after_secs,
                    "supervisor: heartbeat is stale; the sweep loop has not published \
                     within its window (#8335)"
                );
            }
            true
        }
        SupervisorMetricsStatus::Current { age_secs, .. } => {
            if was_stale {
                info!(age_secs, "supervisor: heartbeat recovered");
            }
            false
        }
        SupervisorMetricsStatus::Unavailable { .. } => was_stale,
    }
}

/// Spawn the heartbeat watchdog on the runtime's worker pool.
///
/// Why: the watchdog must keep running while the loop's own future is stuck, so
/// it cannot live inside that future.
/// What: every `period`, calls [`check_heartbeat`] on `path`. Runs until the
/// returned handle is aborted.
/// Test: `heartbeat_watchdog_flags_a_stale_snapshot_once` covers the check; the
/// spawn is exercised by every `run_until` test.
pub(super) fn spawn_heartbeat_watchdog(path: PathBuf, period: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(period);
        let mut stale = false;
        loop {
            timer.tick().await;
            stale = check_heartbeat(&path, Utc::now(), stale);
        }
    })
}

/// Reports the sweep loop ending without a shutdown signal (#8335).
///
/// Why: the loop's future can be dropped or unwound by a panic, and neither
/// path passes through a `return`. Drop runs on both, so a drop guard is the one
/// place that sees every exit.
/// What: holds the watchdog handle and aborts it on drop. Unless
/// [`Self::disarm`] ran first, drop logs at `error`.
/// Test: `loop_exit_is_reported_unless_shutdown_disarmed_it`.
pub(super) struct LoopExitGuard {
    watchdog: Option<JoinHandle<()>>,
    armed: bool,
}

impl LoopExitGuard {
    /// Arm a guard that owns `watchdog` (if any).
    pub(super) fn new(watchdog: Option<JoinHandle<()>>) -> Self {
        Self {
            watchdog,
            armed: true,
        }
    }

    /// Mark the exit as the clean shutdown path.
    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }

    /// Whether dropping the guard now would log an unexpected exit.
    #[cfg(test)]
    pub(super) fn is_armed(&self) -> bool {
        self.armed
    }
}

impl Drop for LoopExitGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.watchdog.take() {
            handle.abort();
        }
        if self.armed {
            error!(
                panicking = std::thread::panicking(),
                "supervisor: sweep loop exited without a shutdown signal; fleet sweeps \
                 and heartbeat publishing have STOPPED (#8335)"
            );
        }
    }
}
