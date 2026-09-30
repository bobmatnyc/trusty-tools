//! Liveness guards for the supervisor sweep loop (#8335).
//!
//! Why: on 2026-09-21 the supervisor's sweep future stopped being polled while
//! the process stayed up. The heartbeat froze for hours and nothing was logged,
//! because no step had a bound, nothing watched the heartbeat, and nothing
//! reported the loop ending. See #8335.
//! What: [`DetachedSweep`] runs a sweep on the blocking pool under a wall-clock
//! bound, so a blocking `gh`, `git` or tmux call inside it cannot stall the loop
//! task; [`bounded`] caps an async step; [`HeartbeatWatch`], driven by
//! [`spawn_heartbeat_watchdog`] on its own task, logs at `error` once the
//! published heartbeat goes stale and skips its verdict across a system sleep;
//! [`LoopExitGuard`] logs at `error` when the loop ends without a shutdown
//! signal.
//! Test: `a_hung_gh_in_the_cleanup_sweep_is_abandoned_and_the_loop_continues`,
//! `abandoned_fleet_sweeps_are_counted_and_read_as_stale`,
//! `heartbeat_watchdog_flags_a_stale_snapshot_once`,
//! `heartbeat_watch_skips_its_verdict_across_a_wall_clock_jump`,
//! `an_armed_loop_exit_guard_logs_an_error_when_dropped` in `super::tests`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

use super::publish::{self, SupervisorMetricsStatus};

/// Await `fut` for at most `limit`; `None` means it was abandoned.
///
/// Why: an unbounded await on a future that never wakes wedged the loop with no
/// log line (#8335).
/// What: wraps [`tokio::time::timeout`]; on expiry logs `what` and the bound at
/// `error` and returns `None`. It fires only at an await point, so it bounds
/// async waits (a held lock), never a blocking call — see [`DetachedSweep`].
/// Test: `supervisor_publishes_run_stats_after_sweeps` (the in-bound path).
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

/// How much longer than the sweep's own bound the loop task waits for its thread.
///
/// Why: an async wait ends on the in-thread bound and frees the thread; the
/// loop task's later bound then fires only for a thread that is really blocked,
/// so an async stall never leaves an orphan behind.
const BLOCKED_GRACE: Duration = Duration::from_millis(500);

/// One kind of sweep, run off the loop task under a wall-clock bound (#8335).
///
/// Why: `RealGh::run`, `RealGit::run` and the tmux driver block their thread in
/// `Command::output()`. Run inline, a hung call held the loop task itself, so a
/// `tokio::time::timeout` around it never got polled to fire. Running the sweep
/// on the blocking pool leaves the loop task free to time it out.
/// What: [`Self::run`] spawns the sweep with `spawn_blocking` and drives it
/// there with `Handle::block_on`. Two bounds apply: `limit` inside that thread,
/// which ends an async wait by dropping the future (the thread is then free),
/// and `limit` plus [`BLOCKED_GRACE`] on the loop task, which fires even while
/// the thread is blocked. On that second expiry the task is kept as the orphan: its thread
/// stays blocked until the call returns, and the child process is NOT killed —
/// it runs to completion or until something else ends it. While the orphan
/// runs, every later call is skipped, so two sweeps of one kind never overlap
/// and at most one thread per kind is held. A thread still blocked at process
/// shutdown delays exit until its call returns.
/// Test: `a_hung_gh_in_the_cleanup_sweep_is_abandoned_and_the_loop_continues`,
/// `abandoned_fleet_sweeps_are_counted_and_read_as_stale`,
/// `a_wedged_sweep_times_out_and_the_loop_continues`.
pub(super) struct DetachedSweep<T> {
    /// The log name, e.g. `"fleet sweep"`.
    what: &'static str,
    /// A sweep abandoned at its bound whose thread has not finished yet.
    orphan: Option<JoinHandle<Option<T>>>,
}

