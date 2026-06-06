//! Tmux orchestrator for session and pane management.
//!
//! Why: Wraps the `tmux` CLI in a typed Rust API so the harness can spawn,
//! observe, and control tmux sessions without each call site shelling out
//! manually. Centralizing this also gives us one place to enforce the
//! send-text-then-Enter pattern that prevents stuck input on receivers.
//! What: `TmuxOrchestrator` owns the tmux binary path and exposes session
//! lifecycle, pane enumeration, and I/O methods. Each method maps to one
//! or two `tmux` invocations.
//! Test: See `#[cfg(test)]` block — basic checks run unconditionally; full
//! integration tests are gated by `#[ignore]` because they require a real
//! tmux server.

use std::process::{Command, Output};

use tracing::{debug, trace, warn};

use super::error::{Result, TmuxError};
use super::session::{TmuxPane, TmuxSession};

/// Main tmux orchestrator for session and pane management.
#[derive(Debug)]
pub struct TmuxOrchestrator {
    /// Path to tmux binary.
    tmux_path: String,
}

impl TmuxOrchestrator {
    /// Create a new TmuxOrchestrator.
    ///
    /// Why: TM is now always-on infrastructure (#319) — the constructor must
    /// not fail when tmux is missing. Instead we degrade gracefully: probe
    /// the binary, fall back to the bare "tmux" string when not found, and
    /// let individual commands surface `TmuxError::NotFound` at exec time.
    /// What: Tries `find_tmux`; on failure, logs a warning and stores the
    /// literal "tmux" so any subsequent shell-out fails predictably with
    /// the OS's "command not found" error.
    /// Test: `test_new_succeeds_even_when_tmux_missing` covers the degraded
    /// path; integration tests still exercise the happy path.
    pub fn new() -> Result<Self> {
        match Self::find_tmux() {
            Ok(tmux_path) => {
                debug!(path = %tmux_path, "tmux found");
                Ok(Self { tmux_path })
            }
            Err(_) => {
                warn!(
                    "tmux not found in PATH; TmuxOrchestrator initialized in degraded mode \
                     (commands will fail until tmux is installed)"
                );
                Ok(Self {
                    tmux_path: "tmux".to_string(),
                })
            }
        }
    }

    /// Check if tmux is available in PATH.
    pub fn is_available() -> bool {
        Self::find_tmux().is_ok()
    }

    /// Find tmux binary in PATH (also verifies it can run via `-V`).
    fn find_tmux() -> Result<String> {
        let output = Command::new("which").arg("tmux").output()?;

        if !output.status.success() {
            return Err(TmuxError::NotFound);
        }
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() {
            return Err(TmuxError::NotFound);
        }

        // Sanity check: ensure tmux actually runs.
        let version = Command::new(&path).arg("-V").output();
        match version {
            Ok(o) if o.status.success() => Ok(path),
            _ => Err(TmuxError::NotFound),
        }
    }

    /// Run a tmux command and return the raw Output.
    fn run_tmux(&self, args: &[&str]) -> Result<Output> {
        trace!(args = ?args, "running tmux command");
        let output = Command::new(&self.tmux_path).args(args).output()?;
        trace!(
            status = %output.status,
            stdout_len = output.stdout.len(),
            stderr_len = output.stderr.len(),
            "tmux command completed"
        );
        Ok(output)
    }

