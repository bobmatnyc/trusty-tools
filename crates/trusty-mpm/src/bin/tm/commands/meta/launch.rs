//! Real Claude Code session launch + session-exit poll for the metaharness
//! (#1049 WI-A, #1051 WI-B).
//!
//! Why: the re-scoped POC drives a REAL `claude` CLI session through trusty-mpm's
//! existing machinery rather than an in-process trusty-code orchestrator. This
//! module owns the launch ritual — `prepare_session` (deploy agents/skills/
//! CLAUDE.md/PM-prompt/MCP) → `SessionManager::create_with_id` (tmux host rooted
//! at the project dir) → `ClaudeCodeAdapter::spawn` (launch `claude`) → send the
//! task into the pane — and the time-bounded poll that waits for the session to
//! exit. The poll's *decision* (continue vs. time-out) is a pure function so it
//! is unit-testable WITHOUT a live `claude` or tmux; the live launch is
//! integration-level and gated `#[ignore]` (#1053 follow-up).
//! What: [`PollDecision`] + [`poll_decision`] are the pure deadline logic;
//! [`LaunchOutcome`] reports how a launch ended; [`launch_and_wait`] performs the
//! full live sequence (provision-skipped local dir) and returns the outcome plus
//! the final pane transcript for diagnostics; [`new_session_manager`] builds a
//! standalone, daemon-free [`SessionManager`].
//! Test: `launch::tests` cover [`poll_decision`] exhaustively; the live
//! `launch_and_wait` path is exercised only by the `#[ignore]` end-to-end test.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use tracing::{info, warn};

use trusty_mpm::core::paths::FrameworkPaths;
use trusty_mpm::core::session_launch::prepare_session;
use trusty_mpm::runtime::{RuntimeKind, build_adapter};
use trusty_mpm::session_manager::real_tmux::RealTmuxDriver;
use trusty_mpm::session_manager::{ManagedSessionId, ManagedTmuxDriver, SessionManager};

/// Interval between session-exit liveness probes during the poll loop.
///
/// Why: a fixed, modest cadence keeps the poll responsive without busy-spinning
/// on `tmux has-session`. Centralising it keeps the live loop and the (future)
/// tuning in one place.
/// What: 2 seconds between `session_exists` probes.
/// Test: side-effect timing; the decision logic is tested via [`poll_decision`].
pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How many trailing pane lines to capture for the diagnostic transcript.
///
/// Why: on failure/timeout the operator needs enough scrollback to see what the
/// session was doing, but not the entire unbounded history.
/// What: the last 400 lines of the tmux pane.
/// Test: side-effect-only (capture is exercised by the live test).
pub(crate) const TRANSCRIPT_LINES: usize = 400;

/// The pure decision a single poll tick makes given the elapsed time.
///
/// Why: separating the *decision* (keep waiting vs. give up) from the *I/O*
/// (probe tmux, sleep) makes the time-bound logic deterministic and unit-testable
/// without a clock or a live session.
/// What: `Continue` (deadline not yet reached — probe again) or `TimedOut` (the
/// elapsed time met/exceeded the budget).
/// Test: `poll_decision_*` arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PollDecision {
    /// The timeout has not elapsed; the caller should probe again.
    Continue,
    /// The timeout budget has been reached or exceeded; stop waiting.
    TimedOut,
}

/// Decide whether a poll loop should continue or has timed out.
///
/// Why: the live loop must stop after a bounded wall-clock budget so a stuck
/// session never hangs the command forever (#1051). Expressing the rule as a
/// pure `(elapsed, timeout) -> decision` function lets us assert the boundary
/// behaviour exactly, with no sleeping in tests.
/// What: returns [`PollDecision::TimedOut`] when `elapsed >= timeout`, else
/// [`PollDecision::Continue`]. The boundary is inclusive so a zero-budget poll
/// times out immediately rather than probing once.
/// Test: `poll_decision_continues_before_deadline`,
/// `poll_decision_times_out_at_deadline`, `poll_decision_times_out_after_deadline`,
/// `poll_decision_zero_timeout_times_out_immediately`.
pub(crate) fn poll_decision(elapsed: Duration, timeout: Duration) -> PollDecision {
    if elapsed >= timeout {
        PollDecision::TimedOut
    } else {
        PollDecision::Continue
    }
}

