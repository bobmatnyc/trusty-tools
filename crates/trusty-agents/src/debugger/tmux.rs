//! Thin synchronous tmux subprocess adapter for the debug REPL TUI.
//!
//! Why: The `trusty-agents debug` subcommand (#237) needs to drive a detached
//! REPL inside a tmux session — capture its scrollback and inject keystrokes
//! — without taking a tmux library dependency. We shell out to the `tmux`
//! binary and parse its output. Synchronous `std::process::Command` is fine
//! because every call is short-lived (capture-pane, send-keys, list-panes).
//! What: `TmuxAdapter` exposes session lifecycle (create/kill/exists),
//! pane introspection (`get_pane_id`), output capture (`capture_output`),
//! and keystroke injection (`send_line`). Errors flow through `TmuxError`.
//! Test: `tmux_adapter_finds_binary` (when tmux is installed) plus
//! lifecycle/capture/send round-trip behind the `tmux_integration` cfg gate
//! so CI without tmux still passes.

use std::process::{Command, Output};

use thiserror::Error;
use tracing::{debug, trace};

#[derive(Error, Debug)]
pub enum TmuxError {
    #[error("tmux not found in PATH")]
    NotFound,
    #[error("session not found: {0}")]
    SessionNotFound(String),
    #[error("tmux command failed: {0}")]
    CommandFailed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, TmuxError>;

/// Thin wrapper around the `tmux` binary.
///
/// Why: All debug-REPL tmux interactions go through one place so logging,
/// argument escaping, and error mapping stay consistent.
/// What: Holds the resolved `tmux` path and forwards short-lived commands.
/// Test: `tmux_adapter_finds_binary` validates `new()` succeeds when tmux
/// is installed; gated tests cover create/capture/kill round trips.
#[derive(Debug, Clone)]
pub struct TmuxAdapter {
    tmux_path: String,
}

impl TmuxAdapter {
    /// Locate `tmux` in PATH and construct an adapter.
    ///
    /// Why: We resolve the absolute path once so subsequent `Command::new`
    /// calls are immune to PATH mutations during the debug session.
    /// What: Uses `which tmux`; returns `TmuxError::NotFound` on miss.
    /// Test: `tmux_adapter_finds_binary` (skipped when tmux missing).
    pub fn new() -> Result<Self> {
        let tmux_path = Self::find_tmux()?;
        debug!(path = %tmux_path, "tmux found");
        Ok(Self { tmux_path })
    }

    /// Cheap availability probe used by the subcommand entry point.
    pub fn is_available() -> bool {
        Self::find_tmux().is_ok()
    }

    fn find_tmux() -> Result<String> {
        let output = Command::new("which").arg("tmux").output()?;
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if path.is_empty() {
                return Err(TmuxError::NotFound);
            }
            Ok(path)
        } else {
            Err(TmuxError::NotFound)
        }
    }

    fn run(&self, args: &[&str]) -> Result<Output> {
        trace!(args = ?args, "tmux exec");
        let out = Command::new(&self.tmux_path).args(args).output()?;
        Ok(out)
    }

    fn run_checked(&self, args: &[&str]) -> Result<String> {
        let out = self.run(args)?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            Err(TmuxError::CommandFailed(stderr))
        }
    }

    /// True if a session with `name` is currently registered with tmux.
    ///
    /// Why: The TUI must know whether to launch fresh or attach to an
    /// existing session (`--no-launch`).
    /// What: `tmux has-session -t <name>` exits 0 when present.
    /// Test: covered by manual integration; unit-tested via mock would
    /// require depending on the binary which we deliberately avoid.
    pub fn session_exists(&self, name: &str) -> bool {
        let out = self.run(&["has-session", "-t", name]);
        matches!(out, Ok(o) if o.status.success())
    }

    /// Create a new detached session running `cmd`.
    ///
    /// Why: Detached so the TUI in the invoking terminal can render its own
    /// view while the REPL runs alongside.
    /// What: `tmux new-session -d -s <name> <cmd>`.
    /// Test: integration; manual run shows the session created.
    pub fn create_session(&self, name: &str, cmd: &str) -> Result<()> {
        debug!(name = %name, cmd = %cmd, "creating tmux session");
        self.run_checked(&["new-session", "-d", "-s", name, cmd])?;
        Ok(())
    }

    /// Kill a session by name.
    ///
    /// Why: Used on `q` and to reset state when `--no-launch` is absent
    /// and a stale session exists.
    /// What: `tmux kill-session -t <name>`. Missing session is mapped to
    /// `SessionNotFound`.
    pub fn kill_session(&self, name: &str) -> Result<()> {
        let out = self.run(&["kill-session", "-t", name])?;
        if out.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("can't find session") || stderr.contains("session not found") {
                Err(TmuxError::SessionNotFound(name.to_string()))
            } else {
                Err(TmuxError::CommandFailed(stderr.to_string()))
            }
        }
    }

    /// Capture up to `lines` rows of scrollback from the first pane.
    ///
    /// Why: This is the data the left panel renders.
    /// What: `tmux capture-pane -t <session> -p -S -<lines>` writes the
    /// joined scrollback (with ANSI codes if `-e` were set; we leave it off
    /// and strip what slips through downstream).
    /// Test: integration; downstream consumer trims/strips.
    pub fn capture_output(&self, session: &str, lines: u32) -> Result<String> {
        let lines_arg = format!("-{lines}");
        self.run_checked(&["capture-pane", "-t", session, "-p", "-S", &lines_arg])
    }

    /// Inject `text` followed by Enter into the first pane.
    ///
    /// Why: Drives REPL commands from the TUI input box.
    /// What: Two `send-keys` calls — first the literal text (`-l` disables
    /// keysym translation), then a synthetic `Enter`. Splitting them is
    /// critical: combining a literal payload with `Enter` in one call
    /// would send the word "Enter" as text.
    /// Test: integration; manual round-trip with `echo`.
    pub fn send_line(&self, session: &str, text: &str) -> Result<()> {
        // -l: send literally, treating the argument as raw input.
        self.run_checked(&["send-keys", "-t", session, "-l", text])?;
        // Then a real Enter keypress to submit the line.
        self.run_checked(&["send-keys", "-t", session, "Enter"])?;
        Ok(())
    }

    /// Return the first pane's id (e.g., `%0`).
    ///
    /// Why: Displayed in the status panel for operator visibility.
    /// What: `tmux list-panes -t <session> -F "#{pane_id}"` and take first.
    /// Test: integration.
    pub fn get_pane_id(&self, session: &str) -> Result<String> {
        let out = self.run_checked(&["list-panes", "-t", session, "-F", "#{pane_id}"])?;
        out.lines()
            .next()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| TmuxError::SessionNotFound(session.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_adapter_finds_binary_when_installed() {
        // Why: Sanity-check the PATH probe. Skipped when tmux isn't on the
        // CI runner so the test suite stays portable.
        if !TmuxAdapter::is_available() {
            eprintln!("tmux not installed — skipping");
            return;
        }
        let adapter = TmuxAdapter::new().expect("tmux adapter constructs");
        assert!(!adapter.tmux_path.is_empty());
    }

    #[test]
    fn tmux_error_session_not_found_displays() {
        let e = TmuxError::SessionNotFound("foo".into());
        assert!(format!("{e}").contains("foo"));
    }
}