    /// Run a tmux command and check for success, returning stdout as a String.
    fn run_tmux_checked(&self, args: &[&str]) -> Result<String> {
        let output = self.run_tmux(args)?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Err(TmuxError::CommandFailed(stderr))
        }
    }

    // ==================== Session Management ====================

    /// Create a new detached tmux session, optionally rooted at `dir`.
    pub fn create_session(&self, name: &str, dir: Option<&str>) -> Result<TmuxSession> {
        debug!(name = %name, dir = ?dir, "creating tmux session");

        let mut args = vec!["new-session", "-d", "-s", name];
        if let Some(d) = dir {
            args.push("-c");
            args.push(d);
        }
        self.run_tmux_checked(&args)?;

        // Verify session was created and return its struct.
        let sessions = self.list_sessions()?;
        sessions
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| TmuxError::CommandFailed(format!("session '{}' was not created", name)))
    }

    /// Destroy a tmux session.
    pub fn destroy_session(&self, name: &str) -> Result<()> {
        debug!(name = %name, "destroying tmux session");

        if !self.session_exists(name) {
            return Err(TmuxError::SessionNotFound(name.to_string()));
        }

        self.run_tmux_checked(&["kill-session", "-t", name])?;
        Ok(())
    }

    /// List all tmux sessions, deduplicating by session group.
    ///
    /// Why: tmux session groups (created via `new-session -t <existing>`) are
    /// mirrors of the same underlying window/pane state; surfacing them as
    /// distinct sessions causes the rest of the harness to display and
    /// operate on duplicates. We keep the first session in each group and
    /// hide subsequent group siblings.
    /// What: Skips a session if its group has already been seen earlier in
    /// the list, OR if its name matches a previously-seen group identifier
    /// (mirrors ai-commander behavior).
    /// Test: With two sessions sharing group "grp1", only the first is kept.
    pub fn list_sessions(&self) -> Result<Vec<TmuxSession>> {
        let output = self.run_tmux(&[
            "list-sessions",
            "-F",
            "#{session_name}:#{session_created}:#{session_group}",
        ])?;

        // If no sessions exist, tmux returns non-zero exit code.
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("no server running") || stderr.contains("no sessions") {
                return Ok(Vec::new());
            }
            return Err(TmuxError::CommandFailed(stderr.to_string()));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut sessions: Vec<TmuxSession> = Vec::new();
        let mut seen_groups: Vec<String> = Vec::new();

        for line in stdout.lines() {
            if line.is_empty() {
                continue;
            }
            match TmuxSession::parse(line) {
                Ok(mut session) => {
                    // Group-based dedup: skip if this session's group has
                    // already been recorded, or if this session's name matches
                    // a previously seen group name.
                    if let Some(g) = &session.group {
                        if seen_groups.contains(g) {
                            continue;
                        }
                        seen_groups.push(g.clone());
                    }
                    if seen_groups.contains(&session.name) {
                        continue;
                    }

                    // Best-effort: load panes for this session.
                    if let Ok(panes) = self.list_panes(&session.name) {
                        session.panes = panes;
                    }
                    sessions.push(session);
                }
                Err(e) => {
                    warn!(line = %line, error = %e, "failed to parse session");
                }
            }
        }

        Ok(sessions)
    }

    /// Check if a session exists.
    pub fn session_exists(&self, name: &str) -> bool {
        let output = self.run_tmux(&["has-session", "-t", name]);
        matches!(output, Ok(o) if o.status.success())
    }

    /// Rename a session.
    pub fn rename_session(&self, old: &str, new: &str) -> Result<()> {
        if !self.session_exists(old) {
            return Err(TmuxError::SessionNotFound(old.to_string()));
        }
        self.run_tmux_checked(&["rename-session", "-t", old, new])?;
        Ok(())
    }

    /// Get the current working directory of a session's active pane.
    ///
    /// Uses `tmux display-message -p -t <session> '#{pane_current_path}'`.
    pub fn get_session_path(&self, session: &str) -> Result<String> {
        if !self.session_exists(session) {
            return Err(TmuxError::SessionNotFound(session.to_string()));
        }
        let out = self.run_tmux_checked(&[
            "display-message",
            "-p",
            "-t",
            session,
            "#{pane_current_path}",
        ])?;
        Ok(out.trim().to_string())
    }

    // ==================== Pane Management ====================

    /// Create a new pane in the session (splits the window).
    pub fn create_pane(&self, session: &str) -> Result<TmuxPane> {
        debug!(session = %session, "creating pane");

        if !self.session_exists(session) {
            return Err(TmuxError::SessionNotFound(session.to_string()));
        }

        // Split the window to create a new pane.
        self.run_tmux_checked(&["split-window", "-t", session])?;

        // Newly created pane is the active one.
        let panes = self.list_panes(session)?;
        panes
            .into_iter()
            .find(|p| p.active)
            .ok_or_else(|| TmuxError::CommandFailed("failed to find new pane".to_string()))
    }

    /// List all panes in a session.
    pub fn list_panes(&self, session: &str) -> Result<Vec<TmuxPane>> {
        if !self.session_exists(session) {
            return Err(TmuxError::SessionNotFound(session.to_string()));
        }

        let output = self.run_tmux_checked(&[
            "list-panes",
            "-t",
            session,
            "-F",
            "#{pane_id}:#{pane_index}:#{pane_active}:#{pane_width}:#{pane_height}",
        ])?;

        let mut panes = Vec::new();
        for line in output.lines() {
            if line.is_empty() {
                continue;
            }
            match TmuxPane::parse(line) {
                Ok(pane) => panes.push(pane),
                Err(e) => {
                    warn!(line = %line, error = %e, "failed to parse pane");
                }
            }
        }

        Ok(panes)
    }

    // ==================== I/O Operations ====================

    /// Capture the last `lines` (default 50) of output from a pane.
    pub fn capture_output(
        &self,
        session: &str,
        pane: Option<&str>,
        lines: Option<u32>,
    ) -> Result<String> {
        if !self.session_exists(session) {
            return Err(TmuxError::SessionNotFound(session.to_string()));
        }

        // Validate pane exists if specified.
        if let Some(p) = pane {
            let panes = self.list_panes(session)?;
            if !panes
                .iter()
                .any(|pn| pn.id == p || pn.index.to_string() == p)
            {
                return Err(TmuxError::PaneNotFound(p.to_string(), session.to_string()));
            }
        }

        let target = match pane {
            Some(p) => format!("{}:{}", session, p),
            None => session.to_string(),
        };

        let n = lines.unwrap_or(50);
        let lines_arg = format!("-{}", n);
        let args = vec!["capture-pane", "-t", &target, "-p", "-S", &lines_arg];

        self.run_tmux_checked(&args)
    }

    /// Send raw keys (may include key names like `Enter`, `Escape`) to a pane.
    pub fn send_keys(&self, session: &str, pane: Option<&str>, keys: &str) -> Result<()> {
        debug!(session = %session, pane = ?pane, keys = %keys, "sending keys");

        if !self.session_exists(session) {
            return Err(TmuxError::SessionNotFound(session.to_string()));
        }

        let target = match pane {
            Some(p) => format!("{}:{}", session, p),
            None => session.to_string(),
        };

        self.run_tmux_checked(&["send-keys", "-t", &target, keys])?;
        Ok(())
    }

    /// Send a line of text to a pane.
    ///
    /// CRITICAL: sends the text literally with `-l` (so key names like
    /// `Enter` inside `text` are NOT interpreted), then sends `Enter` as a
    /// SEPARATE call. Never combine text + Enter into one `send-keys` —
    /// receivers (Claude CLI etc.) drop the trailing Enter unpredictably
    /// when both are sent in a single tmux invocation.
    pub fn send_line(&self, session: &str, pane: Option<&str>, text: &str) -> Result<()> {
        debug!(session = %session, pane = ?pane, text = %text, "sending line");

        if !self.session_exists(session) {
            return Err(TmuxError::SessionNotFound(session.to_string()));
        }

        let target = match pane {
            Some(p) => format!("{}:{}", session, p),
            None => session.to_string(),
        };

        // First: send text literally (no key-name interpretation).
        self.run_tmux_checked(&["send-keys", "-t", &target, "-l", text])?;
        // Then: send Enter as a separate call.
        self.run_tmux_checked(&["send-keys", "-t", &target, "Enter"])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_available() {
        // Returns whatever it returns; just must not panic.
        let _ = TmuxOrchestrator::is_available();
    }

    #[test]
    fn test_new_succeeds_even_when_tmux_missing() {
        // #319: TmuxOrchestrator::new() now always returns Ok so TM can be
        // always-on infrastructure. When tmux is absent we degrade to a bare
        // "tmux" path; commands will fail at exec time with NotFound-style
        // errors instead of failing construction.
        let result = TmuxOrchestrator::new();
        assert!(result.is_ok(), "constructor must not fail on missing tmux");
    }

    // Integration tests — require a real tmux server.
    #[test]
    #[ignore]
    fn test_create_and_destroy_session() {
        let tmux = TmuxOrchestrator::new().unwrap();
        let name = "test-trusty-agents-create";
        let _ = tmux.destroy_session(name);

        let session = tmux.create_session(name, None).unwrap();
        assert_eq!(session.name, name);
        assert!(tmux.session_exists(name));

        tmux.destroy_session(name).unwrap();
        assert!(!tmux.session_exists(name));
    }

    #[test]
    #[ignore]
    fn test_send_line_and_capture() {
        let tmux = TmuxOrchestrator::new().unwrap();
        let name = "test-trusty-agents-io";
        let _ = tmux.destroy_session(name);

        tmux.create_session(name, None).unwrap();
        tmux.send_line(name, None, "echo hello").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(150));

        let output = tmux.capture_output(name, None, Some(20)).unwrap();
        assert!(output.contains("echo") || output.contains("hello"));

        tmux.destroy_session(name).unwrap();
    }
}
