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

/// Attempts [`detect_unconsumed_launch`] makes before declaring the pane stuck.
///
/// Why: the sentinel is written by the shim the pane's shell runs, so it appears
/// as soon as the shell parses the line — milliseconds, not the 1-3 s `claude`
/// itself needs. The budget only has to cover a shell still finishing its init
/// hooks (`direnv` prints immediately before the cut in this issue's captures).
/// Paired with [`LAUNCH_CONSUMED_INTERVAL`] this is a 2 s ceiling, and it returns
/// the instant the sentinel appears.
pub(crate) const LAUNCH_CONSUMED_ATTEMPTS: u32 = 10;

/// Delay between [`detect_unconsumed_launch`] probes. See
/// [`LAUNCH_CONSUMED_ATTEMPTS`].
pub(crate) const LAUNCH_CONSUMED_INTERVAL: Duration = Duration::from_millis(200);

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

/// Did the pane's SHELL run the launch line? (#8233 review, defence in depth.)
///
/// Why: `tmux send-keys` reports that tmux accepted the keystrokes. It says
/// nothing about what the shell did with them, and the failure this issue tracks
/// is precisely a shell that took the bytes and never executed them — the pane
/// sits at a PS2 continuation prompt and every later probe reads as "the runtime
/// is not up", which is true but tells the operator nothing and leads the daemon
/// to kill the pane and retry into the same state. The shim writes a sentinel
/// the moment it starts, so its absence is positive evidence the LINE, not the
/// launch, is what failed — available in ~2 s instead of at the end of the 4.5 s
/// runtime poll, and with an actionable message.
/// What: `None` — nothing to report — for a non-Claude-Code runtime (no shim, so
/// no sentinel), for a driver that cannot see the tmux session at all (the same
/// `Unverifiable` guard [`verify_launch`] opens with, which is what keeps every
/// hermetic test on this path unchanged), and when the spec root cannot be
/// resolved. Otherwise polls for `<root>/<session-id>.started`, deleting it and
/// returning `None` once it appears. On exhaustion it interrupts the pane
/// (best-effort — a wedged parser is exactly what `C-c` clears, so the next
/// attempt starts from a usable prompt) and returns the operator-facing message.
/// Test: `unconsumed_launch_is_reported_when_the_sentinel_never_appears`,
/// `a_consumed_launch_reports_nothing_and_clears_its_sentinel`,
/// `unconsumed_launch_is_not_reported_for_an_unobservable_session`,
/// `unconsumed_launch_is_not_reported_for_a_non_claude_runtime`.
pub(crate) async fn detect_unconsumed_launch(
    tmux: &dyn ManagedTmuxDriver,
    record: &crate::session_manager::SessionRecord,
    attempts: u32,
    interval: Duration,
) -> Option<String> {
    let dir = crate::runtime::launch_spec::LaunchSpec::root()?;
    detect_unconsumed_launch_in(tmux, record, &dir, attempts, interval).await
}

