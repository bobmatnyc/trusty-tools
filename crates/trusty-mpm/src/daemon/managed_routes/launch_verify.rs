//! Did the relaunch actually take? A bounded post-send status check (#6766).
//!
//! Why: every managed spawn path hands its command to `tmux send-keys` and
//! returns. `send_line`/`send_line_to_pane` report whether TMUX accepted the
//! keystrokes — nothing about what the pane's shell then did with them. So
//! `resume_managed` logged "managed session resumed and runtime respawned" for
//! a pane where `claude` printed a refusal and exited before it drew a frame
//! (#6765's transcript), and the record stayed `Active` with no runtime behind
//! it until the periodic runtime-exit reaper noticed, ~60 s later. The pane-side
//! half of #6766 (`runtime::claude_code_exit_hint`) makes the PANE say what
//! happened; this is the half that makes `tm` know.
//!
//! What: [`verify_launch`] polls [`ManagedTmuxDriver::runtime_ready`] — the same
//! signal `SessionManager::inject_task_when_ready` gates task delivery on, which
//! `RealTmuxDriver` implements as the `claude`-PID probe — until it reports the
//! runtime is up or the budget runs out. The caller turns [`LaunchOutcome::NotStarted`]
//! into the `mark_errored` transition `resume_managed` already performs when the
//! adapter itself fails, so this adds a new DETECTION, not a new state.
//!
//! **Three outcomes, not two.** `runtime_ready` is `false` both for "the runtime
//! is not there" and for "this driver has no signal to offer" — `FakeNoopTmuxDriver`
//! (the documented tmux-is-absent fallback) and every hermetic test built on
//! `DaemonState::with_root_isolated_managed` report `false` by construction.
//! Reading that as a verdict is the exact false-positive that got a `runtime_ready`
//! gate on `send_input` reverted (see `session_manager::task_inject::PaneReadiness`).
//! So the check asks `session_exists` FIRST: a driver that cannot even see the
//! tmux session answers [`LaunchOutcome::Unverifiable`] immediately, pays no
//! polling delay, and leaves the caller's behaviour exactly as it was.
//!
//! **Why polling and not a re-send.** #6766 also proposes re-sending the command
//! with the resume flag dropped when no PID appears. That is deliberately not
//! here: a poll window that expires while `claude` is still starting would put a
//! SECOND `claude` in the same pane, two agents against one transcript and one
//! `TM_MANAGED_SESSION_ID`. Reporting the failure is safe at any budget;
//! recovering from it is not.
//!
//! Test: `verify_launch_reports_running_when_the_runtime_comes_up`,
//! `verify_launch_reports_not_started_when_the_pane_stays_bare`,
//! `verify_launch_is_unverifiable_without_an_observable_session`,
//! `verify_launch_does_not_wait_when_it_cannot_observe` in this file's `tests`
//! module.

use std::time::Duration;

use crate::session_manager::ManagedTmuxDriver;

/// Attempts [`verify_launch`] makes before declaring the runtime absent.
///
/// Why: `claude` takes 1-3 s to appear after `send-keys` (the figure
/// `daemon::services::session_service::spawn_pid_capture` was written to), so
/// the budget has to clear that with margin — a premature verdict would mark a
/// perfectly good session `Errored`. Paired with [`RESUME_VERIFY_INTERVAL`] this
/// is a 4.5 s ceiling, and the check returns the instant the runtime appears, so
/// a healthy resume typically pays 1-2 s.
pub(crate) const RESUME_VERIFY_ATTEMPTS: u32 = 10;

/// Delay between [`verify_launch`] probes. See [`RESUME_VERIFY_ATTEMPTS`].
pub(crate) const RESUME_VERIFY_INTERVAL: Duration = Duration::from_millis(500);

/// What a post-send [`verify_launch`] concluded about the pane.
///
/// Why: the caller must be able to tell "the runtime is provably not there"
/// apart from "nothing here can tell" — the second must never drive a state
/// transition. See this module's header for the drivers that produce it.
/// What: three variants; only [`Self::NotStarted`] is a failure verdict.
/// Test: one test per variant in this file's `tests` module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchOutcome {
    /// The runtime came up in the pane within the budget.
    Running,
    /// The tmux session is observably alive and the runtime never appeared —
    /// the pane is sitting at a bare shell.
    NotStarted,
    /// This driver offers no usable signal; the caller learns nothing.
    Unverifiable,
}

/// Poll `tmux_name`'s pane until its runtime is up, or the budget expires.
///
/// Why: see this module's header — `send-keys` succeeding is not the runtime
/// starting, and until #6766 nothing between the send and the periodic reaper
/// ever checked.
/// What: returns [`LaunchOutcome::Unverifiable`] immediately when the driver
/// cannot observe the tmux session at all (no delay, no verdict); otherwise
/// probes [`ManagedTmuxDriver::runtime_ready`] up to `attempts` times, sleeping
/// `interval` between probes, and returns [`LaunchOutcome::Running`] on the
/// first success or [`LaunchOutcome::NotStarted`] when the budget is spent.
/// Sleeps with `tokio::time::sleep`, never `std::thread::sleep`, so it never
/// parks a Tokio worker — the same discipline `inject_task_when_ready`'s
/// readiness loop follows.
/// Test: the four `verify_launch_*` tests below drive every branch with fake
/// drivers; `attempts`/`interval` are parameters precisely so those tests do not
/// spend the production budget.
pub(crate) async fn verify_launch(
    tmux: &dyn ManagedTmuxDriver,
    tmux_name: &str,
    attempts: u32,
    interval: Duration,
) -> LaunchOutcome {
    // Ask observability BEFORE spending any of the budget: a driver that cannot
    // see the session can only ever answer `false`, and 4.5 s of polling to
    // learn nothing would tax every hermetic test on this path.
    if !tmux.session_exists(tmux_name) {
        return LaunchOutcome::Unverifiable;
    }
    for attempt in 0..attempts.max(1) {
        if attempt > 0 {
            tokio::time::sleep(interval).await;
        }
        if tmux.runtime_ready(tmux_name) {
            return LaunchOutcome::Running;
        }
    }
    LaunchOutcome::NotStarted
}

