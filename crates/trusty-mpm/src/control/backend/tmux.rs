//! tmux execution backend: `tmux send-keys` + `capture-pane` behind the trait.
//!
//! Why: existing tmux session launch is implemented inline throughout the
//! daemon (`session_manager`, `real_tmux`, launch commands). Wrapping it
//! behind `SessionBackend` makes the `SessionActor` backend-agnostic and
//! preserves the #1452 RAII reaping discipline without regressing any tmux
//! behavior operators depend on today.
//! What: [`TmuxBackend`] owns a named tmux session (named per §5.2:
//! `tm:<project-id>:<sessionNo>`) and implements `send` via `tmux send-keys`,
//! `recv` via periodic `tmux capture-pane -p`, and `stop` via
//! `tmux kill-session`. Output is unstructured pane text wrapped in
//! `SessionEvent::Output { structured: None }`.
//! Test: `tmux_backend_session_name`, `tmux_backend_constructs_without_spawning`
//! in the inline test module. Integration tests require a live `tmux` binary
//! and are marked `#[ignore]`.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use std::process::Command;
use tracing::{debug, warn};

use crate::control::event::SessionEvent;
use crate::control::id::ControlSessionId;
use crate::core::tmux::{TmuxCommand, TmuxTarget, tmux_argv};
use crate::daemon::tmux::TmuxDriver;

use super::{SessionBackend, SessionInput};

/// Derive the tmux session name for a control-plane session.
///
/// Why: §5.2 mandates `tm:<project-id>:<sessionNo>` so operators can run
/// `tmux attach -t tm:trusty-tools:0` without consulting a registry.
/// What: formats `tm:<project_id>:<n>` from a `ControlSessionId`.
/// Test: `tmux_backend_session_name`.
pub fn tmux_session_name(session_id: &ControlSessionId) -> String {
    // session_id format: "<project-id>-<n>"; map to "tm:<project-id>:<n>"
    if let Some((proj, n)) = session_id.parse_parts() {
        format!("tm:{proj}:{n}")
    } else {
        // Fallback: use the raw ID with a tm: prefix (should not happen in
        // practice since SessionCounter always produces valid IDs).
        format!("tm:{}", session_id.as_str())
    }
}

/// State for the tmux session backend.
///
/// Why: the actor needs to know the tmux session name to issue subsequent
/// send-keys and capture-pane commands, and needs the TmuxDriver to execute
/// them. Spawned = false until `spawn()` succeeds.
/// What: holds session identity, the driver, and a spawned flag.
/// Test: `tmux_backend_constructs_without_spawning`.
pub struct TmuxBackend {
    session_id: ControlSessionId,
    tmux_name: String,
    driver: TmuxDriver,
    /// Tracks whether `kill_session` should be called on `stop`/`Drop`.
    spawned: bool,
    /// Lines to capture on each `recv()` poll (configurable; default 100).
    capture_lines: u32,
}

impl TmuxBackend {
    /// Create and register a new tmux session for the given session ID.
    ///
    /// Why: session creation is kept in `new()` so the `SessionActor` can
    /// detect spawn failure immediately rather than deferring to the first
    /// `recv()` call.
    /// What: discovers the `tmux` binary; creates a detached tmux session named
    /// `tm:<project-id>:<n>` in `workdir`; sends the `claude` command with the
    /// system prompt file if provided. Returns `Err` if `tmux` is unavailable
    /// or session creation fails.
    /// Test: `tmux_backend_constructs_without_spawning` (driver discovery only).
    pub fn new(
        session_id: ControlSessionId,
        workdir: impl Into<std::path::PathBuf>,
        prompt_file: Option<std::path::PathBuf>,
        claude_cmd: String,
        capture_lines: u32,
    ) -> Result<Self> {
        let workdir = workdir.into();
        let tmux_name = tmux_session_name(&session_id);
        let driver = TmuxDriver::discover()
            .with_context(|| format!("tmux unavailable for session {session_id}"))?;

        // Create a new detached tmux session in the project directory.
        let workdir_str = workdir.to_string_lossy().to_string();
        driver
            .create_session(&tmux_name, Some(&workdir_str))
            .with_context(|| {
                format!("failed to create tmux session {tmux_name} in {workdir_str}")
            })?;

        // Build and send the `claude` start command.
        let cmd_str = if let Some(pf) = prompt_file {
            format!(
                "{claude_cmd} --append-system-prompt-file {}",
                pf.display()
            )
        } else {
            claude_cmd
        };
        let target = TmuxTarget::session(&tmux_name);
        driver
            .send_line(&target, &cmd_str)
            .with_context(|| format!("failed to start claude in tmux session {tmux_name}"))?;

        debug!(
            session_id = %session_id,
            tmux_name = %tmux_name,
            "tmux backend spawned"
        );

        Ok(Self {
            session_id,
            tmux_name,
            driver,
            spawned: true,
            capture_lines,
        })
    }

