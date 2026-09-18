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

/// Attempts [`launch_was_delivered`] makes when reading the sentinel.
///
/// Why: this runs only AFTER the runtime poll has already spent its whole
/// budget, so the sentinel — written by the shim the moment the shell parses the
/// line — has long since appeared if it ever will. A handful of retries covers
/// nothing but filesystem timing. It is deliberately NOT the thing that decides
/// how long a launch may take.
const DELIVERY_READ_ATTEMPTS: u32 = 3;

/// Delay between [`launch_was_delivered`] reads. See [`DELIVERY_READ_ATTEMPTS`].
const DELIVERY_READ_INTERVAL: Duration = Duration::from_millis(100);

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
/// is precisely a shell that took the bytes and never executed them. The shim
/// writes a sentinel the moment it starts, so the sentinel's absence is positive
/// evidence that the LINE, not the launch, is what failed — which is the
/// difference between "not delivered" and "ran and failed" in the operator's
/// report.
///
/// #8233 review round 2 (finding 2): this is a DIAGNOSTIC, never a verdict, and
/// it is read only after [`verify_launch`] has already spent its whole budget
/// and concluded the runtime is absent. The previous shape ran it FIRST on a 2 s
/// budget and let a missing sentinel alone error the record and interrupt the
/// pane — which killed a launch whose `direnv` hook was merely stalling on
/// `gh auth token` while the runtime was still perfectly capable of coming up.
/// Nothing here interrupts anything.
///
/// #8233 review round 2 (finding 3): the sentinel is keyed on the LAUNCH, read
/// through `<session-id>.launch`, so a marker written by an earlier launch of
/// the same session cannot make this one look delivered.
/// What: `true` when the launch is known to have reached the shim. `true` also
/// for a non-Claude-Code runtime (no shim, so no sentinel to read) and when no
/// launch pointer exists — both are "cannot tell", and an unknown must never be
/// reported as a positive "not delivered".
/// Test: `a_delivered_launch_is_reported_as_delivered`,
/// `an_undelivered_launch_is_reported_as_not_delivered`,
/// `a_marker_from_an_earlier_launch_does_not_satisfy_a_later_one`,
/// `delivery_is_assumed_for_a_non_claude_runtime`.
pub(crate) async fn launch_was_delivered_in(
    record: &crate::session_manager::SessionRecord,
    spec_dir: &std::path::Path,
    attempts: u32,
    interval: Duration,
) -> bool {
    if record.runtime != crate::runtime::RuntimeKind::ClaudeCode {
        return true;
    }
    let session = record.id.to_string();
    let Some(launch_id) =
        crate::runtime::launch_spec::LaunchSpec::read_launch_pointer_in(spec_dir, &session)
    else {
        return true;
    };
    let marker = crate::runtime::launch_spec::LaunchSpec::started_marker_in(spec_dir, &launch_id);
    for attempt in 0..attempts.max(1) {
        if attempt > 0 {
            tokio::time::sleep(interval).await;
        }
        if marker.exists() {
            let _ = std::fs::remove_file(&marker);
            return true;
        }
    }
    false
}

/// [`launch_was_delivered_in`] against the production spec root.
///
/// What: `true` — "cannot tell" — when the root cannot be resolved.
/// Test: the four `*_delivered*` tests below drive the body.
async fn launch_was_delivered(record: &crate::session_manager::SessionRecord) -> bool {
    match crate::runtime::launch_spec::LaunchSpec::root() {
        Some(dir) => {
            launch_was_delivered_in(record, &dir, DELIVERY_READ_ATTEMPTS, DELIVERY_READ_INTERVAL)
                .await
        }
        None => true,
    }
}