impl<T: Send + 'static> DetachedSweep<T> {
    /// A sweep kind with no orphan.
    pub(super) fn new(what: &'static str) -> Self {
        Self { what, orphan: None }
    }

    /// Run one sweep; `None` when it was abandoned, skipped, or panicked.
    ///
    /// What: see the type docs. `make` builds the sweep future on the blocking
    /// thread, so the future itself need not be `Send`.
    /// Test: as the type.
    pub(super) async fn run<F, Fut>(&mut self, limit: Duration, make: F) -> Option<T>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T> + 'static,
    {
        let what = self.what;
        if let Some(orphan) = &self.orphan {
            if !orphan.is_finished() {
                error!(
                    "supervisor: the {what} abandoned earlier is still blocked; skipping \
                     this one so two never run at once (#8335)"
                );
                return None;
            }
            self.orphan = None;
            info!("supervisor: the abandoned {what} has finished");
        }
        let rt = tokio::runtime::Handle::current();
        let mut task = tokio::task::spawn_blocking(move || {
            rt.block_on(async move { tokio::time::timeout(limit, make()).await.ok() })
        });
        let limit_secs = limit.as_secs();
        match tokio::time::timeout(limit + BLOCKED_GRACE, &mut task).await {
            Ok(Ok(Some(out))) => Some(out),
            Ok(Ok(None)) => {
                error!(
                    limit_secs,
                    "supervisor: {what} did not finish within its bound; abandoned it at an \
                     await point (#8335)"
                );
                None
            }
            Ok(Err(e)) => {
                error!("supervisor: {what} did not complete: {e} (#8335)");
                None
            }
            Err(_) => {
                error!(
                    limit_secs,
                    "supervisor: {what} is blocked past its bound; abandoning it so the \
                     sweep loop continues. Its thread stays blocked until the call returns, \
                     and the child process is not killed (#8335)"
                );
                self.orphan = Some(task);
                None
            }
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
                    consecutive_sweeps_abandoned =
                        snapshot.fleet.run_stats.consecutive_sweeps_abandoned,
                    "supervisor: heartbeat is stale; no fleet sweep has completed and \
                     published within its window (#8335)"
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

/// The watchdog's state between checks.
///
/// Why (#8335 review): after a system sleep the heartbeat file is hours old
/// until the loop's next tick publishes, so a check on wake would log a false
/// ERROR.
/// What: [`Self::observe`] skips the verdict — keeping the previous one — when
/// the wall-clock gap since the previous check exceeds twice the period, and
/// otherwise calls [`check_heartbeat`].
/// Test: `heartbeat_watch_skips_its_verdict_across_a_wall_clock_jump`.
pub(super) struct HeartbeatWatch {
    /// The check cadence.
    period: Duration,
    /// When the previous check ran.
    last_check: Option<DateTime<Utc>>,
    /// The current verdict.
    stale: bool,
}

impl HeartbeatWatch {
    /// A watch that checks every `period`.
    pub(super) fn new(period: Duration) -> Self {
        Self {
            period,
            last_check: None,
            stale: false,
        }
    }

    /// Check `path` as of `now`; returns whether the heartbeat is stale.
    pub(super) fn observe(&mut self, path: &Path, now: DateTime<Utc>) -> bool {
        let gap = self.last_check.map(|prev| now - prev);
        self.last_check = Some(now);
        let max_gap = chrono::Duration::from_std(self.period.saturating_mul(2))
            .unwrap_or(chrono::Duration::MAX);
        if let Some(gap) = gap.filter(|g| *g > max_gap) {
            debug!(
                gap_secs = gap.num_seconds(),
                "supervisor: wall clock jumped since the last heartbeat check (system \
                 sleep?); skipping this verdict"
            );
            return self.stale;
        }
        self.stale = check_heartbeat(path, now, self.stale);
        self.stale
    }
}

/// Spawn the heartbeat watchdog on the runtime's worker pool.
///
/// Why: the watchdog must keep running while the loop's own future is stuck, so
/// it cannot live inside that future.
/// What: every `period`, calls [`HeartbeatWatch::observe`] on `path`. Runs
/// until the returned handle is aborted.
/// Test: `heartbeat_watch_skips_its_verdict_across_a_wall_clock_jump` covers the
/// check; the spawn is exercised by every `run_until` test.
pub(super) fn spawn_heartbeat_watchdog(path: PathBuf, period: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(period);
        let mut watch = HeartbeatWatch::new(period);
        loop {
            timer.tick().await;
            watch.observe(&path, Utc::now());
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
/// Test: `an_armed_loop_exit_guard_logs_an_error_when_dropped`,
/// `supervisor_run_until_stops_cleanly`.
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
