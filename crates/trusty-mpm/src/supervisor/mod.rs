//! Unattended supervisor: 24/7 fleet observer + auto-resumer.
//!
//! Why: the session manager normally needs a live calling agentic process to keep
//! a fleet moving. For overnight / unattended operation (#1206) we need an
//! always-on, lightweight supervisor that auto-resumes enduring (`stopped`)
//! sessions, observes session health without a caller, surfaces `pending_decision`s
//! for a human, and survives reboots under launchd/systemd. It is a PASSIVE
//! observer — it never makes autonomy decisions and never auto-answers a decision;
//! it feeds fleet state back to a human or a higher-level fleet manager.
//! What: re-exports the config, metrics, poller, and (feature-gated) HTTP types,
//! and defines [`Supervisor`] — the long-running loop that, each tick, runs a
//! fleet sweep ([`poller::run_tick`]), folds the result into [`SupervisorRunStats`],
//! and publishes a fresh [`FleetMetrics`] snapshot for the `/metrics` endpoint.
//! Test: `super::tests` covers config parsing, metrics derivation, the per-tick
//! sweep (including an N-session fleet), and the HTTP handlers.

pub mod config;
pub mod metrics;
pub mod poller;

#[cfg(feature = "daemon")]
pub mod http;

#[cfg(test)]
mod tests;

use std::future::Future;
use std::sync::Arc;

use tracing::{info, warn};

use crate::activity::monitor::{ActivityMonitor, LlmClassifier};
use crate::session_manager::SessionManager;

pub use config::SupervisorConfig;
pub use metrics::{FleetMetrics, PendingDecision, SupervisorRunStats};
pub use poller::{TickReport, run_tick};

#[cfg(feature = "daemon")]
pub use http::{MetricsHandle, new_handle};

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
    /// Immutable run configuration (cadence + policy).
    cfg: SupervisorConfig,
    /// Optional activity classifier for idle `active` sessions.
    monitor: Option<ActivityMonitor<C>>,
    /// Cumulative counters across every sweep this run.
    stats: SupervisorRunStats,
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
    /// What: calls [`poller::run_tick`], increments `sweeps`, and adds the tick's
    /// resumed / failure / classified counts into `self.stats`; returns the
    /// [`TickReport`] for the caller to inspect.
    /// Test: `supervisor_tick_updates_stats`, `supervisor_fleet_resume_e2e`.
    pub async fn tick(&mut self) -> TickReport {
        let report = run_tick(&self.mgr, &self.cfg, self.monitor.as_ref()).await;
        self.stats.sweeps += 1;
        self.stats.auto_resumed += report.resumed.len() as u64;
        self.stats.resume_failures += report.resume_failures as u64;
        self.stats.classified += report.classified as u64;
        report
    }

    /// Compute a fresh fleet-metrics snapshot overlaid with current run stats.
    ///
    /// Why: the `/metrics` endpoint must reflect both the authoritative session
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

    /// Run the supervisor loop until an OS shutdown signal arrives.
    ///
    /// Why: this is the unattended heartbeat — it keeps the fleet moving (auto-
    /// resume), keeps observing (classification), and keeps the `/metrics`
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
    #[cfg(feature = "daemon")]
    pub async fn run(self, handle: http::MetricsHandle) -> anyhow::Result<()> {
        self.run_until(handle, shutdown_signal()).await
    }

    /// Run the supervisor loop until `shutdown` resolves, publishing snapshots.
    ///
    /// Why: a bare `loop { timer.tick().await; … }` is killed mid-sweep when the
    /// process is signalled, which can leave an auto-resume half-applied. Selecting
    /// the timer tick against an injectable shutdown future gives a clean stop AND
    /// keeps the loop testable: production passes the OS-signal future, a unit test
    /// passes a future it controls (e.g. a oneshot) to trigger a deterministic stop.
    /// What: on an interval timer (`cfg.interval`), `select!`s the next tick against
    /// `shutdown`. On a tick it runs [`Self::tick`] and republishes [`Self::snapshot`]
    /// into `handle`; once `shutdown` resolves it breaks the loop *after* any
    /// in-flight sweep completes, logs a final line, and returns `Ok(())`.
    /// Test: `supervisor_run_until_stops_cleanly`.
    #[cfg(feature = "daemon")]
    pub async fn run_until(
        mut self,
        handle: http::MetricsHandle,
        shutdown: impl Future<Output = ()>,
    ) -> anyhow::Result<()> {
        info!(
            interval_secs = self.cfg.interval.as_secs(),
            auto_resume = self.cfg.auto_resume,
            classify_idle = self.cfg.classify_idle,
            "supervisor loop starting"
        );
        if !self.cfg.auto_resume {
            warn!(
                "supervisor: auto-resume DISABLED ({}=1 to enable); running as observe-only",
                config::ENV_AUTO_RESUME
            );
        }
        let mut timer = tokio::time::interval(self.cfg.interval);
        // Publish an initial snapshot before the first sleep so /metrics is
        // populated immediately on startup rather than after one full interval.
        *handle.write().await = self.snapshot().await;
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
                    *handle.write().await = self.snapshot().await;
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
#[cfg(feature = "daemon")]
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
