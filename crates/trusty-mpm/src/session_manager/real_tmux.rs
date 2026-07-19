//! Real tmux driver adapter bridging [`ManagedTmuxDriver`] to [`TmuxDriver`].
//!
//! Why: the session manager speaks to tmux through the [`ManagedTmuxDriver`]
//! trait seam (so it stays unit-testable), but the daemon already owns a
//! concrete [`crate::daemon::tmux::TmuxDriver`]. This adapter bridges the two so
//! the daemon can hand the session manager a real, tmux-backed driver without
//! duplicating any `tmux` subprocess logic.
//! What: [`RealTmuxDriver`] wraps a `TmuxDriver` and maps each trait method onto
//! the corresponding `TmuxDriver` call, translating its `anyhow`/typed errors
//! into [`ManagedError`].
//! Test: `real_tmux_constructs` (construction is the only side-effect-free path;
//! the actual tmux calls require a live `tmux` binary and are exercised by the
//! `#[ignore]` live integration test in `tests/session_manager_mvp.rs`).

use crate::core::tmux::TmuxTarget;
use crate::daemon::tmux::TmuxDriver;

use super::manager::{ManagedError, ManagedTmuxDriver};

/// Adapter exposing the daemon's [`TmuxDriver`] as a [`ManagedTmuxDriver`].
///
/// Why: the session manager depends on the trait, not the concrete driver, so a
/// thin adapter keeps the manager runtime-agnostic while reusing the daemon's
/// existing tmux subprocess wrapper.
/// What: holds a `TmuxDriver` and forwards every trait method to it, adapting
/// argument and error types.
/// Test: `real_tmux_constructs`.
pub struct RealTmuxDriver {
    driver: TmuxDriver,
}

impl RealTmuxDriver {
    /// Construct a real tmux driver by discovering the `tmux` binary.
    ///
    /// Why: the daemon needs a ready-to-use driver; discovery resolves the
    /// `tmux` path once so subsequent calls avoid repeated PATH lookups.
    /// What: calls [`TmuxDriver::discover`], mapping failure into
    /// [`ManagedError::TmuxUnavailable`].
    /// Test: `real_tmux_constructs` (succeeds only when tmux is installed).
    pub fn discover() -> Result<Self, ManagedError> {
        let driver =
            TmuxDriver::discover().map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;
        Ok(Self { driver })
    }
}

impl ManagedTmuxDriver for RealTmuxDriver {
    fn create_session(&self, name: &str, workdir: &str) -> Result<(), ManagedError> {
        self.driver
            .create_session(name, Some(workdir))
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        self.driver
            .kill_session(name)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    /// Rename the live tmux session via `tmux rename-session -t <old> <new>`,
    /// routed through the shared `core::tmux::run_tmux` choke point (#2398).
    fn rename_session(&self, old: &str, new: &str) -> Result<(), ManagedError> {
        let cmd = crate::core::tmux::TmuxCommand::RenameSession {
            old: old.to_string(),
            new: new.to_string(),
        };
        let output = crate::core::tmux::run_tmux(&cmd)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            Err(ManagedError::TmuxUnavailable(format!(
                "tmux rename-session {old} -> {new} failed: {stderr}"
            )))
        }
    }