/// How a [`launch_and_wait`] run ended.
///
/// Why: the caller maps the launch result to a verification step and a process
/// exit code; an explicit enum makes the exited-vs-timed-out distinction clear.
/// What: `Exited` (the tmux session disappeared within the budget — `claude`
/// finished) or `TimedOut` (the budget elapsed while the session was still
/// alive). Either way the caller still runs artifact verification.
/// Test: the variants are produced by the `#[ignore]` live test; the boundary
/// that selects them is tested via [`poll_decision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchOutcome {
    /// The session exited on its own within the timeout budget.
    Exited,
    /// The timeout budget elapsed while the session was still running.
    TimedOut,
}

impl LaunchOutcome {
    /// A stable status token for the structured report.
    ///
    /// Why: the JSON report needs a machine-readable token independent of the
    /// human prose.
    /// What: `"exited"` or `"timed-out"`.
    /// Test: `launch_outcome_status_tokens`.
    pub(crate) fn status(&self) -> &'static str {
        match self {
            LaunchOutcome::Exited => "exited",
            LaunchOutcome::TimedOut => "timed-out",
        }
    }
}

/// Result of a live launch: how it ended plus the final pane transcript.
///
/// Why: the caller needs both the outcome (to decide exit code) and the captured
/// transcript (to fold into the failure report, #1052 observability scope).
/// What: bundles the [`LaunchOutcome`], the tmux session name, and the captured
/// pane transcript (possibly empty if capture failed).
/// Test: produced only by the live `#[ignore]` end-to-end test.
#[derive(Debug)]
pub(crate) struct LaunchReport {
    /// How the launched session ended.
    pub(crate) outcome: LaunchOutcome,
    /// The tmux session name the harness created.
    pub(crate) tmux_name: String,
    /// The captured pane transcript (last [`TRANSCRIPT_LINES`] lines).
    pub(crate) transcript: String,
}

/// Build a standalone, daemon-free [`SessionManager`] backed by real tmux.
///
/// Why: the POC must run WITHOUT the trusty-mpm daemon, but still reuse the
/// daemon's session bookkeeping. Constructing a `SessionManager` directly over a
/// throwaway state dir gives the same `create_with_id` / `observe` machinery the
/// daemon uses, with no HTTP server in the loop.
/// What: discovers a [`RealTmuxDriver`] (erroring with an actionable message when
/// `tmux` is absent) and loads a `SessionManager` rooted at `state_dir`.
/// Test: side-effect/IO-heavy (requires `tmux`); covered by the live test.
pub(crate) async fn new_session_manager(state_dir: &Path) -> Result<SessionManager> {
    let tmux = RealTmuxDriver::discover()
        .context("tmux not found — the metaharness POC requires tmux to host the claude session")?;
    let tmux: Arc<dyn ManagedTmuxDriver> = Arc::new(tmux);
    SessionManager::new(state_dir, tmux)
        .await
        .context("failed to initialise the standalone session manager")
}

