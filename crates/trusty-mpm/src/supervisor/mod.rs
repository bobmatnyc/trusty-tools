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

    /// Run the supervisor loop forever, publishing snapshots after each sweep.
    ///
    /// Why: this is the unattended heartbeat — it keeps the fleet moving (auto-
    /// resume), keeps observing (classification), and keeps the `/metrics`
    /// snapshot fresh, with no live caller, until the process is signalled to stop.
    /// What: on an interval timer (`cfg.interval`), runs [`Self::tick`], then
    /// writes [`Self::snapshot`] into the shared `handle` so HTTP readers see the
    /// latest state. The loop never returns under normal operation; a fleet sweep
    /// that errors internally is already degraded-handled inside `run_tick`.
    /// Test: the per-iteration behavior is tested via `tick` + `snapshot`; the
    /// timer wrapper itself is a thin shell exercised at runtime.
    #[cfg(feature = "daemon")]
    pub async fn run(mut self, handle: http::MetricsHandle) -> anyhow::Result<()> {
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
        loop {
            timer.tick().await;
            self.tick().await;
            *handle.write().await = self.snapshot().await;
        }
    }
}