/// Verify a just-sent resume and record what it found (#6766).
///
/// Why: `resume_managed`'s success arm used to log "managed session resumed and
/// runtime respawned" on the strength of `send-keys` alone. This is the seam
/// that makes that claim conditional, and it lives here rather than inline so
/// `lifecycle.rs` stays inside its frozen SLOC budget.
/// What: runs [`verify_launch`] with the production budget. On
/// [`LaunchOutcome::NotStarted`] it warns and drives the SAME
/// `SessionManager::mark_errored` transition the adapter-failure arm above the
/// call site already uses — deliberately not a NEW state, and deliberately not a
/// re-send (see this module's header). Any other outcome logs the resume as
/// before. The `mark_errored` result is dropped for the same reason the
/// adapter-failure arm drops it: the resume response still has to be built from
/// whatever the store now holds.
/// Test: the failure verdict itself is covered by
/// `verify_launch_reports_not_started_when_the_pane_stays_bare`; the hermetic
/// resume tests in `tests/session_manager_mvp.rs` cover this wrapper's
/// [`LaunchOutcome::Unverifiable`] pass-through (their fake driver cannot
/// observe a tmux session, so the resume path must behave exactly as before).
pub(crate) async fn record_resume_outcome(
    mgr: &crate::session_manager::SessionManager,
    tmux: &dyn ManagedTmuxDriver,
    record: &crate::session_manager::SessionRecord,
    workspace: &std::path::Path,
) {
    let outcome = verify_launch(
        tmux,
        &record.tmux_name,
        RESUME_VERIFY_ATTEMPTS,
        RESUME_VERIFY_INTERVAL,
    )
    .await;
    if outcome == LaunchOutcome::NotStarted {
        let msg = format!(
            "resume relaunch did not take: no runtime came up in pane '{}' — the pane \
             printed why on its own last line; resume again once that is addressed (#6766)",
            record.tmux_name
        );
        tracing::warn!(
            id = %record.id,
            name = %record.tmux_name,
            workspace = %workspace.display(),
            "{msg}"
        );
        let _ = mgr.mark_errored(&record.id, &msg).await;
        return;
    }
    tracing::info!(
        id = %record.id,
        name = %record.tmux_name,
        workspace = %workspace.display(),
        ?outcome,
        "managed session resumed and runtime respawned"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_manager::ManagedError;

    /// A driver with independently settable `session_exists` / `runtime_ready`
    /// answers — the only two signals [`verify_launch`] reads.
    struct ProbeDriver {
        session_live: bool,
        runtime_up: bool,
    }

    impl ManagedTmuxDriver for ProbeDriver {
        fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
            Ok(String::new())
        }
        fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
            Ok(if self.session_live {
                vec!["tmpm-probe".to_string()]
            } else {
                Vec::new()
            })
        }
        fn runtime_ready(&self, _name: &str) -> bool {
            self.runtime_up
        }
    }

    /// A short budget so the failure path does not spend the production ceiling.
    const TEST_INTERVAL: Duration = Duration::from_millis(1);

    #[tokio::test]
    async fn verify_launch_reports_running_when_the_runtime_comes_up() {
        let driver = ProbeDriver {
            session_live: true,
            runtime_up: true,
        };
        assert_eq!(
            verify_launch(&driver, "tmpm-probe", 3, TEST_INTERVAL).await,
            LaunchOutcome::Running
        );
    }

    #[tokio::test]
    async fn verify_launch_reports_not_started_when_the_pane_stays_bare() {
        // #6766: the tmux session is alive and the runtime never appeared —
        // the refused-relaunch shape. This is the ONLY failure verdict.
        let driver = ProbeDriver {
            session_live: true,
            runtime_up: false,
        };
        assert_eq!(
            verify_launch(&driver, "tmpm-probe", 3, TEST_INTERVAL).await,
            LaunchOutcome::NotStarted
        );
    }

    #[tokio::test]
    async fn verify_launch_is_unverifiable_without_an_observable_session() {
        // A driver that cannot see the session (tmux absent, hermetic fake)
        // must not be read as proof the runtime is missing — that is the
        // false positive that got a `runtime_ready` gate on `send_input`
        // reverted; see this module's header.
        let driver = ProbeDriver {
            session_live: false,
            runtime_up: false,
        };
        assert_eq!(
            verify_launch(&driver, "tmpm-probe", 3, TEST_INTERVAL).await,
            LaunchOutcome::Unverifiable
        );
    }

    #[tokio::test]
    async fn verify_launch_does_not_wait_when_it_cannot_observe() {
        // The observability check must short-circuit BEFORE the poll loop —
        // otherwise every hermetic resume test pays the full budget.
        let driver = ProbeDriver {
            session_live: false,
            runtime_up: false,
        };
        let started = std::time::Instant::now();
        let outcome = verify_launch(&driver, "tmpm-probe", 20, Duration::from_millis(200)).await;
        assert_eq!(outcome, LaunchOutcome::Unverifiable);
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "an unobservable driver must cost no polling delay, took {:?}",
            started.elapsed()
        );
    }
}