/// Launch a real `claude` session for `project_dir` and wait for it to exit.
///
/// Why: this is the WI-A launch path (#1049) plus the WI-B poll (#1051), wired
/// in-process with no daemon. It reuses the EXACT machinery a daemon launch uses
/// ([`prepare_session`], [`SessionManager::create_with_id`], [`build_adapter`] →
/// `ClaudeCodeAdapter::spawn`) so the POC exercises the production path, not a
/// parallel one.
/// What: in order — (1) `prepare_session` deploys agents/skills/CLAUDE.md/PM
/// prompt/MCP into `project_dir` (a prep failure is non-fatal: logged, the launch
/// proceeds); (2) creates a tmux session rooted at `project_dir` via
/// `create_with_id` (cwd = project, so NO git clone happens — the `--no-provision`
/// behaviour); (3) builds the Claude Code adapter and `spawn`s `claude` in the
/// pane; (4) when `task` is `Some`, sends it into the pane (after a short settle
/// so `claude` is ready to receive it) — this is how the demo task reaches the
/// session, mirroring `SessionManager::send_input`; (5) polls `session_exists`
/// every [`POLL_INTERVAL`] until the session exits or `timeout` elapses; (6)
/// captures the final pane transcript and decommissions the session. Returns a
/// [`LaunchReport`].
/// Test: integration-level (live `claude` + tmux); the `#[ignore]` end-to-end
/// test in `tests/` drives it. The pure poll boundary is covered by
/// [`poll_decision`] tests.
pub(crate) async fn launch_and_wait(
    mgr: &SessionManager,
    project_dir: &Path,
    task: Option<&str>,
    timeout: Duration,
) -> Result<LaunchReport> {
    // (1) Deploy the custom instructions. Non-fatal: a missing framework only
    // means the session launches without trusty-mpm agents/skills.
    let fw = FrameworkPaths::default();
    match prepare_session(&fw, project_dir) {
        Ok(report) => info!(
            agents = report.deploy.deployed.len(),
            project = %project_dir.display(),
            "meta run: prepared session (agents/skills/CLAUDE.md/MCP deployed)"
        ),
        Err(e) => warn!(
            project = %project_dir.display(),
            "meta run: session preparation failed (continuing): {e}"
        ),
    }

    // (2) Create the tmux host rooted at the local project dir. Passing
    // `cwd = Some(project_dir)` and NO workspace/repo means the manager creates
    // the pane directly in the existing directory — no provisioner, no git clone.
    let id = ManagedSessionId::new();
    let task_label = task.unwrap_or("metaharness session").to_string();
    let record = mgr
        .create_with_id(
            id,
            task_label,
            Some(project_dir.to_path_buf()),
            None,
            None,
            None,
            None,
            RuntimeKind::ClaudeCode,
            false,
            false, // not SM-owned: meta-launch uses an existing local dir (#1511)
        )
        .await
        .context("failed to create the tmux session for the metaharness")?;
    info!(
        id = %record.id,
        tmux = %record.tmux_name,
        "meta run: tmux session created (no provision — local dir used in place)"
    );

    // (3) Launch `claude` in the pane via the SAME adapter the daemon uses.
    // `gh_env`: empty — this CLI-driven metaharness path runs outside the
    // daemon's `DaemonState`/`ProjectRegistry`, so #3025's spawn-env
    // injection (which needs the registry) is out of scope here, matching
    // the existing scope boundary around the bare-`tm` in-place relaunch
    // path (`build_inplace_resume_command`).
    let adapter = build_adapter(RuntimeKind::ClaudeCode, mgr.tmux_driver());
    adapter
        .spawn(
            &record.tmux_name,
            project_dir,
            &record.task,
            &record.id.to_string(),
            &[],
        )
        .context("failed to spawn the claude runtime in the tmux pane")?;
    info!(tmux = %record.tmux_name, "meta run: claude runtime spawned");

    // (4) Deliver the task to the session. `claude` reads the PM prompt from the
    // `--append-system-prompt-file` set by `prepare_session`; the concrete task
    // is typed into the pane through the shared readiness-gated injection seam
    // (`SessionManager::inject_task_when_ready`, #1903/#1299) — which polls for
    // the runtime to be ready instead of the blind fixed sleep this prototype
    // used before that seam existed.
    if let Some(task) = task {
        match mgr.inject_task_when_ready(&record.id, task).await {
            Ok(true) => info!(tmux = %record.tmux_name, "meta run: demo task delivered to session"),
            Ok(false) => {
                warn!(tmux = %record.tmux_name, "meta run: runtime never became ready; demo task not delivered")
            }
            Err(e) => warn!(tmux = %record.tmux_name, "meta run: failed to inject demo task: {e}"),
        }
    }

    // (5) Poll for the session to exit, time-bounded.
    let start = Instant::now();
    let outcome = loop {
        let alive = mgr.observe(&record.id, 1).await.map(|o| o.runtime_active);
        match alive {
            Ok(false) => break LaunchOutcome::Exited,
            Ok(true) => {}
            Err(e) => warn!(tmux = %record.tmux_name, "meta run: liveness probe failed: {e}"),
        }
        if poll_decision(start.elapsed(), timeout) == PollDecision::TimedOut {
            break LaunchOutcome::TimedOut;
        }
        // TODO(M1 POC hardening follow-up): the effective timeout can overshoot
        // the requested budget by up to one POLL_INTERVAL, since we sleep a full
        // interval before re-checking the deadline. Acceptable for the POC; a
        // tighter deadline-aware sleep is a tracked follow-up (issue number to be
        // attached separately).
        tokio::time::sleep(POLL_INTERVAL).await;
    };
    info!(
        tmux = %record.tmux_name,
        outcome = outcome.status(),
        elapsed_secs = start.elapsed().as_secs(),
        "meta run: poll loop finished"
    );

    // (6) Capture the final transcript BEFORE teardown, then decommission so the
    // POC leaves no stray tmux session behind.
    let transcript = mgr
        .capture_pane(&record.id, TRANSCRIPT_LINES)
        .await
        .unwrap_or_default();
    if let Err(e) = mgr.decommission(&record.id, None).await {
        warn!(tmux = %record.tmux_name, "meta run: decommission failed (best-effort): {e}");
    }

    Ok(LaunchReport {
        outcome,
        tmux_name: record.tmux_name,
        transcript,
    })
}

