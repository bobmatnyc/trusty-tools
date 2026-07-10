//! The [`ManagedTmuxDriver`] trait seam used by the session manager.
//!
//! Why: split out of `manager.rs` (issue #1955) purely to keep that file under
//! the 500-SLOC production cap after the serial-numbered naming rework added a
//! new error variant and helper method. The trait itself has no behavior
//! change — it is a mechanical move.
//! What: [`ManagedTmuxDriver`], the minimal tmux operation surface the manager
//! depends on, with default implementations for the operations most drivers
//! do not need to override (`send_keys_literal`, `send_interrupt`,
//! `get_pane_cwd`, `session_exists`, `graceful_stop`).
//! Test: exercised via [`crate::session_manager::real_tmux::RealTmuxDriver`]
//! and the `FakeTmuxDriver` test double in `tests.rs`.

use super::manager::ManagedError;

/// Trait seam over tmux operations used by the session manager.
///
/// Why: the manager must be fully unit-testable without a live tmux binary;
/// a trait lets tests inject a `FakeTmuxDriver` instead of the real
/// [`crate::daemon::tmux::TmuxDriver`].
/// What: minimal surface — create session, kill session, send a line, capture
/// pane output, list session names, and probe existence.
/// Test: `FakeTmuxDriver` in `manager.rs`'s test section (`tests.rs`).
pub trait ManagedTmuxDriver: Send + Sync {
    /// Create a detached tmux session named `name`, rooted at `workdir`.
    fn create_session(&self, name: &str, workdir: &str) -> Result<(), ManagedError>;

    /// Kill the tmux session named `name`.
    fn kill_session(&self, name: &str) -> Result<(), ManagedError>;

    /// Send literal text followed by Enter to the session named `name`.
    fn send_line(&self, name: &str, text: &str) -> Result<(), ManagedError>;

    /// Send literal text to `name` WITHOUT a trailing Enter (#1461).
    ///
    /// Why: the harness-agnostic [`Submit::NoSubmit`](crate::core::sm::control::Submit::NoSubmit)
    /// intent must type into the pane without committing the line (e.g. staging a
    /// partial command a human will edit). Splitting this out keeps `send_line`
    /// (literal + Enter) and this (literal only) as distinct, testable primitives.
    /// What: sends `text` literally. The default returns an explicit
    /// `TmuxUnavailable` error rather than silently falling back to `send_line`:
    /// delegating to `send_line` would append an Enter and SUBMIT a line the
    /// caller asked NOT to submit (`Submit::NoSubmit`) — a silent-wrong-behavior
    /// trap. Any driver actually used with `inject(.., NoSubmit)` MUST override
    /// this (the real `RealTmuxDriver` does); an un-overriding driver fails loudly.
    /// Test: `RealTmuxDriver` override is asserted via `core::tmux` argv tests;
    /// the no-submit dispatch is asserted by `inject_dispatch_nosubmit_sends_literal_only`.
    fn send_keys_literal(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Err(ManagedError::TmuxUnavailable(
            "send_keys_literal not implemented for this driver".into(),
        ))
    }

    /// Send an interrupt (Ctrl-C) to the session named `name` (#1461).
    ///
    /// Why: the [`Submit::Interrupt`](crate::core::sm::control::Submit::Interrupt)
    /// intent stops a running task in place (the clean precursor to relaunching a
    /// runtime). Exposing it on the trait keeps the interrupt path runtime-neutral.
    /// What: sends the runtime's interrupt. The default returns an explicit
    /// `TmuxUnavailable` error rather than a silent no-op `Ok(())`: Interrupt is
    /// the verb used to STOP a running task, so a silent no-op would leave the
    /// task running while the caller believes it was interrupted — a dangerous
    /// silent-wrong-behavior trap. Any driver actually used with
    /// `inject(.., Interrupt)` MUST override this (the real `RealTmuxDriver` sends
    /// `C-c`); an un-overriding driver fails loudly.
    /// Test: `RealTmuxDriver` override via `core::tmux::send_keys_keyname_argv`;
    /// dispatch asserted by `inject_dispatch_interrupt_sends_ctrl_c`.
    fn send_interrupt(&self, _name: &str) -> Result<(), ManagedError> {
        Err(ManagedError::TmuxUnavailable(
            "send_interrupt not implemented for this driver".into(),
        ))
    }