    /// Report every tmux session that currently has a client attached, via the
    /// concrete driver's `list-sessions` (`#{session_attached}`).
    fn attached_session_names(&self) -> Vec<String> {
        self.driver
            .list_sessions()
            .map(|sessions| {
                sessions
                    .into_iter()
                    .filter(|s| s.attached)
                    .map(|s| s.name)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn send_line(&self, name: &str, text: &str) -> Result<(), ManagedError> {
        self.driver
            .send_line(&TmuxTarget::session(name), text)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    /// Type literal text with NO trailing Enter (overrides the trait default so
    /// the production path actually omits the newline; #1461).
    fn send_keys_literal(&self, name: &str, text: &str) -> Result<(), ManagedError> {
        self.driver
            .send_keys_literal(&TmuxTarget::session(name), text)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    /// Overrides the trait default so a resume/restart respawn actually lands
    /// in the recorded pane instead of tmux's session-scoped "active pane"
    /// resolution (sibling-window hijack, follow-up to #2456).
    fn send_line_to_pane(&self, name: &str, pane_id: &str, text: &str) -> Result<(), ManagedError> {
        self.driver
            .send_line(&TmuxTarget::pane(name, pane_id), text)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    /// Overrides the trait's optimistic `true` default so the production path
    /// actually calls `tmux list-panes` and confirms the recorded pane is
    /// still among them (sibling-window hijack, follow-up to #2456).
    fn pane_exists(&self, name: &str, pane_id: &str) -> bool {
        self.driver.pane_exists(name, pane_id)
    }

    /// Type literal text with NO trailing Enter into a SPECIFIC pane
    /// (overrides the trait default so `inject`'s `Submit::NoSubmit`
    /// dispatch actually lands in the recorded pane; #2468).
    fn send_keys_literal_to_pane(
        &self,
        name: &str,
        pane_id: &str,
        text: &str,
    ) -> Result<(), ManagedError> {
        self.driver
            .send_keys_literal(&TmuxTarget::pane(name, pane_id), text)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    /// Send Ctrl-C to the session (overrides the no-op trait default; #1461).
    fn send_interrupt(&self, name: &str) -> Result<(), ManagedError> {
        self.driver
            .send_interrupt(&TmuxTarget::session(name))
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    /// Send Ctrl-C to a SPECIFIC pane (overrides the trait default so
    /// `inject`'s `Submit::Interrupt` dispatch actually targets the recorded
    /// pane instead of tmux's session-scoped "active pane" resolution; #2468).
    fn send_interrupt_to_pane(&self, name: &str, pane_id: &str) -> Result<(), ManagedError> {
        self.driver
            .send_interrupt(&TmuxTarget::pane(name, pane_id))
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    fn capture(&self, name: &str, lines: usize) -> Result<String, ManagedError> {
        // The tmux `-S -<n>` argv takes a u32; this driver edge is the single
        // narrowing point so the rest of the observe path stays `usize`. An
        // out-of-range request is saturated to u32::MAX (capturing the whole
        // scrollback) rather than erroring — more pane than asked for is benign.
        let lines = u32::try_from(lines).unwrap_or(u32::MAX);
        self.driver
            .capture(&TmuxTarget::session(name), Some(lines))
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    /// Capture a SPECIFIC pane's output (overrides the trait default so
    /// `observe` actually reads the recorded pane instead of tmux's
    /// session-scoped "active pane" resolution; #2468).
    fn capture_pane(
        &self,
        name: &str,
        pane_id: &str,
        lines: usize,
    ) -> Result<String, ManagedError> {
        let lines = u32::try_from(lines).unwrap_or(u32::MAX);
        self.driver
            .capture(&TmuxTarget::pane(name, pane_id), Some(lines))
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        self.driver
            .list_sessions()
            .map(|sessions| sessions.into_iter().map(|s| s.name).collect())
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    fn get_pane_cwd(&self, name: &str) -> Option<std::path::PathBuf> {
        self.driver.pane_current_path(name)
    }

    /// Overrides the trait's `None` default so the production path actually
    /// calls `tmux display-message -p '#{pane_id}'` (#2453 review finding 1,
    /// round 2).
    fn get_pane_id(&self, name: &str) -> Option<String> {
        self.driver.pane_id(name)
    }

    /// Report the runtime ready when the `claude` child PID has appeared under
    /// the pane shell (overrides the trait's weaker `session_exists` default;
    /// issue #1903 / #1299).
    ///
    /// Why: the pane exists the instant `create_session` returns, long before
    /// `claude` has exec'd — typing the task then would lose it. The presence
    /// of the `claude` child process is the same "runtime is up" signal the
    /// PID-capture path relies on, so injection waits for it.
    /// What: a SINGLE-shot probe (`max_attempts = 1`, no delay) of
    /// [`crate::core::process::find_claude_pid_in_tmux`]; the retry/backoff loop
    /// lives in [`crate::session_manager::SessionManager::inject_task_when_ready`],
    /// so this stays a cheap yes/no the loop polls.
    /// Test: exercised by the `#[ignore]` live integration test (requires a real
    /// `tmux` + `claude`); the pure retry loop is unit-tested against the fake.
    fn runtime_ready(&self, name: &str) -> bool {
        crate::core::process::find_claude_pid_in_tmux(name, 1, std::time::Duration::ZERO).is_some()
    }

    /// Overrides the no-op trait default so the production path actually calls
    /// `tmux set-environment` (#2157 item 1).
    fn set_environment(&self, name: &str, key: &str, value: &str) -> Result<(), ManagedError> {
        self.driver
            .set_environment(name, key, value)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }
}

/// A no-op tmux driver used when `tmux` is not installed.
///
/// Why: the managed-session API must still respond (list/get/spawn-record) even
/// on a host without `tmux`; operations that genuinely need a pane return a
/// typed error rather than panicking, and read-only operations keep working.
/// What: every method that mutates tmux state returns
/// [`ManagedError::TmuxUnavailable`]; `list_sessions` returns an empty list so
/// reconciliation treats every stored session as orphaned.
/// Test: side-effect-only fallback; covered indirectly by the daemon's
/// `session_manager` accessor when tmux is absent.
pub struct NoopTmuxDriver;

impl ManagedTmuxDriver for NoopTmuxDriver {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Err(ManagedError::TmuxUnavailable("tmux not installed".into()))
    }

    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Err(ManagedError::TmuxUnavailable("tmux not installed".into()))
    }

    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }

    /// Fails loudly like every other mutating op on this driver — mirrors
    /// `create_session`/`send_line` rather than the trait's silent-no-op
    /// default, keeping "tmux is absent" uniformly observable.
    fn set_environment(&self, _name: &str, _key: &str, _value: &str) -> Result<(), ManagedError> {
        Err(ManagedError::TmuxUnavailable("tmux not installed".into()))
    }
}

/// A silent no-op tmux driver used in tests that need to create session records
/// without spawning any real tmux process.
///
/// Why: tests that exercise session-manager business logic (lifecycle transitions,
/// route handlers, prune/decommission) must not spawn real `tmpm-*` tmux
/// sessions. Real sessions created by tests escape into the host and get adopted
/// by the production daemon's `reconcile_on_boot`, polluting `~/.trusty-mpm`
/// with phantom sessions (issue #1790). `NoopTmuxDriver` is the production
/// "tmux unavailable" fallback and intentionally returns `Err` on `create_session`
/// — the wrong behavior for tests that call `create_with_id`. This driver instead
/// accepts every mutating call as a silent no-op so session records can be seeded
/// freely without touching real tmux.
/// What: `create_session` and `kill_session` return `Ok(())` silently; `send_line`
/// returns `Ok(())` silently; `capture` returns an empty string; `list_sessions`
/// returns an empty list so `reconcile_on_boot` sees no live sessions and marks
/// stored records `Stopped` (never adopts real host sessions).
/// Test: used by [`DaemonState::with_root_isolated_managed`] — the sole test
/// constructor that pre-seeds the `managed_sessions` OnceCell so the lazy
/// initialiser never touches the real tmux binary.
///
/// Note: compiled unconditionally under the `daemon` feature (not `#[cfg(test)]`)
/// so integration tests in `tests/` can reference it; never called in production.
#[doc(hidden)]
pub struct FakeNoopTmuxDriver;

impl ManagedTmuxDriver for FakeNoopTmuxDriver {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn send_keys_literal(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn send_interrupt(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }

    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_tmux_constructs() {
        // Construction only succeeds when `tmux` is on PATH; in CI it may not be,
        // so we only assert the call does not panic and returns a typed result.
        let _ = RealTmuxDriver::discover();
    }

    #[test]
    fn fake_noop_driver_accepts_create_session() {
        // Why: regression guard that FakeNoopTmuxDriver::create_session returns
        // Ok so test code that seeds session records never hits TmuxUnavailable.
        let driver = FakeNoopTmuxDriver;
        assert!(
            driver.create_session("tmpm-test-abc123", "/tmp").is_ok(),
            "FakeNoopTmuxDriver::create_session must succeed"
        );
    }

    #[test]
    fn noop_driver_set_environment_fails_loudly() {
        // #2157: the no-tmux fallback must fail this best-effort mutating op the
        // same way it fails create_session/send_line, not silently no-op.
        let driver = NoopTmuxDriver;
        assert!(driver.set_environment("s", "K", "v").is_err());
    }

    #[test]
    fn fake_noop_driver_list_sessions_is_empty() {
        // Why: list_sessions must return empty so reconcile_on_boot never adopts
        // real host sessions into the test-owned store.
        let driver = FakeNoopTmuxDriver;
        let sessions = driver.list_sessions().expect("list_sessions ok");
        assert!(
            sessions.is_empty(),
            "FakeNoopTmuxDriver must see no live sessions"
        );
    }
}
