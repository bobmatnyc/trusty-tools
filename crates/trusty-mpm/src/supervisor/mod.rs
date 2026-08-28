//! Unattended supervisor: 24/7 fleet observer + auto-resumer.
//!
//! Why: the session manager normally needs a live calling agentic process to keep
//! a fleet moving. For overnight / unattended operation (#1206) we need an
//! always-on, lightweight supervisor that auto-resumes enduring (`stopped`)
//! sessions, observes session health without a caller, surfaces `pending_decision`s
//! for a human, and survives reboots under launchd/systemd. It is a PASSIVE
//! observer — it never makes autonomy decisions and never auto-answers a decision;
//! it feeds fleet state back to a human or a higher-level fleet manager.
//! What: re-exports the config, metrics, poller, and publication types, and
//! defines [`Supervisor`] — the long-running loop that, each tick, runs a fleet
//! sweep ([`poller::run_tick`]), folds the result into [`SupervisorRunStats`],
//! and publishes a fresh [`FleetMetrics`] snapshot to
//! `<framework root>/supervisor-metrics.json` for the daemon to read (#6288).
//! Test: `super::tests` covers config parsing, metrics derivation, the per-tick
//! sweep (including an N-session fleet), and the publish/read round trip.
//!
//! [`Supervisor`]: crate::supervisor::Supervisor
//! [`poller::run_tick`]: crate::supervisor::poller::run_tick
//! [`SupervisorRunStats`]: crate::supervisor::SupervisorRunStats
//! [`FleetMetrics`]: crate::supervisor::FleetMetrics

pub mod config;
pub mod metrics;
pub mod poller;
pub mod publish;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use tracing::{error, info, warn};

use crate::activity::monitor::{ActivityMonitor, LlmClassifier};
use crate::core::auto_resume;
use crate::core::paths::FrameworkPaths;
use crate::session_manager::SessionManager;

pub use config::SupervisorConfig;
pub use metrics::{FleetMetrics, PendingDecision, SupervisorRunStats};
pub use poller::{TickReport, run_tick};
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
    /// Immutable BOOT configuration (cadence + policy). `auto_resume` here is
    /// the boot-time env / CLI value; the flag actually in force each sweep is
    /// [`Self::resolve_auto_resume`]'s result (#5208).
    cfg: SupervisorConfig,
    /// Optional activity classifier for idle `active` sessions.
    monitor: Option<ActivityMonitor<C>>,
    /// Cumulative counters across every sweep this run.
    stats: SupervisorRunStats,
    /// #5208: the console-written desired-state file, re-read every sweep.
    auto_resume_path: PathBuf,
    /// #5208: the last override read successfully, so a transient read failure
    /// cannot silently flip auto-resume off.
    last_override: Option<bool>,
    /// #6288: where each sweep's snapshot is published for the daemon to read.
    metrics_path: PathBuf,
}