    /// Capture the last `lines` of pane output for the session named `name`.
    ///
    /// `lines` is `usize` to match the harness-agnostic
    /// [`SessionControl::observe`](crate::core::sm::control::SessionControl::observe)
    /// contract end-to-end; the single `usize → u32` narrowing the underlying
    /// tmux `-S` argv requires is confined to the real-tmux driver edge, so no
    /// caller in the observe path needs a `try_from`.
    fn capture(&self, name: &str, lines: usize) -> Result<String, ManagedError>;

    /// Return the pane's current working directory for snapshot-before-stop (#1816).
    ///
    /// Why: the idle auto-stop path captures cwd just before killing tmux so
    /// `resume()` can restore the operator's working directory instead of always
    /// starting at the workspace root. Returning `None` (the default) is safe —
    /// resume falls back to `workspace_path`/`cwd` as before.
    /// What: returns the `pane_current_path` tmux format string value, or `None`
    /// if the driver does not support it or the call fails.
    /// Test: `RealTmuxDriver` runs `tmux display-message`; fake drivers return `None`.
    fn get_pane_cwd(&self, _name: &str) -> Option<std::path::PathBuf> {
        None
    }

    /// Return all live tmux session names on the host.
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError>;

    /// True if a tmux session with this name currently exists.
    fn session_exists(&self, name: &str) -> bool {
        self.list_sessions()
            .map(|names| names.iter().any(|n| n == name))
            .unwrap_or(false)
    }

    /// Probe whether the runtime inside `name`'s pane is READY to accept typed
    /// input (issue #1903 / #1299).
    ///
    /// Why: turnkey task injection (`SessionManager::inject_task_when_ready`)
    /// must not type the task into the pane before the runtime (`claude`) has
    /// actually launched — keystrokes sent to a pane whose shell is still
    /// exec-ing the runtime are lost or mangled. This is the readiness signal
    /// the injection loop polls (with bounded backoff) before delivering the
    /// task, replacing the blind fixed sleep the meta-launch prototype used.
    /// What: the default is `session_exists(name)` — sufficient for the test
    /// doubles (a fake reports its created session as existing) and a safe
    /// lower bound for any driver that cannot inspect the pane's process tree.
    /// [`super::real_tmux::RealTmuxDriver`] overrides this to the stronger
    /// signal: the `claude` child PID has appeared under the pane shell (the
    /// same probe `daemon::services::session_service::spawn_pid_capture` uses).
    /// Test: `RealTmuxDriver`'s override is exercised by the `#[ignore]` live
    /// integration test; the default is exercised via `FakeTmuxDriver` in
    /// `session_manager::task_inject`'s tests.
    fn runtime_ready(&self, name: &str) -> bool {
        self.session_exists(name)
    }

