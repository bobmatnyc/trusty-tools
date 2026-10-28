//! The daemon's auto-resume loop and its parked-sessions notice (#10356).
//!
//! Why: owner ruling D74 (option a) makes the daemon the one process that
//! auto-resumes `Stopped` sessions. Only the daemon installs the runtime
//! relauncher (`managed_routes::auto_relaunch`), so only the daemon can put a
//! runtime in a recreated pane. The standalone `tm supervisor` built its own
//! manager with no relauncher, and after the 2026-10-27 reboot its resumes
//! marked 15 records `Active` over panes with nothing in them. Owner ruling D73
//! adds the boot grace (in `session_manager::resume_breaker`) and one notice,
//! after the grace window, that names every session the breaker still holds.
//! What: [`AutoResumeLoop`] re-reads the console toggle each sweep
//! ([`AutoResumeGate`]), runs [`resume_sweep`] on the blocking pool under the
//! sweep bound ([`DetachedSweep`]), and emits the parked notice once per boot.
//! The first sweep runs at daemon start, so a cold start gets a launch attempt
//! for every eligible session without waiting an interval.
//! Test: `auto_resume_loop_tests.rs`.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::core::auto_resume::{self, AutoResumeGate};
use crate::core::paths::FrameworkPaths;
use crate::daemon::DaemonState;
use crate::session_manager::{SessionManager, SessionRecord};
use crate::supervisor::watchdog::DetachedSweep;
use crate::supervisor::{SupervisorConfig, TickReport, resume_sweep};

/// What one loop iteration did.
///
/// Test: asserted by every test in `auto_resume_loop_tests.rs`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LoopTick {
    /// The resume sweep's report; empty when auto-resume is off.
    pub report: TickReport,
    /// The parked-sessions notice, on the one tick that emitted it.
    pub park_notice: Option<String>,
}

/// The daemon's auto-resume driver.
///
/// Why: see the module doc.
/// What: owns the manager handle, the sweep config, the console gate, the
/// sweep bound and the once-per-boot notice latch.
/// Test: `auto_resume_loop_tests.rs`.
pub struct AutoResumeLoop {
    mgr: Arc<SessionManager>,
    cfg: SupervisorConfig,
    gate: AutoResumeGate,
    sweep: DetachedSweep<TickReport>,
    notice_done: bool,
}

impl AutoResumeLoop {
    /// A loop over `mgr` with `cfg` (its `auto_resume` is the boot flag) and
    /// the console's desired-state file at `desired_path`.
    pub fn new(
        mgr: Arc<SessionManager>,
        cfg: SupervisorConfig,
        desired_path: impl Into<std::path::PathBuf>,
    ) -> Self {
        let gate = AutoResumeGate::new(cfg.auto_resume, desired_path);
        Self {
            mgr,
            cfg,
            gate,
            sweep: DetachedSweep::new("auto-resume sweep"),
            notice_done: false,
        }
    }

    /// Run one iteration: resume eligible sessions, then the notice check.
    ///
    /// Why: one atomic step that the timer loop and the tests both drive.
    /// What: resolves the console gate (#5208). When on, runs [`resume_sweep`]
    /// under `cfg.tick_timeout`; an abandoned sweep yields an empty report and
    /// the next tick retries. Then [`Self::park_notice_once`].
    /// Test: `the_daemon_loop_resumes_a_cold_start_fleet`,
    /// `a_relaunch_failure_on_the_daemon_path_is_recorded`,
    /// `the_console_toggle_enables_a_running_daemon_sweep`.
    pub async fn tick(&mut self) -> LoopTick {
        let on = self.gate.resolve();
        let report = if on {
            let cfg = SupervisorConfig {
                auto_resume: true,
                ..self.cfg.clone()
            };
            let mgr = Arc::clone(&self.mgr);
            let sweep = move || async move { resume_sweep(&mgr, &cfg).await };
            self.sweep
                .run(self.cfg.tick_timeout, sweep)
                .await
                .unwrap_or_default()
        } else {
            TickReport::default()
        };
        let park_notice = self.park_notice_once().await;
        LoopTick {
            report,
            park_notice,
        }
    }

