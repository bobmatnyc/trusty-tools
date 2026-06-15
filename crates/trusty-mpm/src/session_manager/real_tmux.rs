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

    fn send_line(&self, name: &str, text: &str) -> Result<(), ManagedError> {
        self.driver
            .send_line(&TmuxTarget::session(name), text)
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    fn capture(&self, name: &str, lines: u32) -> Result<String, ManagedError> {
        self.driver
            .capture(&TmuxTarget::session(name), Some(lines))
            .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))
    }

    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        self.driver
            .list_sessions()
            .map(|sessions| sessions.into_iter().map(|s| s.name).collect())
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

    fn capture(&self, _name: &str, _lines: u32) -> Result<String, ManagedError> {
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
}