/// Forget whatever sentinel and pointer this session's launch left behind.
///
/// Why (#8233 review round 2, finding 4): a launch that SUCCEEDED leaves its
/// sentinel and pointer in the spec directory, where only the TTL sweep would
/// eventually collect them. Clearing them on the success path means the common
/// case leaves nothing at all, and the sweep is left to handle only the launches
/// nobody was around to finish.
/// What: best-effort removal of both files; a failure is hygiene, never a
/// launch verdict.
/// Test: `a_running_verdict_clears_the_sentinel_and_the_pointer`.
fn clear_launch_files(record: &crate::session_manager::SessionRecord) {
    let Some(dir) = crate::runtime::launch_spec::LaunchSpec::root() else {
        return;
    };
    let session = record.id.to_string();
    if let Some(launch_id) =
        crate::runtime::launch_spec::LaunchSpec::read_launch_pointer_in(&dir, &session)
    {
        let _ = std::fs::remove_file(crate::runtime::launch_spec::LaunchSpec::started_marker_in(
            &dir, &launch_id,
        ));
    }
    let _ = std::fs::remove_file(crate::runtime::launch_spec::LaunchSpec::launch_pointer_in(
        &dir, &session,
    ));
}

/// The message a `NotStarted` verdict reports, named by whether the launch was
/// ever DELIVERED (#8233 acceptance item 2).
///
/// Why: "the pane printed why on its own last line" is false for an undelivered
/// launch — the pane printed nothing, because nothing ran. Telling an operator
/// to read a pane that has nothing to say is what made this issue take a dozen
/// live occurrences to diagnose. The two cases need different words and
/// different next steps.
/// What: `delivered` chooses between "ran and failed" and "not delivered".
/// `verb` is the caller's noun for the attempt, so the spawn and resume paths
/// keep their own wording.
/// Test: `not_delivered_and_ran_and_failed_are_worded_apart`.
fn not_started_message(record: &crate::session_manager::SessionRecord, delivered: bool) -> String {
    if delivered {
        format!(
            "launch ran and failed: `tm internal-spawn-disclaimed` started in pane '{}' but no \
             runtime came up — the pane printed why on its own last line (a launch spec that \
             could not be read prints there); start again once that is addressed (#8233)",
            record.tmux_name
        )
    } else {
        format!(
            "launch was NOT DELIVERED to pane '{}': tmux accepted the keystrokes, the runtime \
             never came up, and the launch never reached `tm internal-spawn-disclaimed` — so \
             the pane's shell never executed the line and the pane has nothing printed to \
             explain it. Nothing was started and no `claude` is running (#8233)",
            record.tmux_name
        )
    }
}

/// Verify a just-sent resume and record what it found (#6766).
///
/// Why: `resume_managed`'s success arm used to log "managed session resumed and
/// runtime respawned" on the strength of `send-keys` alone. This is the seam
/// that makes that claim conditional, and it lives here rather than inline so
/// `lifecycle.rs` stays inside its frozen SLOC budget.
/// What: [`record_launch_outcome`] with the resume path's context. Returns the
/// failure message when the record was errored, so `resume_managed` can fail its
/// own call rather than returning `Ok` on a session it just marked errored
/// (#8233 review round 2, finding 7).
/// Test: `record_resume_outcome_returns_the_failure_it_recorded`.
pub(crate) async fn record_resume_outcome(
    mgr: &crate::session_manager::SessionManager,
    tmux: &dyn ManagedTmuxDriver,
    record: &crate::session_manager::SessionRecord,
    workspace: &std::path::Path,
) -> Option<String> {
    record_launch_outcome(mgr, tmux, record, Some(workspace)).await
}