    /// Emit the parked-sessions notice once, after the boot grace ends.
    ///
    /// Why (owner ruling D73, item C): a boot that leaves sessions parked must
    /// say so ONCE, naming all of them, rather than one log line per session
    /// scattered through the reap ticks. The surface is the daemon log at
    /// `warn`, target `trusty_mpm::resume_park` — the same surface every
    /// existing park line uses (#6568), and the one #2621 made the greppable
    /// park signal.
    /// What: returns `None` while the manager is inside its boot grace and
    /// after the notice was decided. The first call after the window decides:
    /// it lists every record with a park reason, logs the notice when there is
    /// at least one, and latches either way, so a boot has at most one notice.
    /// Test: `the_park_notice_waits_for_the_grace_window_and_fires_once`,
    /// `no_park_notice_when_nothing_is_parked`.
    async fn park_notice_once(&mut self) -> Option<String> {
        if self.notice_done || self.mgr.in_resume_boot_grace().await {
            return None;
        }
        self.notice_done = true;
        let records = self.mgr.list().await;
        let notice = park_notice(&records)?;
        warn!(target: "trusty_mpm::resume_park", "{notice}");
        Some(notice)
    }

    /// Run until `cancel` fires, one tick per `cfg.interval`.
    ///
    /// What: the first tick runs immediately, so a cold start resumes at boot.
    /// Test: the per-tick behaviour is tested through [`Self::tick`].
    pub async fn run_until(mut self, cancel: CancellationToken) {
        info!(
            interval_secs = self.cfg.interval.as_secs(),
            boot_flag = self.cfg.auto_resume,
            desired_path = %self.gate.path().display(),
            "auto-resume: daemon loop starting (#10356)"
        );
        let mut timer = tokio::time::interval(self.cfg.interval);
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    info!("auto-resume: cancel signal received; exiting");
                    return;
                }
                _ = timer.tick() => {
                    self.tick().await;
                }
            }
        }
    }
}

/// The one-line notice naming every parked session, or `None` when none is.
///
/// What: lists `<tmux name> (<id>)` for each record whose
/// `auto_resume_park_reason` is set, comma-separated, with the count and the
/// command that clears a park.
/// Test: `the_park_notice_waits_for_the_grace_window_and_fires_once`.
pub fn park_notice(records: &[SessionRecord]) -> Option<String> {
    let parked: Vec<String> = records
        .iter()
        .filter(|r| r.auto_resume_park_reason().is_some())
        .map(|r| format!("{} ({})", r.tmux_name, r.id))
        .collect();
    if parked.is_empty() {
        return None;
    }
    Some(format!(
        "auto-resume: {} session(s) still parked after the boot grace window (#6568, \
         #10356): {}. Resume each by hand once the cause is fixed: `tm session resume <id>`",
        parked.len(),
        parked.join(", ")
    ))
}

/// Start the daemon's auto-resume loop.
///
/// Why: #10356 (owner ruling D74). Call it after `install_auto_relauncher`,
/// so the first sweep already has the relauncher.
/// What: boot flag and cadence from [`SupervisorConfig::from_env`]
/// (`TRUSTY_MPM_AUTO_RESUME`, `TRUSTY_MPM_SUPERVISOR_INTERVAL`), the console
/// file from [`auto_resume::desired_path`], spawned on its own task.
/// Test: the loop itself is tested through [`AutoResumeLoop::tick`].
pub async fn spawn(state: &Arc<DaemonState>, cancel: CancellationToken) {
    let mgr = state.session_manager().await;
    let desired = auto_resume::desired_path(&FrameworkPaths::default());
    let driver = AutoResumeLoop::new(mgr, SupervisorConfig::from_env(), desired);
    tokio::spawn(driver.run_until(cancel));
}

#[cfg(test)]
#[path = "auto_resume_loop_tests.rs"]
mod tests;