/// [`detect_unconsumed_launch`] against an explicit spec directory (the
/// hermetic seam).
///
/// Why: the production root is under the operator's real config home, and a
/// test must neither read nor write there.
/// What: the body [`detect_unconsumed_launch`] wraps; see its doc.
/// Test: the four `*_unconsumed_launch*` / `*_consumed_launch*` tests below.
pub(crate) async fn detect_unconsumed_launch_in(
    tmux: &dyn ManagedTmuxDriver,
    record: &crate::session_manager::SessionRecord,
    spec_dir: &std::path::Path,
    attempts: u32,
    interval: Duration,
) -> Option<String> {
    if record.runtime != crate::runtime::RuntimeKind::ClaudeCode {
        return None;
    }
    if !tmux.session_exists(&record.tmux_name) {
        return None;
    }
    let marker = crate::runtime::launch_spec::LaunchSpec::started_marker_in(
        spec_dir,
        &record.id.to_string(),
    );
    for attempt in 0..attempts.max(1) {
        if attempt > 0 {
            tokio::time::sleep(interval).await;
        }
        if marker.exists() {
            let _ = std::fs::remove_file(&marker);
            return None;
        }
    }
    // A parser holding an unterminated construct swallows whatever is typed next
    // as more of it, so leaving the pane wedged would cost the next attempt too.
    let interrupted = match record.pane_id.as_deref() {
        Some(pane) => tmux.send_interrupt_to_pane(&record.tmux_name, pane),
        None => tmux.send_interrupt(&record.tmux_name),
    };
    if let Err(e) = interrupted {
        tracing::debug!(
            name = %record.tmux_name,
            "could not interrupt the stuck pane (non-fatal, #8233): {e}"
        );
    }
    Some(format!(
        "launch line was never run by the pane shell in '{}': tmux accepted the keystrokes \
         but no launch reached `tm internal-spawn-disclaimed`, which means the shell was not \
         at a usable prompt — typically an unterminated construct left over from an earlier \
         command, so it swallowed this line as a continuation. The pane has been interrupted; \
         nothing was started and no `claude` is running (#8233)",
        record.tmux_name
    ))
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
    // #8233: ask the cheaper, sharper question first — did the shell even run
    // the line? A `Some` here names the cause; the runtime poll below could only
    // ever report the symptom.
    if let Some(msg) = detect_unconsumed_launch(
        tmux,
        record,
        LAUNCH_CONSUMED_ATTEMPTS,
        LAUNCH_CONSUMED_INTERVAL,
    )
    .await
    {
        tracing::warn!(
            id = %record.id,
            name = %record.tmux_name,
            workspace = %workspace.display(),
            "{msg}"
        );
        let _ = mgr.mark_errored(&record.id, &msg).await;
        return;
    }
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

/// Verify a just-sent SPAWN and record what it found (#8233, mirroring #6766's
/// resume half).
///
/// Why: `spawn` returning `Ok` means tmux accepted the keystrokes, not that
/// `claude` started. Since #8233 the pane runs `tm internal-spawn-disclaimed
/// --launch-spec <file>`, and that shim can still fail AFTER the daemon is out
/// of the loop — a spec deleted or clobbered between write and read, a `claude`
/// that vanished from disk. The shim prints why, but only this check turns an
/// absent runtime into an errored RECORD; without it the session sat `Active`
/// with no runtime behind it until the ~60 s reaper noticed, which is exactly
/// how `dd0e2fb8-…` stayed ACTIVE with last activity `None`.
/// What: [`verify_launch`] with the production budget; on
/// [`LaunchOutcome::NotStarted`] it warns and drives the same
/// `SessionManager::mark_errored` transition the adapter-failure arm uses.
/// [`LaunchOutcome::Unverifiable`] — every hermetic test's driver, and the
/// documented tmux-absent fallback — changes nothing, exactly as on the resume
/// path.
/// Test: `verify_launch_reports_not_started_when_the_pane_stays_bare` covers the
/// verdict; `spawn_outcome_errors_a_record_whose_runtime_never_came_up` covers
/// this wrapper's transition.
pub(crate) async fn record_spawn_outcome(
    mgr: &crate::session_manager::SessionManager,
    tmux: &dyn ManagedTmuxDriver,
    record: &crate::session_manager::SessionRecord,
) {
    // #8233: same sentinel handshake as the resume path — see
    // `detect_unconsumed_launch`.
    if let Some(msg) = detect_unconsumed_launch(
        tmux,
        record,
        LAUNCH_CONSUMED_ATTEMPTS,
        LAUNCH_CONSUMED_INTERVAL,
    )
    .await
    {
        tracing::warn!(id = %record.id, name = %record.tmux_name, "{msg}");
        let _ = mgr.mark_errored(&record.id, &msg).await;
        return;
    }
    let outcome = verify_launch(
        tmux,
        &record.tmux_name,
        RESUME_VERIFY_ATTEMPTS,
        RESUME_VERIFY_INTERVAL,
    )
    .await;
    if outcome == LaunchOutcome::NotStarted {
        let msg = format!(
            "launch did not take: no runtime came up in pane '{}' — the pane printed why \
             on its own last line (a launch spec that could not be read prints there); \
             start again once that is addressed (#8233)",
            record.tmux_name
        );
        tracing::warn!(id = %record.id, name = %record.tmux_name, "{msg}");
        let _ = mgr.mark_errored(&record.id, &msg).await;
        return;
    }
    tracing::info!(
        id = %record.id,
        name = %record.tmux_name,
        ?outcome,
        "managed session spawned successfully"
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

    /// A [`ProbeDriver`] that also counts the `C-c` the stuck-pane arm sends.
    struct InterruptDriver {
        session_live: bool,
        interrupts: std::sync::atomic::AtomicUsize,
    }

    impl ManagedTmuxDriver for InterruptDriver {
        fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn send_interrupt(&self, _name: &str) -> Result<(), ManagedError> {
            self.interrupts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    }

    /// A record naming `tmpm-probe`, on the given runtime.
    #[rustfmt::skip]
    fn stub_record(runtime: crate::runtime::RuntimeKind) -> crate::session_manager::SessionRecord {
        crate::session_manager::SessionRecord {
            id: crate::session_manager::ManagedSessionId::new(),
            tmux_name: "tmpm-probe".to_owned(),
            cwd: std::path::PathBuf::from("/tmp/project"),
            task: "task".into(),
            state: crate::session_manager::ManagedSessionState::Active,
            created_at: chrono::Utc::now(),
            last_activity_at: None,
            workspace_path: None, repo_url: None,
            branch: None, pending_decision: None,
            proposed_default: None, correlation: Default::default(),
            runtime,
            ephemeral: false, workspace_owned: false,
            source_id: None,
            claude_session_id: None, scrollback_path: None,
            last_cwd: None, deliverable_id: None,
            pane_id: None, injection_status: Default::default(),
            worktree_owner: None,
            terminal_at: None,
            stop_cause: None,
        }
    }

    /// #8233 regression: the pane's shell never ran the launch line — the
    /// stuck-parser shape this issue is about. The check must SAY so, and must
    /// clear the wedged parser so the next attempt starts from a usable prompt.
    /// Without it the operator gets only "no runtime came up", which is the
    /// symptom of half a dozen unrelated failures.
    #[tokio::test]
    async fn unconsumed_launch_is_reported_when_the_sentinel_never_appears() {
        let dir = tempfile::tempdir().expect("tempdir");
        let driver = InterruptDriver {
            session_live: true,
            interrupts: std::sync::atomic::AtomicUsize::new(0),
        };
        let record = stub_record(crate::runtime::RuntimeKind::ClaudeCode);

        let msg = detect_unconsumed_launch_in(&driver, &record, dir.path(), 3, TEST_INTERVAL)
            .await
            .expect("an absent sentinel must be reported");

        assert!(msg.contains("never run by the pane shell"), "{msg}");
        assert!(msg.contains("#8233"), "{msg}");
        assert_eq!(
            driver.interrupts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the wedged parser must be interrupted, or the next launch is swallowed too"
        );
    }

    /// The happy path: the shim wrote its sentinel, so the shell demonstrably
    /// ran the line and this check must step aside. It also has to CONSUME the
    /// sentinel, or the next launch of this session would pass on a stale one.
    #[tokio::test]
    async fn a_consumed_launch_reports_nothing_and_clears_its_sentinel() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = stub_record(crate::runtime::RuntimeKind::ClaudeCode);
        let marker = crate::runtime::launch_spec::LaunchSpec::started_marker_in(
            dir.path(),
            &record.id.to_string(),
        );
        std::fs::write(&marker, b"").expect("plant the sentinel");
        let driver = InterruptDriver {
            session_live: true,
            interrupts: std::sync::atomic::AtomicUsize::new(0),
        };

        let verdict =
            detect_unconsumed_launch_in(&driver, &record, dir.path(), 3, TEST_INTERVAL).await;

        assert_eq!(verdict, None, "a started launch must report nothing");
        assert!(!marker.exists(), "the sentinel must be consumed");
        assert_eq!(
            driver.interrupts.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a healthy pane must never be interrupted"
        );
    }

    /// The same `Unverifiable` guard `verify_launch` opens with: a driver that
    /// cannot see the tmux session — every hermetic test, and the documented
    /// tmux-absent fallback — learns nothing and must change nothing.
    #[tokio::test]
    async fn unconsumed_launch_is_not_reported_for_an_unobservable_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let driver = InterruptDriver {
            session_live: false,
            interrupts: std::sync::atomic::AtomicUsize::new(0),
        };
        let record = stub_record(crate::runtime::RuntimeKind::ClaudeCode);
        assert_eq!(
            detect_unconsumed_launch_in(&driver, &record, dir.path(), 3, TEST_INTERVAL).await,
            None
        );
    }

    /// Only the Claude Code launch runs through `tm internal-spawn-disclaimed`,
    /// so only it writes a sentinel. Reading its absence as a verdict on a
    /// `tcode` session would error every one of them.
    #[tokio::test]
    async fn unconsumed_launch_is_not_reported_for_a_non_claude_runtime() {
        let dir = tempfile::tempdir().expect("tempdir");
        let driver = InterruptDriver {
            session_live: true,
            interrupts: std::sync::atomic::AtomicUsize::new(0),
        };
        let record = stub_record(crate::runtime::RuntimeKind::Tcode);
        assert_eq!(
            detect_unconsumed_launch_in(&driver, &record, dir.path(), 3, TEST_INTERVAL).await,
            None
        );
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
