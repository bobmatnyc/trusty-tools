//! Unattended supervisor: 24/7 fleet observer.
//!
//! Why: the session manager normally needs a live calling agentic process to keep
//! a fleet moving. For overnight / unattended operation (#1206) we need an
//! always-on, lightweight supervisor that observes session health without a
//! caller, surfaces `pending_decision`s
//! for a human, and survives reboots under launchd/systemd. It is a PASSIVE
//! observer — it never makes autonomy decisions and never auto-answers a decision;
//! it feeds fleet state back to a human or a higher-level fleet manager.
//! What: re-exports the config, metrics, poller, and publication types, and
//! defines [`Supervisor`] — the long-running loop that, each tick, runs a fleet
//! sweep ([`poller::run_tick`]), folds the result into [`SupervisorRunStats`],
//! and publishes a fresh [`FleetMetrics`] snapshot to
//! `<framework root>/supervisor-metrics.json` for the daemon to read (#6288).
//! #10356 (owner ruling D74): it no longer auto-resumes. The daemon does, in
//! `crate::daemon::auto_resume_loop`, over [`poller::resume_sweep`], because
//! only the daemon installs the runtime relauncher a resume needs.
//! Test: `super::tests` covers config parsing, metrics derivation, the per-tick
//! sweep (including an N-session fleet), and the publish/read round trip.
//!
//! [`Supervisor`]: crate::supervisor::Supervisor
//! [`poller::run_tick`]: crate::supervisor::poller::run_tick
//! [`poller::resume_sweep`]: crate::supervisor::poller::resume_sweep
//! [`SupervisorRunStats`]: crate::supervisor::SupervisorRunStats
//! [`FleetMetrics`]: crate::supervisor::FleetMetrics

pub mod config;
pub mod metrics;
pub mod poller;
// #7275: the periodic post-merge cleanup sweep, on its own cadence.
pub mod pr_cleanup_tick;
pub mod publish;
// #8335: step bounds, the heartbeat watchdog and the loop-exit report.
// #10356: `pub(crate)` so the daemon's auto-resume loop shares `DetachedSweep`.
pub(crate) mod watchdog;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use tracing::{error, info, warn};

use crate::activity::monitor::{ActivityMonitor, LlmClassifier};
use crate::core::paths::FrameworkPaths;
use crate::session_manager::SessionManager;

pub use config::SupervisorConfig;
pub use metrics::{FleetMetrics, PendingDecision, SupervisorRunStats};
pub use poller::{TickReport, resume_sweep, run_tick};
pub use publish::{PublishedMetrics, SupervisorMetricsStatus};

/// The unattended fleet supervisor.
///
/// Why: bundling the session manager handle, the config, an optional activity
/// classifier, and the live run stats into one struct gives the loop a single
/// owner and makes the supervisor straightforward to construct and test.
/// What: holds an `Arc<SessionManager>`, an immutable [`SupervisorConfig`], an
/// optional [`ActivityMonitor`] (classification is skipped when `None` or when
/// `cfg.classify_idle` is false), and the accumulating [`SupervisorRunStats`].
/// Test: `supervisor_tick_updates_stats`, `supervisor_snapshot_reflects_fleet`.
pub struct Supervisor<C: LlmClassifier> {
    /// The session manager whose fleet this supervisor watches.
    mgr: Arc<SessionManager>,
    /// Immutable configuration (cadence + policy). #10356: its `auto_resume`
    /// is ignored here; the daemon owns auto-resume.
    cfg: SupervisorConfig,
    /// Optional activity classifier for idle `active` sessions; shared so the
    /// sweep can run on the blocking pool (#8335).
    monitor: Option<Arc<ActivityMonitor<C>>>,
    /// Cumulative counters across every sweep this run.
    stats: SupervisorRunStats,
    /// #6288: where each sweep's snapshot is published for the daemon to read.
    metrics_path: PathBuf,
    /// #7275: when the post-merge cleanup sweep last ran, so its cadence is
    /// independent of the fleet sweep's.
    last_pr_cleanup: Option<std::time::Instant>,
    /// #8335: the fleet sweep, run off the loop task under `tick_timeout`.
    fleet_sweep: watchdog::DetachedSweep<TickReport>,
    /// #8335: the post-merge cleanup sweep, likewise.
    cleanup_sweep: watchdog::DetachedSweep<usize>,
    /// #8335: what the cleanup sweep runs; a seam so a test can hand it a `gh`
    /// that hangs.
    cleanup: CleanupFn,
}