    /// Durably publish `key=value` into the named session's tmux environment
    /// (#2157 item 1) — belt-and-suspenders alongside the pane-shell `export …;`
    /// prefix `runtime::claude_code`/`runtime::tcode` already send.
    ///
    /// Why: the pane-shell export only lands in the ONE shell process that ran
    /// the spawn/resume command line; a sibling pane/window in the same tmux
    /// session (or a pane spawned before this method existed) never sees it.
    /// `tmux set-environment` writes into the SESSION's own environment table,
    /// which `tmux show-environment` can read from ANY pane in that session
    /// regardless of vintage — this is what lets the in-place-relaunch gate
    /// (`bin/tm/commands/guided_inplace.rs`) fall back to a tmux-side read when
    /// the process environment does not carry `TM_MANAGED_SESSION_ID`.
    /// What: the default is a silent `Ok(())` no-op — this is best-effort
    /// durability plumbing, not a caller-visible control action like
    /// `send_keys_literal`/`send_interrupt`, so a driver that cannot support it
    /// (e.g. tmux absent) must never fail the spawn/resume it is attached to.
    /// [`super::real_tmux::RealTmuxDriver`] overrides this to actually call
    /// tmux; test doubles record the call for assertions.
    /// Test: `RealTmuxDriver` override is asserted via `core::tmux::
    /// set_environment_argv`; call-site assertions live in
    /// `runtime::test_helpers::FakeTmux` and `session_manager::tests::
    /// FakeTmuxDriver`.
    fn set_environment(&self, _name: &str, _key: &str, _value: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    /// Signal a session's process to stop, then kill the tmux session.
    ///
    /// Why: abruptly killing a session (`kill_session`) discards any in-flight
    /// work the claude process was persisting. Sending a termination signal first
    /// gives the process a chance to flush state — the async caller is responsible
    /// for inserting the ~2 s grace window before calling this (see
    /// `SessionManager::shutdown` in `restart_ops.rs`). This method is
    /// intentionally synchronous so it can live on the non-async trait; the delay
    /// is owned by the async shutdown path to avoid blocking a Tokio worker thread.
    /// What: if `claude_pid` is known, sends SIGTERM via `nix::signal::kill`;
    /// then unconditionally calls `kill_session`. If `claude_pid` is `None`,
    /// falls back to `send_interrupt` (Ctrl-C) before the kill. Signal errors are
    /// logged as warnings (the process may have already exited); only
    /// `kill_session` failure is returned as `Err`. Does NOT sleep — callers
    /// must insert an async delay (`tokio::time::sleep`) between the signal phase
    /// and calling this if a grace window is desired.
    /// Test: `fake_driver_graceful_stop_with_pid` (pid known, records kill),
    /// `fake_driver_graceful_stop_without_pid` (no pid, falls back to C-c).
    fn graceful_stop(&self, name: &str, claude_pid: Option<u32>) -> Result<(), ManagedError> {
        self.signal_terminate(name, claude_pid);
        self.kill_session(name)
    }

    /// Signal a session's `claude` process to stop WITHOUT killing the pane.
    ///
    /// Why: a truly graceful teardown sends the termination signal, waits a grace
    /// window so `claude` can flush state, and only THEN reclaims the pane. That
    /// ordering requires the signal and the kill to be separate steps — so this is
    /// the signal-only half, letting an async caller insert a `tokio::time::sleep`
    /// grace window between it and `kill_session` (see
    /// [`SessionManager::graceful_terminate_runtime`] for the CLI stop/decommission
    /// path). `graceful_stop` composes this with an immediate kill for the
    /// batched, one-grace-window-for-all shutdown path.
    /// What: SIGTERMs the known `claude_pid` via `nix::signal::kill` (unix only);
    /// when the pid is unknown, falls back to a single `send_interrupt` (Ctrl-C).
    /// Signal errors are logged and swallowed — the process may already be gone —
    /// so this method is infallible (the fallible reclaim lives in `kill_session`).
    /// Test: `fake_driver_graceful_stop_with_pid` / `_without_pid` cover the
    /// composed `graceful_stop`; the CLI drain is covered by
    /// `graceful_terminate_runtime_signals_then_kills` in `restart_ops`.
    fn signal_terminate(&self, name: &str, claude_pid: Option<u32>) {
        if let Some(pid) = claude_pid {
            #[cfg(unix)]
            {
                use nix::sys::signal::{Signal, kill};
                use nix::unistd::Pid;
                if let Err(e) = kill(Pid::from_raw(pid as i32), Signal::SIGTERM) {
                    tracing::warn!(pid, name, "signal_terminate: SIGTERM failed: {e}");
                }
            }
            #[cfg(not(unix))]
            {
                let _ = pid; // SIGTERM not applicable on non-unix
            }
        } else {
            // No pid — best effort interrupt via tmux send-keys.
            let _ = self.send_interrupt(name);
        }
    }
}