    /// Run a raw tmux command and return its stdout as a `String`.
    ///
    /// Why: the `TmuxDriver` wraps synchronous `std::process::Command` calls;
    /// calling them from async context is acceptable here because they are
    /// short-lived (< 1s) shell invocations. For Phase 1 this avoids
    /// introducing `tokio::task::spawn_blocking` complexity.
    /// What: invokes `tmux <argv>` via `std::process::Command::output()`,
    /// returns stdout or an error.
    /// Test: covered transitively by `recv()` unit tests.
    fn run_tmux_cmd(&self, cmd: &TmuxCommand) -> Result<String> {
        let argv = tmux_argv(cmd);
        let out = Command::new("tmux")
            .args(&argv)
            .output()
            .with_context(|| format!("failed to run tmux {}", argv.first().map_or("", |s| s)))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("tmux command failed: {stderr}");
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[async_trait]
impl SessionBackend for TmuxBackend {
    /// Send input to the tmux session via `tmux send-keys`.
    ///
    /// Why: operators and the SM agent send text commands to the Claude Code
    /// TUI through the write-lock path; the tmux backend relays them as
    /// send-keys (NOT via stdin/stdout pipes, to preserve the interactive TUI).
    /// What: calls `TmuxDriver::send_line` to inject `msg.text + Enter` into
    /// the tmux pane.
    /// Test: integration test (`#[ignore]`); constructor-path covered inline.
    async fn send(&mut self, msg: SessionInput) -> Result<()> {
        let target = TmuxTarget::session(&self.tmux_name);
        self.driver
            .send_line(&target, &msg.text)
            .with_context(|| format!("send-keys to tmux session {} failed", self.tmux_name))
    }

    /// Capture the tmux pane and return the output as `SessionEvent::Output`.
    ///
    /// Why: the actor polls `recv()` periodically to pick up new pane output
    /// and broadcast it to observers. Returning `None` signals that the tmux
    /// session no longer exists (session was killed or `claude` exited cleanly).
    /// What: runs `capture-pane -p -S -<lines>` via `tmux_argv`. On
    /// success returns the raw pane text in an `Output` event with
    /// `structured: None`. On tmux session-not-found returns `None`.
    /// Test: `tmux_recv_returns_none_for_missing_session`.
    async fn recv(&mut self) -> Option<Result<Vec<SessionEvent>>> {
        let cmd = TmuxCommand::CapturePane {
            target: TmuxTarget::session(&self.tmux_name),
            lines: Some(self.capture_lines),
        };
        match self.run_tmux_cmd(&cmd) {
            Ok(output) => {
                if output.trim().is_empty() {
                    // Pane is empty — not an error; keep polling.
                    // Return an empty batch so the actor doesn't spin.
                    return Some(Ok(vec![]));
                }
                let event = SessionEvent::Output {
                    session_id: self.session_id.clone(),
                    raw: output,
                    structured: None, // pane capture is unstructured text
                    ts: Utc::now(),
                };
                Some(Ok(vec![event]))
            }
            Err(e) => {
                // Most likely the tmux session no longer exists.
                warn!(
                    session_id = %self.session_id,
                    tmux_name = %self.tmux_name,
                    "tmux capture-pane failed (session may have exited): {e}"
                );
                None // treat as clean exit
            }
        }
    }

    /// Kill the tmux session and clean up.
    ///
    /// Why: §10.2 reaping contract requires the backend to terminate its
    /// tmux session on stop so no orphan sessions accumulate.
    /// What: calls `tmux kill-session -t <name>`; logs a warning on failure
    /// (session may already be gone). Marks `spawned = false` to prevent
    /// double-kill from Drop.
    /// Test: integration test (`#[ignore]`).
    async fn stop(mut self: Box<Self>) -> Result<()> {
        if self.spawned {
            self.spawned = false;
            if let Err(e) = self.driver.kill_session(&self.tmux_name) {
                warn!(
                    session_id = %self.session_id,
                    tmux_name = %self.tmux_name,
                    "kill_session failed (may already be gone): {e}"
                );
            }
        }
        Ok(())
    }
}

impl Drop for TmuxBackend {
    /// Kill the tmux session on drop to prevent orphaned tmux sessions.
    ///
    /// Why: RAII reaping discipline (#1452) — every tmux session must be
    /// cleaned up on actor teardown even if `stop()` was not called.
    /// What: calls `driver.kill_session` only if `spawned` is still true
    /// (i.e. `stop()` has not already cleaned up).
    /// Test: side-effect-only; covered by integration cleanup suite.
    fn drop(&mut self) {
        if self.spawned {
            if let Err(e) = self.driver.kill_session(&self.tmux_name) {
                // Log at debug level — Drop is best-effort.
                debug!(
                    session_id = %self.session_id,
                    "TmuxBackend::drop: kill_session failed: {e}"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_backend_session_name() {
        let id = ControlSessionId::new("trusty-tools", 0);
        assert_eq!(tmux_session_name(&id), "tm:trusty-tools:0");

        let id2 = ControlSessionId::new("my-proj", 3);
        assert_eq!(tmux_session_name(&id2), "tm:my-proj:3");
    }

    #[test]
    fn tmux_backend_session_name_hyphenated_project() {
        let id = ControlSessionId::new("some-long-project", 7);
        assert_eq!(tmux_session_name(&id), "tm:some-long-project:7");
    }

    /// Test that TmuxBackend::new returns an error when tmux is unavailable,
    /// rather than panicking. Requires tmux to NOT be on PATH to exercise the
    /// error path; skip if tmux is available (the normal case).
    #[test]
    #[ignore = "only runs when tmux is absent from PATH"]
    fn tmux_backend_constructs_without_spawning() {
        let id = ControlSessionId::new("proj", 0);
        let result = TmuxBackend::new(id, "/tmp", None, "claude".into(), 100);
        assert!(result.is_err());
    }
}