/// The post-merge cleanup sweep as the supervisor calls it (#8335).
///
/// The returned future is built and driven on a blocking-pool thread, so it
/// need not be `Send`.
type CleanupFn = Arc<
    dyn Fn(Arc<SessionManager>) -> std::pin::Pin<Box<dyn Future<Output = usize>>> + Send + Sync,
>;

impl<C: LlmClassifier + 'static> Supervisor<C> {
    /// Construct a supervisor over a session manager and config.
    ///
    /// Why: callers wire the supervisor with whatever classifier they have (a real
    /// `OpenRouterClassifier` in production, a mock in tests, or `None` to skip
    /// classification); dependency injection keeps the loop testable offline.
    /// What: stores the handles and zeroes the run stats.
    /// Test: every `supervisor_*` test constructs via this.
    pub fn new(
        mgr: Arc<SessionManager>,
        cfg: SupervisorConfig,
        monitor: Option<ActivityMonitor<C>>,
    ) -> Self {
        Self {
            mgr,
            cfg,
            monitor: monitor.map(Arc::new),
            stats: SupervisorRunStats::default(),
            // #6288: the same `~/.trusty-mpm` root the daemon reads from, so
            // production wiring needs no extra call.
            metrics_path: publish::metrics_path(&FrameworkPaths::default()),
            // #7275: `None` makes the first tick after a restart sweep, rather
            // than waiting a full cleanup interval to notice a merge that
            // landed while the daemon was down.
            last_pr_cleanup: None,
            fleet_sweep: watchdog::DetachedSweep::new("fleet sweep"),
            cleanup_sweep: watchdog::DetachedSweep::new("post-merge cleanup sweep"),
            cleanup: Arc::new(|mgr| {
                Box::pin(async move { pr_cleanup_tick::run_sweep(&mgr).await })
            }),
        }
    }

    /// Replace what the post-merge cleanup sweep runs (#8335).
    ///
    /// Why: the production sweep spawns the real `gh`; a test needs one that
    /// hangs, driven through the same bounded path.
    /// Test: `a_hung_gh_in_the_cleanup_sweep_is_abandoned_and_the_loop_continues`.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_cleanup(mut self, cleanup: CleanupFn) -> Self {
        self.cleanup = cleanup;
        self
    }

    /// Point the supervisor at a specific published-metrics file (#6288).
    ///
    /// Why: `new` resolves the real `~/.trusty-mpm/supervisor-metrics.json`, so a
    /// test that let the loop publish would overwrite the developer's own live
    /// snapshot. Tests pin a temp path; production keeps the default.
    /// What: replaces [`Self::metrics_path`] and returns `self` for chaining.
    /// Test: `supervisor_publishes_run_stats_after_sweeps`.
    #[must_use]
    pub fn with_metrics_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.metrics_path = path.into();
        self
    }

    /// The path this supervisor publishes its snapshot to.
    ///
    /// Test: `supervisor_publishes_run_stats_after_sweeps`.
    pub fn metrics_path(&self) -> &std::path::Path {
        &self.metrics_path
    }

    /// Borrow the current cumulative run stats.
    ///
    /// Why: tests and the metrics snapshot need read access to the counters the
    /// loop maintains.
    /// What: returns a reference to the live [`SupervisorRunStats`].
    /// Test: `supervisor_tick_updates_stats`.
    pub fn stats(&self) -> &SupervisorRunStats {
        &self.stats
    }

    /// Run one sweep and fold the result into the cumulative run stats.
    ///
    /// Why: the timer loop and the tests both need "do exactly one sweep and
    /// update stats" as an atomic step; exposing it separately keeps the loop a
    /// trivial timer wrapper and lets tests advance the supervisor deterministically.
    /// What: calls [`poller::run_tick`], increments `sweeps`, and adds the
    /// tick's classified count into `self.stats`; returns the [`TickReport`]
    /// for the caller to inspect. #10356: the sweep resumes nothing, so
    /// `auto_resumed` and `resume_failures` stay at zero here. #8335:
    /// the sweep runs on the blocking pool under `cfg.tick_timeout`
    /// ([`watchdog::DetachedSweep`]); one that does not complete returns an
    /// empty report, is not counted in `sweeps`, and bumps `sweeps_abandoned`
    /// and `consecutive_sweeps_abandoned`.
    /// Test: `supervisor_tick_updates_stats`, `the_supervisor_tick_no_longer_resumes`,
    /// `a_wedged_sweep_times_out_and_the_loop_continues`,
    /// `abandoned_fleet_sweeps_are_counted_and_read_as_stale`.
    pub async fn tick(&mut self) -> TickReport {
        // #10356 (owner ruling D74): this process installs no runtime
        // relauncher, so it must never auto-resume. The daemon owns that.
        let cfg = SupervisorConfig {
            auto_resume: false,
            ..self.cfg.clone()
        };
        // #8335: a sweep blocked in a tmux call held this task, so no timeout
        // around it could fire. It runs on the blocking pool instead.
        let (mgr, monitor) = (Arc::clone(&self.mgr), self.monitor.clone());
        let sweep = move || async move { run_tick(&mgr, &cfg, monitor.as_deref()).await };
        let Some(report) = self.fleet_sweep.run(self.cfg.tick_timeout, sweep).await else {
            self.stats.sweeps_abandoned += 1;
            self.stats.consecutive_sweeps_abandoned += 1;
            return TickReport::default();
        };
        self.stats.consecutive_sweeps_abandoned = 0;
        self.pr_cleanup_tick().await;
        self.stats.sweeps += 1;
        self.stats.classified += report.classified as u64;
        report
    }

    /// Run the post-merge cleanup sweep when its own cadence says it is due
    /// (#7275, owner scope amendment 2026-09-09).
    ///
    /// Why: a merge determines that a worktree, its branches and its session
    /// claim are obsolete, and merges that happen outside `tm pr merge` have
    /// nothing else watching for them. Running the sweep here — inside the tick
    /// the supervisor already owns — means no second timer can run it
    /// concurrently with itself.
    /// What: gates on [`pr_cleanup_tick::due`] against
    /// `cfg.pr_cleanup_interval`, runs the sweep, and stamps
    /// [`Self::last_pr_cleanup`]. The stamp is written whether or not the sweep
    /// cleaned anything, so a repository with nothing to do costs one sweep per
    /// interval rather than one per tick.
    /// Test: `pr_cleanup_is_due_only_after_the_interval`; the sweep's own logic
    /// is `sweep_stamps_only_a_fully_successful_run`.
    async fn pr_cleanup_tick(&mut self) {
        let now = std::time::Instant::now();
        if !pr_cleanup_tick::due(self.cfg.pr_cleanup_interval, self.last_pr_cleanup, now) {
            return;
        }
        self.last_pr_cleanup = Some(now);
        // #8335: `RealGh::run` and `RealGit::run` block their thread in
        // `Command::output()`, so this sweep runs on the blocking pool. The loop
        // stops waiting for it after `cfg.tick_timeout` (an await inside it is
        // dropped at that bound), plus 500 ms for a blocked call. A call still
        // blocked then is NOT killed: its thread and child process keep running
        // until the call returns, and every later cleanup sweep is skipped and
        // counted in `cleanup_sweeps_abandoned` until it does.
        let (mgr, cleanup) = (Arc::clone(&self.mgr), Arc::clone(&self.cleanup));
        let sweep = move || cleanup(mgr);
        let limit = self.cfg.tick_timeout;
        let Some(cleaned) = self.cleanup_sweep.run(limit, sweep).await else {
            self.stats.cleanup_sweeps_abandoned += 1;
            return;
        };
        if cleaned > 0 {
            info!(cleaned, "supervisor: post-merge cleanup swept merged PRs");
        }
    }

    /// Compute a fresh fleet-metrics snapshot overlaid with current run stats.
    ///
    /// Why: the published snapshot must reflect both the authoritative session
    /// store AND what the supervisor itself has done; this composes the two.
    /// What: derives [`FleetMetrics::from_records`] from `mgr.list()` and copies the
    /// live `run_stats` in. #8394: first attaches each record's context reading
    /// from the usage store under the metrics file's root.
    /// Test: `supervisor_snapshot_reflects_fleet`.
    pub async fn snapshot(&self) -> FleetMetrics {
        let mut records = self.mgr.list().await;
        // #8394: `metrics_path` is `<root>/supervisor-metrics.json`, and the
        // statusline writes its readings under the same root.
        if let Some(root) = self.metrics_path.parent() {
            metrics::attach_context_usage(&mut records, root, chrono::Utc::now());
        }
        let mut m = FleetMetrics::from_records(&records);
        m.run_stats = self.stats.clone();
        m
    }

    /// Publish the current snapshot for the daemon to read (#6288).
    ///
    /// Why: `run_stats` lives only in this process. Until #6288 it left the
    /// supervisor over a second HTTP listener nothing read, so the daemon's
    /// `console_metrics` / `supervisor_status` reported zero sweeps forever.
    /// Writing the snapshot to `<framework root>/supervisor-metrics.json` after
    /// every sweep is what makes those counters real.
    /// What: computes [`Self::snapshot`] and writes it atomically via
    /// [`publish::write_at`]. BEST-EFFORT by design: a publish failure is logged
    /// at `error` and the loop continues, because losing observability must not
    /// stop the supervisor from auto-resuming sessions. The reader reports the
    /// resulting staleness rather than a silent zero, so a persistent failure is
    /// visible in the console instead of being swallowed here.
    /// Test: `supervisor_publishes_run_stats_after_sweeps`.
    pub async fn publish_snapshot(&self) {
        // #8335: `snapshot` reads the session store; a held lock must not wedge
        // the loop. A skipped publish shows up as a stale heartbeat.
        let read = self.snapshot();
        let Some(snapshot) =
            watchdog::bounded(self.cfg.tick_timeout, "metrics snapshot", read).await
        else {
            return;
        };
        // #6288: the cadence travels with the snapshot so the reader can size its
        // staleness window against the interval this supervisor actually runs at,
        // rather than a fixed constant a slow overnight cadence would trip.
        if let Err(e) = publish::write_at(
            &self.metrics_path,
            &snapshot,
            self.cfg.interval,
            chrono::Utc::now(),
        ) {
            error!(
                path = %self.metrics_path.display(),
                "supervisor: cannot publish metrics snapshot: {e}; \
                 the console will report it as unavailable/stale"
            );
        }
    }

    /// Run the supervisor loop until an OS shutdown signal arrives.
    ///
    /// Why: this is the unattended heartbeat — it keeps observing
    /// (classification) and keeps the published snapshot fresh, with no live
    /// caller. Per the project's connection-safe restart convention (CLAUDE.md
    /// #534) it must stop *cleanly* on SIGTERM / Ctrl-C: never killed mid-sweep.
    /// What: delegates to [`Self::run_until`] with a shutdown future that resolves
    /// on `SIGTERM` (unix) or Ctrl-C, so the loop finishes the in-flight sweep,
    /// emits a final tracing line, and returns `Ok(())`.
    /// Test: the per-iteration behavior is tested via `tick` + `snapshot`; clean
    /// shutdown is tested via `run_until` with an injected shutdown future
    /// (`supervisor_run_until_stops_cleanly`).
    pub async fn run(self) -> anyhow::Result<()> {
        self.run_until(shutdown_signal()).await
    }

    /// Run the supervisor loop until `shutdown` resolves, publishing snapshots.
    ///
    /// Why: a bare `loop { timer.tick().await; … }` is killed mid-sweep when the
    /// process is signalled, which can leave an auto-resume half-applied. Selecting
    /// the timer tick against an injectable shutdown future gives a clean stop AND
    /// keeps the loop testable: production passes the OS-signal future, a unit test
    /// passes a future it controls (e.g. a oneshot) to trigger a deterministic stop.
    /// What: on an interval timer (`cfg.interval`), `select!`s the next tick against
    /// `shutdown`. On a tick it runs [`Self::tick`] and republishes the snapshot
    /// via [`Self::publish_snapshot`]; once `shutdown` resolves it breaks the loop
    /// *after* any in-flight sweep completes, logs a final line, and returns
    /// `Ok(())`. #8335: each step is bounded by `cfg.tick_timeout`, a watchdog
    /// task logs at `error` when the published heartbeat goes stale, and any
    /// exit other than `shutdown` (a drop or a panic) is logged at `error`.
    /// Test: `supervisor_run_until_stops_cleanly`,
    /// `a_wedged_sweep_times_out_and_the_loop_continues`.
    pub async fn run_until(mut self, shutdown: impl Future<Output = ()>) -> anyhow::Result<()> {
        info!(
            interval_secs = self.cfg.interval.as_secs(),
            classify_idle = self.cfg.classify_idle,
            "supervisor loop starting (observe-only; the daemon owns auto-resume, #10356)"
        );
        let mut timer = tokio::time::interval(self.cfg.interval);
        // #6288: publish an initial snapshot before the first sleep so the
        // console sees the supervisor immediately on startup rather than after
        // one full interval (during which it would read `unavailable`).
        self.publish_snapshot().await;
        // #8335: the watchdog runs on its own task so a stuck loop cannot silence
        // it; the guard logs at `error` if the loop ends any way but shutdown.
        let watchdog =
            watchdog::spawn_heartbeat_watchdog(self.metrics_path.clone(), self.cfg.interval);
        let mut exit_guard = watchdog::LoopExitGuard::new(Some(watchdog));
        // Pin the shutdown future so it can be polled across loop iterations.
        let mut shutdown = std::pin::pin!(shutdown);
        loop {
            tokio::select! {
                // Bias toward shutdown so a signal that arrives during a long
                // interval is never starved by the timer.
                biased;
                () = &mut shutdown => {
                    exit_guard.disarm();
                    info!(
                        sweeps = self.stats.sweeps,
                        "supervisor received shutdown signal; stopping cleanly"
                    );
                    return Ok(());
                }
                _ = timer.tick() => {
                    self.tick().await;
                    self.publish_snapshot().await;
                }
            }
        }
    }
}