/// The shared body of the spawn and resume post-send checks (#8233 review
/// round 2, findings 2, 7 and 10).
///
/// Why: the two wrappers had drifted into different orders and different
/// messages for the same three outcomes. One body means the ordering fix —
/// runtime poll FIRST, sentinel only as a diagnostic afterwards — cannot be
/// half-applied.
/// What: runs [`verify_launch`] with the production budget.
///   * [`LaunchOutcome::Running`] — clears the launch files and CLEARS the
///     record's accumulated error text (acceptance item 3: a session that is
///     running must not keep showing why a previous attempt failed), then logs.
///   * [`LaunchOutcome::NotStarted`] — asks [`launch_was_delivered`] which
///     failure this was, logs at ERROR so the event reaches the daemon's
///     `errors.jsonl` capture layer (acceptance item 10 — nine of these were
///     logged at WARN and never reached it), marks the record errored, and
///     returns the message.
///   * [`LaunchOutcome::Unverifiable`] — changes nothing, exactly as before.
///
/// Test: `not_delivered_and_ran_and_failed_are_worded_apart`,
/// `an_undelivered_launch_is_reported_as_not_delivered`,
/// `an_undelivered_launch_is_not_interrupted_while_the_runtime_may_come_up`.
async fn record_launch_outcome(
    mgr: &crate::session_manager::SessionManager,
    tmux: &dyn ManagedTmuxDriver,
    record: &crate::session_manager::SessionRecord,
    workspace: Option<&std::path::Path>,
) -> Option<String> {
    let outcome = verify_launch(
        tmux,
        &record.tmux_name,
        RESUME_VERIFY_ATTEMPTS,
        RESUME_VERIFY_INTERVAL,
    )
    .await;
    if outcome == LaunchOutcome::NotStarted {
        let delivered = launch_was_delivered(record).await;
        let msg = not_started_message(record, delivered);
        // #8233 acceptance item 10: ERROR, not WARN. The bug-capture layer
        // (`bin/tm/tracing_setup::init_daemon_tracing`) records ERROR events to
        // `errors.jsonl`; at WARN this failure existed only in the session
        // record's `task` field, which is why three live failures left no trace.
        tracing::error!(
            id = %record.id,
            name = %record.tmux_name,
            workspace = %workspace.unwrap_or(std::path::Path::new("")).display(),
            delivered,
            "{msg}"
        );
        let _ = mgr.mark_errored(&record.id, &msg).await;
        return Some(msg);
    }
    if outcome == LaunchOutcome::Running {
        clear_launch_files(record);
        // #8233 acceptance item 3: the runtime is up, so whatever a previous
        // attempt appended to `task` no longer describes this session.
        let _ = mgr.clear_error_note(&record.id).await;
    }
    tracing::info!(
        id = %record.id,
        name = %record.tmux_name,
        ?outcome,
        "managed session launch verified"
    );
    None
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
) -> Option<String> {
    record_launch_outcome(mgr, tmux, record, None).await
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

    /// Plant a launch pointer naming `launch_id`, as `deliver` does.
    fn plant_pointer(dir: &std::path::Path, session: &str, launch_id: &str) {
        std::fs::write(
            crate::runtime::launch_spec::LaunchSpec::launch_pointer_in(dir, session),
            launch_id.as_bytes(),
        )
        .expect("plant the pointer");
    }

    /// The happy path: the shim wrote the sentinel for THIS launch, so the shell
    /// demonstrably ran the line. It also has to CONSUME the sentinel.
    #[tokio::test]
    async fn a_delivered_launch_is_reported_as_delivered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = stub_record(crate::runtime::RuntimeKind::ClaudeCode);
        plant_pointer(dir.path(), &record.id.to_string(), "launch-a");
        let marker =
            crate::runtime::launch_spec::LaunchSpec::started_marker_in(dir.path(), "launch-a");
        std::fs::write(&marker, b"").expect("plant the sentinel");

        assert!(launch_was_delivered_in(&record, dir.path(), 3, TEST_INTERVAL).await);
        assert!(!marker.exists(), "the sentinel must be consumed");
    }

    /// #8233 regression: the pane's shell never ran the launch line. This is a
    /// DIAGNOSTIC — it names which failure happened — and it must never
    /// interrupt anything, because by the time it is read the runtime poll has
    /// already had its full budget.
    #[tokio::test]
    async fn an_undelivered_launch_is_reported_as_not_delivered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = stub_record(crate::runtime::RuntimeKind::ClaudeCode);
        plant_pointer(dir.path(), &record.id.to_string(), "launch-a");

        assert!(!launch_was_delivered_in(&record, dir.path(), 3, TEST_INTERVAL).await);
    }

    /// #8233 review round 2, finding 3: a sentinel left by an EARLIER launch of
    /// this same session must not satisfy a later one. Keyed on the session it
    /// did exactly that — a stuck pane reported as a healthy launch.
    #[tokio::test]
    async fn a_marker_from_an_earlier_launch_does_not_satisfy_a_later_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = stub_record(crate::runtime::RuntimeKind::ClaudeCode);
        // The earlier launch ran and left its marker behind.
        std::fs::write(
            crate::runtime::launch_spec::LaunchSpec::started_marker_in(dir.path(), "launch-old"),
            b"",
        )
        .expect("plant the stale sentinel");
        // The current launch is a different one, and its pane never ran it.
        plant_pointer(dir.path(), &record.id.to_string(), "launch-new");

        assert!(
            !launch_was_delivered_in(&record, dir.path(), 3, TEST_INTERVAL).await,
            "a stale per-session marker must not stand in for this launch"
        );
    }

    /// Only the Claude Code launch runs through `tm internal-spawn-disclaimed`,
    /// so only it writes a sentinel. Reading its absence as a verdict on a
    /// `tcode` session would report every one of them as undelivered.
    #[tokio::test]
    async fn delivery_is_assumed_for_a_non_claude_runtime() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = stub_record(crate::runtime::RuntimeKind::Tcode);
        plant_pointer(dir.path(), &record.id.to_string(), "launch-a");
        assert!(launch_was_delivered_in(&record, dir.path(), 3, TEST_INTERVAL).await);
    }

    /// No pointer at all is "cannot tell", never "not delivered" — an unknown
    /// must not be reported as a positive failure.
    #[tokio::test]
    async fn delivery_is_assumed_without_a_launch_pointer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = stub_record(crate::runtime::RuntimeKind::ClaudeCode);
        assert!(launch_was_delivered_in(&record, dir.path(), 3, TEST_INTERVAL).await);
    }

    /// #8233 acceptance item 10: a relaunch that does not take must be logged
    /// at ERROR, because ERROR is the level the daemon's bug-capture layer
    /// (`bin/tm/tracing_setup::init_daemon_tracing`) records into
    /// `errors.jsonl`.
    ///
    /// Why: at WARN this failure existed only in the session record's `task`
    /// field. Three live launch failures on 2026-09-18 left nothing in
    /// `errors.jsonl` at all, which is why the defect took a dozen occurrences
    /// to diagnose. Fails on a212f8efd, where both wrappers used `warn!`.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn a_relaunch_that_does_not_take_is_recorded_at_error_level() {
        use tracing_subscriber::layer::SubscriberExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let mgr = std::sync::Arc::new(
            crate::session_manager::SessionManager::new(
                dir.path(),
                std::sync::Arc::new(crate::session_manager::FakeNoopTmuxDriver),
            )
            .await
            .expect("session manager"),
        );
        let created = mgr
            .create(
                "launch-verify".into(),
                Some(dir.path().to_path_buf()),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create");
        let mut record = created.clone();
        record.tmux_name = "tmpm-probe".to_owned();
        // Observable session, runtime never comes up: the NotStarted verdict.
        let driver = ProbeDriver {
            session_live: true,
            runtime_up: false,
        };

        // #4181/#4931: `tracing` short-circuits every macro on a process-global
        // MAX_LEVEL a thread-local dispatcher does not raise.
        crate::test_support::enable_event_capture();
        let buffer = trusty_common::log_buffer::LogBuffer::new(64);
        let subscriber = tracing_subscriber::registry().with(
            trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
        );
        // `set_default` returns a guard rather than taking a closure, so the
        // await below runs on the test's own tokio runtime. A `block_on`
        // closure would not drive tokio's timer and `verify_launch`'s sleep
        // would hang forever.
        let _guard = tracing::subscriber::set_default(subscriber);
        let msg = record_spawn_outcome(&mgr, &driver, &record).await;
        drop(_guard);

        let msg = msg.expect("a runtime that never came up must be reported");
        let lines = buffer.tail(64);
        let recorded = lines
            .iter()
            .find(|l| l.contains("no `claude` is running") || l.contains("ran and failed"))
            .unwrap_or_else(|| panic!("the launch failure must be logged at all; got: {lines:?}"));
        assert!(
            recorded.contains("ERROR"),
            "the failure must be logged at ERROR so it reaches errors.jsonl; got: {recorded}"
        );
        assert!(msg.contains("#8233"), "{msg}");
    }

    /// #8233 acceptance item 2: the two failures are different bugs and must
    /// read differently. "The pane printed why on its own last line" is a lie
    /// when nothing ran.
    #[test]
    fn not_delivered_and_ran_and_failed_are_worded_apart() {
        let record = stub_record(crate::runtime::RuntimeKind::ClaudeCode);
        let ran = not_started_message(&record, true);
        let never = not_started_message(&record, false);
        assert!(ran.contains("ran and failed"), "{ran}");
        assert!(ran.contains("printed why"), "{ran}");
        assert!(never.contains("NOT DELIVERED"), "{never}");
        assert!(
            !never.contains("printed why"),
            "an undelivered launch printed nothing: {never}"
        );
    }

    /// #8233 review round 2, finding 2: a launch whose runtime DID come up must
    /// never be interrupted just because its sentinel is missing.
    ///
    /// Why: the pre-fix order asked the sentinel FIRST, on its own 2 s budget,
    /// and an absent marker ALONE errored the record and sent a `C-c` into the
    /// pane. A slow start — live evidence: a `direnv` hook stalling on
    /// `gh auth token` during the launch `cd` — was killed while the runtime
    /// was still perfectly capable of coming up. The runtime poll now runs
    /// first and the sentinel is only consulted to NAME a failure that already
    /// happened, so nothing on this path interrupts anything.
    /// What: drives the real entry point with a pane whose runtime comes up and
    /// whose sentinel never does — the exact combination the old order
    /// destroyed — and asserts no interrupt and no error.
    /// Test: this function IS the test.
    #[tokio::test]
    async fn an_undelivered_launch_is_not_interrupted_while_the_runtime_may_come_up() {
        /// A pane whose runtime IS up, counting every interrupt sent to it.
        struct SlowStarter {
            interrupts: std::sync::atomic::AtomicUsize,
        }

        impl ManagedTmuxDriver for SlowStarter {
            fn create_session(&self, _n: &str, _w: &str) -> Result<(), ManagedError> {
                Ok(())
            }
            fn kill_session(&self, _n: &str) -> Result<(), ManagedError> {
                Ok(())
            }
            fn send_line(&self, _n: &str, _t: &str) -> Result<(), ManagedError> {
                Ok(())
            }
            fn send_interrupt(&self, _n: &str) -> Result<(), ManagedError> {
                self.interrupts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
            fn send_interrupt_to_pane(&self, _n: &str, _p: &str) -> Result<(), ManagedError> {
                self.interrupts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }
            fn capture(&self, _n: &str, _l: usize) -> Result<String, ManagedError> {
                Ok(String::new())
            }
            fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
                Ok(vec!["tmpm-probe".to_string()])
            }
            fn runtime_ready(&self, _n: &str) -> bool {
                true
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let mgr = std::sync::Arc::new(
            crate::session_manager::SessionManager::new(
                dir.path(),
                std::sync::Arc::new(crate::session_manager::FakeNoopTmuxDriver),
            )
            .await
            .expect("session manager"),
        );
        let created = mgr
            .create(
                "slow-start".into(),
                Some(dir.path().to_path_buf()),
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create");
        let mut record = created.clone();
        record.tmux_name = "tmpm-probe".to_owned();
        // The launch was published but its sentinel has not appeared — a shell
        // still finishing its init hooks. The runtime comes up anyway.
        if let Some(root) = crate::runtime::launch_spec::LaunchSpec::root() {
            let _ = std::fs::create_dir_all(&root);
            let _ = std::fs::write(
                crate::runtime::launch_spec::LaunchSpec::launch_pointer_in(
                    &root,
                    &record.id.to_string(),
                ),
                b"a-launch-whose-sentinel-never-lands",
            );
        }
        let driver = SlowStarter {
            interrupts: std::sync::atomic::AtomicUsize::new(0),
        };

        let verdict = record_spawn_outcome(&mgr, &driver, &record).await;

        assert_eq!(
            verdict, None,
            "a runtime that came up is a successful launch, whatever the sentinel says"
        );
        assert_eq!(
            driver.interrupts.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a pane whose runtime came up must never be interrupted — the pre-fix order \
             killed exactly this launch on a 2 s sentinel budget"
        );
        assert_ne!(
            mgr.get(&record.id).await.expect("record").state,
            crate::session_manager::ManagedSessionState::Errored,
            "and it must not be errored either"
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