/// Resolve the throwaway state directory for the standalone session store.
///
/// Why: the daemon-free `SessionManager` needs a place to persist its record
/// store; rooting it under the project keeps a run self-contained and avoids
/// colliding with a real daemon's store.
/// What: returns `<project_dir>/.trusty-mpm/meta-sessions`.
/// Test: `state_dir_is_project_scoped`.
pub(crate) fn state_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".trusty-mpm").join("meta-sessions")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_decision_continues_before_deadline() {
        assert_eq!(
            poll_decision(Duration::from_secs(5), Duration::from_secs(10)),
            PollDecision::Continue
        );
    }

    #[test]
    fn poll_decision_times_out_at_deadline() {
        // The boundary is inclusive: elapsed == timeout is a time-out.
        assert_eq!(
            poll_decision(Duration::from_secs(10), Duration::from_secs(10)),
            PollDecision::TimedOut
        );
    }

    #[test]
    fn poll_decision_times_out_after_deadline() {
        assert_eq!(
            poll_decision(Duration::from_secs(11), Duration::from_secs(10)),
            PollDecision::TimedOut
        );
    }

    #[test]
    fn poll_decision_zero_timeout_times_out_immediately() {
        assert_eq!(
            poll_decision(Duration::from_secs(0), Duration::from_secs(0)),
            PollDecision::TimedOut
        );
    }

    #[test]
    fn launch_outcome_status_tokens() {
        assert_eq!(LaunchOutcome::Exited.status(), "exited");
        assert_eq!(LaunchOutcome::TimedOut.status(), "timed-out");
    }

    #[test]
    fn state_dir_is_project_scoped() {
        let dir = state_dir(Path::new("/work/proj"));
        assert_eq!(
            dir,
            Path::new("/work/proj/.trusty-mpm/meta-sessions"),
            "state dir must be project-scoped"
        );
    }
}