/// Resolve when the process receives an OS shutdown signal (SIGTERM / Ctrl-C).
///
/// Why: an unattended launchd/systemd daemon is stopped with `SIGTERM`
/// (`launchctl bootout`), and an interactive run is stopped with Ctrl-C
/// (`SIGINT`); the supervisor must treat either as a clean-shutdown request so it
/// never dies mid-sweep (CLAUDE.md #534).
/// What: completes on the first of `SIGTERM` (unix only) or Ctrl-C; on a signal
/// installation error it logs and resolves immediately (fail-stop rather than
/// hang). On non-unix targets (Windows) `SIGTERM` is unavailable, so shutdown is
/// intentionally limited to Ctrl-C — acceptable because the supported persistence
/// targets (launchd/systemd) are both unix. Side-effect-only beyond the future.
/// Test: covered indirectly — `run`'s shutdown path is unit-tested via
/// `run_until` with an injected future, since real OS signals can't be raised
/// deterministically in a parallel test binary.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            warn!("failed to listen for Ctrl-C: {e}");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => warn!("failed to install SIGTERM handler: {e}"),
        }
    };

    // On non-unix targets (Windows) there is no SIGTERM; `tokio::signal::unix`
    // is unix-only. Shutdown there is intentionally limited to Ctrl-C — the
    // production deployment targets (launchd on macOS, systemd on Linux) are both
    // unix and stop the daemon with SIGTERM, so Windows is a best-effort/dev path.
    // A never-resolving future keeps the `select!` arm valid while deferring
    // entirely to the Ctrl-C handler.
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