impl<C: LlmClassifier> Supervisor<C> {
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
            monitor,
            stats: SupervisorRunStats::default(),
            // #5208: default to the same `~/.trusty-mpm/auto_resume` the console
            // writes, so production wiring needs no extra call.
            auto_resume_path: auto_resume::desired_path(&FrameworkPaths::default()),
            last_override: None,
            // #6288: the same `~/.trusty-mpm` root the daemon reads from, so
            // production wiring needs no extra call.
            metrics_path: publish::metrics_path(&FrameworkPaths::default()),
        }
    }

    /// Point the supervisor at a specific auto-resume desired-state file.
    ///
    /// Why: `new` resolves the real `~/.trusty-mpm/auto_resume`, which would make
    /// every supervisor test depend on the developer's own console toggle. Tests
    /// pin a temp path instead; production keeps the default.
    /// What: replaces [`Self::auto_resume_path`] and returns `self` for chaining.
    /// Test: `supervisor_honours_console_desired_state_without_restart`.
    #[must_use]
    pub fn with_auto_resume_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.auto_resume_path = path.into();
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

    /// Resolve the auto-resume flag in force for the sweep about to run.
    ///
    /// Why: #5208 — an operator toggling auto-resume in the console wrote
    /// `~/.trusty-mpm/auto_resume` and nothing read it, so the write reported
    /// success while the running supervisor kept its boot-time behavior. Reading
    /// the file every sweep is what makes the console control real without a
    /// process restart.
    /// What: applies the documented precedence — persisted file when present >
    /// boot-time `TRUSTY_MPM_AUTO_RESUME` / `--auto-resume` > off — logging each
    /// transition once rather than every sweep. On a read error it holds the last
    /// value it observed (falling back to the boot flag) and logs at `error`; it
    /// never fails open to `false`, which would re-create the original defect one
    /// layer out by letting an unreadable file disable an enabled supervisor.
    /// Test: `supervisor_honours_console_desired_state_without_restart`,
    /// `supervisor_console_disable_overrides_env_enabled`,
    /// `supervisor_absent_desired_file_keeps_boot_flag`,
    /// `supervisor_unreadable_desired_file_does_not_disable_resume`.
    fn resolve_auto_resume(&mut self) -> bool {
        match auto_resume::read_override_at(&self.auto_resume_path) {
            Ok(Some(desired)) => {
                if self.last_override != Some(desired) {
                    info!(
                        desired,
                        path = %self.auto_resume_path.display(),
                        "supervisor: auto-resume desired state applied from console"
                    );
                    self.last_override = Some(desired);
                }
                desired
            }
            Ok(None) => {
                if self.last_override.is_some() {
                    info!(
                        boot_flag = self.cfg.auto_resume,
                        path = %self.auto_resume_path.display(),
                        "supervisor: auto-resume override removed; reverting to boot flag"
                    );
                    self.last_override = None;
                }
                self.cfg.auto_resume
            }
            Err(e) => {
                let held = self.last_override.unwrap_or(self.cfg.auto_resume);
                error!(
                    path = %self.auto_resume_path.display(),
                    auto_resume = held,
                    "supervisor: cannot read auto-resume desired state: {e}; \
                     holding the last known value (NOT failing open to off)"
                );
                held
            }
        }
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
    /// What: re-resolves the auto-resume flag from the console's persisted
    /// desired-state file (#5208 — [`Self::resolve_auto_resume`]), calls
    /// [`poller::run_tick`] with the resulting per-sweep config, increments
    /// `sweeps`, and adds the tick's resumed / failure / classified counts into
    /// `self.stats`; returns the [`TickReport`] for the caller to inspect.
    /// Test: `supervisor_tick_updates_stats`, `supervisor_fleet_resume_e2e`,
    /// `supervisor_honours_console_desired_state_without_restart`.
    pub async fn tick(&mut self) -> TickReport {
        // #5208: the console toggle is re-read every sweep, so an operator's
        // change takes effect within one interval instead of never.
        let cfg = SupervisorConfig {
            auto_resume: self.resolve_auto_resume(),
            ..self.cfg.clone()
        };
        let report = run_tick(&self.mgr, &cfg, self.monitor.as_ref()).await;
        self.stats.sweeps += 1;
        self.stats.auto_resumed += report.resumed.len() as u64;
        self.stats.resume_failures += report.resume_failures as u64;
        self.stats.classified += report.classified as u64;
        report
    }

    /// Compute a fresh fleet-metrics snapshot overlaid with current run stats.
    ///
    /// Why: the published snapshot must reflect both the authoritative session
    /// store AND what the supervisor itself has done; this composes the two.
    /// What: derives [`FleetMetrics::from_records`] from `mgr.list()` and copies the
    /// live `run_stats` in.
    /// Test: `supervisor_snapshot_reflects_fleet`.
    pub async fn snapshot(&self) -> FleetMetrics {
        let records = self.mgr.list().await;
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
        let snapshot = self.snapshot().await;
        if let Err(e) = publish::write_at(&self.metrics_path, &snapshot, chrono::Utc::now()) {
            error!(
                path = %self.metrics_path.display(),
                "supervisor: cannot publish metrics snapshot: {e}; \
                 the console will report it as unavailable/stale"
            );
        }
    }

    /// Run the supervisor loop until an OS shutdown signal arrives.
    ///
    /// Why: this is the unattended heartbeat — it keeps the fleet moving (auto-
    /// resume), keeps observing (classification), and keeps the published
    /// snapshot fresh, with no live caller. Per the project's connection-safe
    /// restart convention (CLAUDE.md #534) it must stop *cleanly* on SIGTERM /
    /// Ctrl-C: never killed mid-sweep, so a `cargo install` + restart cannot
    /// interrupt a half-applied auto-resume.
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
    /// `Ok(())`.
    /// Test: `supervisor_run_until_stops_cleanly`.
    pub async fn run_until(mut self, shutdown: impl Future<Output = ()>) -> anyhow::Result<()> {
        // #5208: report the flag actually in force at boot, not just the env one —
        // the persisted console override outranks it and is consulted every sweep.
        let boot_auto_resume = self.resolve_auto_resume();
        info!(
            interval_secs = self.cfg.interval.as_secs(),
            auto_resume = boot_auto_resume,
            auto_resume_env = self.cfg.auto_resume,
            auto_resume_path = %self.auto_resume_path.display(),
            classify_idle = self.cfg.classify_idle,
            "supervisor loop starting"
        );
        if !boot_auto_resume {
            warn!(
                "supervisor: auto-resume DISABLED ({}=1, or the console toggle writing {}, \
                 to enable); running as observe-only",
                config::ENV_AUTO_RESUME,
                self.auto_resume_path.display()
            );
        }
        let mut timer = tokio::time::interval(self.cfg.interval);
        // #6288: publish an initial snapshot before the first sleep so the
        // console sees the supervisor immediately on startup rather than after
        // one full interval (during which it would read `unavailable`).
        self.publish_snapshot().await;
        // Pin the shutdown future so it can be polled across loop iterations.
        let mut shutdown = std::pin::pin!(shutdown);
        loop {
            tokio::select! {
                // Bias toward shutdown so a signal that arrives during a long
                // interval is never starved by the timer.
                biased;
                () = &mut shutdown => {
                    info!(
                        sweeps = self.stats.sweeps,
                        auto_resumed = self.stats.auto_resumed,
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
