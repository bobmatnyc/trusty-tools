//! tmux process driver.
//!
//! Why: `trusty-mpm-core::tmux` builds tmux argv vectors but never spawns a
//! process — that keeps it pure and testable. The daemon needs the other half:
//! actually running `tmux` and interpreting its exit status. This module is
//! distilled from `ai-commander`'s `commander-tmux` orchestrator and
//! `open-mpm`'s `tm` manager — find the binary once, run argv, classify the
//! "no server running" empty-list case.
//! What: [`TmuxDriver`] wraps the resolved `tmux` path; it can create/kill/list
//! sessions, send keystrokes, and capture pane output. [`SessionInfo`] is one
//! parsed `list-sessions` row.
//! Test: `cargo test -p trusty-mpm-daemon` covers binary discovery degradation
//! and `list-sessions` row parsing without requiring tmux to be installed.

// The session-start command path (which spawns Claude Code into a tmux
// session) lands in a follow-up issue; until then this driver is exercised
// only by its own tests, so its public surface is intentionally unused.
#![allow(dead_code)]

use std::process::Command;

use crate::core::external_session::ExternalSession;
use crate::core::tmux::{TmuxCommand, TmuxTarget, tmux_argv};
use crate::core::{Error, Result};

/// A parsed `tmux list-sessions` row.
///
/// Why: the dashboard wants structured session data, not raw tmux text.
/// What: the fields from `SESSION_LIST_FORMAT` — name, creation epoch, attached.
/// Test: `parses_session_row`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// tmux session name.
    pub name: String,
    /// Unix epoch seconds the session was created.
    pub created: i64,
    /// Whether a client is currently attached.
    pub attached: bool,
}

impl SessionInfo {
    /// Parse one `name:created:attached` row from `list-sessions`.
    ///
    /// Why: a single parser keeps the format in sync with
    /// `core::tmux::SESSION_LIST_FORMAT`.
    /// What: splits on `:`; tolerates a malformed `attached` flag by defaulting
    /// it to `false`.
    /// Test: `parses_session_row`.
    pub fn parse(line: &str) -> Result<Self> {
        let mut parts = line.splitn(3, ':');
        let name = parts
            .next()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::Protocol(format!("empty tmux session row: {line:?}")))?
            .to_string();
        let created = parts
            .next()
            .and_then(|s| s.parse::<i64>().ok())
            .ok_or_else(|| Error::Protocol(format!("bad tmux created field: {line:?}")))?;
        let attached = parts.next().map(|s| s == "1").unwrap_or(false);
        Ok(Self {
            name,
            created,
            attached,
        })
    }
}

/// Drives the `tmux` binary on behalf of the daemon's session manager.
///
/// Why: hosting Claude Code inside tmux is the primary control model; the
/// daemon needs a thin, fallible wrapper rather than scattering `Command`
/// calls. Holding the resolved path means PATH is consulted only once.
/// What: stores the `tmux` executable path; methods execute typed
/// [`TmuxCommand`]s built by `core::tmux`.
/// Test: `driver_reports_availability`.
#[derive(Debug, Clone)]
pub struct TmuxDriver {
    /// Absolute path to the `tmux` binary.
    tmux_path: String,
}

impl TmuxDriver {
    /// Resolve the `tmux` binary, or fail if it cannot be found.
    ///
    /// Why: the daemon should refuse the tmux control model up front rather
    /// than fail on the first session start. Under launchd the daemon inherits
    /// a minimal `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`) that omits Homebrew,
    /// so a bare `PATH` lookup of `tmux` (which lives at `/opt/homebrew/bin`)
    /// returns nothing and every managed-session spawn 500s after a restart
    /// (#1298). Resolving via the well-known dirs makes discovery survive the
    /// minimal inherited `PATH`.
    /// What: delegates to [`trusty_common::bin_resolve::resolve_binary`], which
    /// consults the live `PATH` first and then falls back to the well-known
    /// daemon dirs (Homebrew + user bins). Errors with a clear message if no
    /// `tmux` is found anywhere.
    /// Test: `driver_reports_availability` (skips assertion when tmux missing).
    pub fn discover() -> Result<Self> {
        let path = trusty_common::bin_resolve::resolve_binary("tmux").ok_or_else(|| {
            Error::Protocol(
                "tmux not found on PATH or in well-known dirs (e.g. /opt/homebrew/bin); \
                 use the PTY or SDK control model"
                    .into(),
            )
        })?;
        let path = path
            .to_str()
            .ok_or_else(|| Error::Protocol("resolved tmux path is not valid UTF-8".into()))?
            .to_string();
        Ok(Self { tmux_path: path })
    }

    /// True if a `tmux` binary is available on this host.
    pub fn is_available() -> bool {
        Self::discover().is_ok()
    }

    /// Run a typed tmux command, returning captured stdout on success.
    ///
    /// Why: every other method routes through here so exit-status handling
    /// lives in one place.
    /// What: renders argv via `core::tmux::tmux_argv`, runs `tmux`, and maps a
    /// non-zero exit to `Error::Protocol` carrying stderr.
    /// Test: exercised indirectly by the `#[ignore]` integration tests.
    fn run(&self, cmd: &TmuxCommand) -> Result<String> {
        let argv = tmux_argv(cmd);
        let output = Command::new(&self.tmux_path).args(&argv).output()?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            Err(Error::Protocol(format!("tmux {argv:?} failed: {stderr}")))
        }
    }

    /// Create a detached tmux session named `name`, optionally in `workdir`.
    ///
    /// Idempotent: if a session with the same name already exists, the
    /// underlying `tmux new-session -A` attaches to it instead of failing
    /// with a "duplicate session" error.
    pub fn create_session(&self, name: &str, workdir: Option<&str>) -> Result<()> {
        self.run(&TmuxCommand::NewSession {
            name: name.to_string(),
            workdir: workdir.map(str::to_string),
        })?;
        Ok(())
    }

    /// Kill the tmux session named `name`.
    pub fn kill_session(&self, name: &str) -> Result<()> {
        self.run(&TmuxCommand::KillSession {
            name: name.to_string(),
        })?;
        Ok(())
    }

    /// List all tmux sessions on this host.
    ///
    /// Why: the multi-session dashboard enumerates every running session.
    /// What: runs `list-sessions`; tmux exits non-zero with "no server running"
    /// when there are zero sessions — that is mapped to an empty `Vec`.
    /// Test: row parsing covered by `parses_session_row`.
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let argv = tmux_argv(&TmuxCommand::ListSessions);
        let output = Command::new(&self.tmux_path).args(&argv).output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("no server running") || stderr.contains("no sessions") {
                return Ok(Vec::new());
            }
            return Err(Error::Protocol(format!("tmux list-sessions: {stderr}")));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut sessions = Vec::new();
        for line in stdout.lines().filter(|l| !l.is_empty()) {
            sessions.push(SessionInfo::parse(line)?);
        }
        Ok(sessions)
    }

    /// Send literal text to a session/pane, then press Enter to execute it.
    ///
    /// Why: launching Claude Code or feeding it a prompt means typing a line
    /// and submitting it; tmux needs the text sent with `-l` (literal) and the
    /// `Enter` keypress sent separately.
    /// What: two `send-keys` invocations — literal text, then the `Enter` key.
    /// Test: argv shapes covered in `core::tmux` tests.
    pub fn send_line(&self, target: &TmuxTarget, text: &str) -> Result<()> {
        self.run(&TmuxCommand::SendKeys {
            target: target.clone(),
            keys: text.to_string(),
            literal: true,
        })?;
        self.run(&TmuxCommand::SendKeys {
            target: target.clone(),
            keys: "Enter".to_string(),
            literal: false,
        })?;
        Ok(())
    }

    /// Send literal text to a session/pane WITHOUT a trailing Enter.
    ///
    /// Why: the harness-agnostic no-submit inject intent (#1461) types into the
    /// pane but leaves the line uncommitted (vs. [`send_line`](Self::send_line),
    /// which appends Enter). Keeping it a separate one-shot avoids the second
    /// `Enter` send-keys call.
    /// What: a single `send-keys -l` invocation (literal, no key-name follow-up).
    /// Test: `core::tmux::send_keys_literal_argv` covers the argv shape.
    pub fn send_keys_literal(&self, target: &TmuxTarget, text: &str) -> Result<()> {
        self.run(&TmuxCommand::SendKeys {
            target: target.clone(),
            keys: text.to_string(),
            literal: true,
        })?;
        Ok(())
    }

    /// Send a Ctrl-C interrupt to a session/pane.
    ///
    /// Why: restarting Claude Code in place means interrupting the running
    /// process before relaunching it; `C-c` is the clean stop.
    /// What: one `send-keys` invocation with the `C-c` key name (non-literal).
    /// Test: `core::tmux::send_keys_keyname_argv` covers the argv shape.
    pub fn send_interrupt(&self, target: &TmuxTarget) -> Result<()> {
        self.run(&TmuxCommand::SendKeys {
            target: target.clone(),
            keys: "C-c".to_string(),
            literal: false,
        })?;
        Ok(())
    }

    /// Capture the last `lines` of a pane's output (whole scrollback if `None`).
    pub fn capture(&self, target: &TmuxTarget, lines: Option<u32>) -> Result<String> {
        self.run(&TmuxCommand::CapturePane {
            target: target.clone(),
            lines,
        })
    }

    /// Durably publish `key=value` into the tmux SESSION environment (#2157 item 1).
    ///
    /// Why: `runtime::claude_code::session_id_export_prefix` exports
    /// `TM_MANAGED_SESSION_ID` into the ONE pane shell that ran the spawn/resume
    /// command line — a sibling pane/window in the same session, or a pane
    /// spawned before this fix landed, never sees it, which is why the
    /// in-place-relaunch gate (`bin/tm/commands/guided_inplace.rs`) could fail
    /// silently and bare `tm` fell through to spawning a nested session
    /// (issue #2157). `set-environment` writes into the session's own
    /// environment table, queryable via `tmux show-environment` from ANY
    /// pane/shell in that session regardless of vintage.
    /// What: runs `tmux set-environment -t <session> <key> <value>`; maps a
    /// non-zero exit to `Error::Protocol`. Callers treat this as best-effort —
    /// the shell-export prefix remains the primary mechanism.
    /// Test: argv shape covered by `core::tmux::set_environment_argv`.
    pub fn set_environment(&self, session: &str, key: &str, value: &str) -> Result<()> {
        self.run(&TmuxCommand::SetEnvironment {
            session: session.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        })?;
        Ok(())
    }

    /// List every pane on the host as `session_name pane_current_command`.
    ///
    /// Why: session auto-discovery scans all panes to find ones running Claude
    /// Code; `list-panes -a` reports every pane across every session in one
    /// call, so the daemon need not iterate sessions itself.
    /// What: runs `tmux list-panes -a -F "#{session_name} #{pane_current_command}"`
    /// directly (this cross-session form has no `core::tmux::TmuxCommand`
    /// variant) and returns its raw stdout. An empty tmux server (`no server
    /// running`) yields an empty string rather than an error.
    /// Test: parsing of the output is covered by `discovery::parse_pane_line`;
    /// this listing path is exercised by the `discovery` tmux-absent test.
    pub fn list_claude_panes(&self) -> Result<String> {
        let output = Command::new(&self.tmux_path)
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{session_name} #{pane_current_command}",
            ])
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("no server running") || stderr.contains("no sessions") {
                return Ok(String::new());
            }
            return Err(Error::Protocol(format!("tmux list-panes -a: {stderr}")));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Enumerate every pane as a structured [`orphan_gc::PaneInfo`] row.
    ///
    /// Why: the orphan-GC must reconcile live tmux against the registries, and
    /// to decide whether a session is idle it needs each pane's
    /// `pane_current_command` (and, for the belt-and-braces liveness check, the
    /// pane's shell PID). `list-panes -a` reports all of that across every
    /// session in a single tmux call.
    /// What: runs
    /// `tmux list-panes -a -F "#{session_name}\t#{pane_current_command}\t#{pane_pid}"`
    /// and parses each row into a [`orphan_gc::PaneInfo`]. A tab delimiter avoids
    /// colliding with the colons in session names. An empty tmux server (`no
    /// server running`) yields an empty `Vec` rather than an error, so a quiet
    /// host reaps nothing rather than failing the sweep.
    /// Test: row parsing covered by `parses_managed_pane_row`; the live listing
    /// path is exercised by the `#[ignore]` integration test.
    pub fn list_managed_panes(&self) -> Result<Vec<crate::daemon::orphan_gc::PaneInfo>> {
        let output = Command::new(&self.tmux_path)
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{session_name}\t#{pane_current_command}\t#{pane_pid}",
            ])
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("no server running") || stderr.contains("no sessions") {
                return Ok(Vec::new());
            }
            return Err(Error::Protocol(format!("tmux list-panes -a: {stderr}")));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(Self::parse_managed_pane_row)
            .collect())
    }

    /// Parse one `session_name\tpane_current_command\tpane_pid` row.
    ///
    /// Why: keeping the parse separate from the subprocess call makes it
    /// unit-testable without spawning tmux.
    /// What: splits on the tab delimiter; a row missing the session name is
    /// dropped (`None`); a missing or unparsable PID degrades to `None` PID
    /// rather than dropping the whole row (the command is the primary signal).
    /// Test: `parses_managed_pane_row`.
    fn parse_managed_pane_row(line: &str) -> Option<crate::daemon::orphan_gc::PaneInfo> {
        let mut parts = line.splitn(3, '\t');
        let session_name = parts.next().filter(|s| !s.is_empty())?.to_string();
        let pane_current_command = parts.next().unwrap_or("").trim().to_string();
        let pane_pid = parts.next().and_then(|s| s.trim().parse::<u32>().ok());
        Some(crate::daemon::orphan_gc::PaneInfo {
            session_name,
            pane_current_command,
            pane_pid,
        })
    }

    /// List every tmux session, tagged with its [`SessionOrigin`].
    ///
    /// Why: the universal-session dashboard manages *all* tmux sessions, not
    /// just the ones trusty-mpm created; each row must carry its origin so the
    /// UI can offer to adopt external sessions.
    /// What: runs `list-sessions` (same as [`list_sessions`](Self::list_sessions))
    /// and maps each row into an origin-classified [`ExternalSession`]. An empty
    /// tmux server yields an empty `Vec`.
    /// Test: classification covered by `core::external_session` tests; the
    /// listing path is exercised by the `#[ignore]` integration test.
    pub fn list_all_sessions(&self) -> Result<Vec<ExternalSession>> {
        Ok(self
            .list_sessions()?
            .into_iter()
            .map(|s| ExternalSession::new(s.name, s.attached, s.created))
            .collect())
    }

    /// Capture the current state of any session for monitoring.
    ///
    /// Why: trusty-mpm oversees external sessions read-only; the daemon must be
    /// able to snapshot a session's windows, panes, and recent output without
    /// modifying it.
    /// What: runs `list-windows` / `list-panes` / `capture-pane` against `name`
    /// and bundles the results into a [`SessionSnapshot`]. tmux being absent or
    /// the session not existing surfaces as an `Err`.
    /// Test: `#[ignore]` integration test `monitor_session_snapshots_state`.
    pub fn monitor_session(&self, name: &str, lines: u32) -> Result<SessionSnapshot> {
        let windows = self.list_windows(name)?;
        let panes = self.list_panes(name)?;
        let output = self.capture(&TmuxTarget::session(name), Some(lines))?;
        Ok(SessionSnapshot {
            name: name.to_string(),
            windows,
            panes,
            output,
            captured_at: chrono::Utc::now().timestamp(),
        })
    }

    /// Return the pane's current working directory via `display-message`.
    ///
    /// Why: the snapshot-before-stop path (#1816) needs the pane's cwd to
    /// restore it on resume; `tmux display-message -p '#{pane_current_path}'`
    /// is the standard mechanism. The call is best-effort — callers must handle
    /// `None` gracefully (resume falls back to workspace_path/cwd).
    /// What: runs `tmux display-message -t <name> -p '#{pane_current_path}'`,
    /// trims the output, and returns `Some(path)` on success or `None` if the
    /// session does not exist, tmux is unavailable, or the path is empty.
    /// Test: exercised indirectly via `snapshot::capture_into` with a live tmux;
    /// `RealTmuxDriver::get_pane_cwd` wraps this method.
    pub fn pane_current_path(&self, session_name: &str) -> Option<std::path::PathBuf> {
        let output = Command::new(&self.tmux_path)
            .args([
                "display-message",
                "-t",
                session_name,
                "-p",
                "#{pane_current_path}",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let raw = String::from_utf8_lossy(&output.stdout);
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(trimmed))
        }
    }

    /// Register an external session for oversight without modifying it.
    ///
    /// Why: before trusty-mpm watches an externally-created session it records
    /// the session's current shape; adoption is explicitly *non-destructive* —
    /// it never kills, renames, or sends keys to the session.
    /// What: probes the session exists, captures its window/pane lists, and
    /// returns an [`AdoptedSession`] describing it. An unknown session is an
    /// `Err`.
    /// Test: `#[ignore]` integration test `adopt_session_captures_state`.
    pub fn adopt_session(&self, name: &str) -> Result<AdoptedSession> {
        let windows = self.list_windows(name)?;
        let panes = self.list_panes(name)?;
        let origin = crate::core::external_session::SessionOrigin::classify(name);
        Ok(AdoptedSession {
            name: name.to_string(),
            origin,
            windows,
            panes,
            adopted_at: chrono::Utc::now().timestamp(),
        })
    }

    /// List the window `index:name` rows for a session.
    ///
    /// Why: snapshot and adoption both need a session's window list.
    /// What: runs `list-windows -F` and returns each row verbatim.
    /// Test: argv shape covered by `core::tmux::list_windows_argv`.
    fn list_windows(&self, name: &str) -> Result<Vec<String>> {
        let raw = self.run(&TmuxCommand::ListWindows {
            name: name.to_string(),
        })?;
        Ok(raw
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect())
    }

    /// List the pane `id:active` rows for a session.
    ///
    /// Why: snapshot and adoption both need a session's pane list.
    /// What: runs `list-panes -F` and returns each row verbatim.
    /// Test: argv shape covered by `core::tmux::list_panes_argv`.
    fn list_panes(&self, name: &str) -> Result<Vec<String>> {
        let raw = self.run(&TmuxCommand::ListPanes {
            name: name.to_string(),
        })?;
        Ok(raw
            .lines()
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect())
    }
}

/// A non-destructive registration of an external tmux session.
///
/// Why: `POST /tmux/adopt` brings a pre-existing session under trusty-mpm
/// oversight; the response documents what was adopted without implying any
/// modification was made.
/// What: the session name, its classified origin, the window/pane lists at
/// adoption time, and the adoption epoch.
/// Test: `adopt_session_captures_state` (integration, `#[ignore]`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AdoptedSession {
    /// tmux session name.
    pub name: String,
    /// Whether the session is a trusty-mpm session or external.
    pub origin: crate::core::external_session::SessionOrigin,
    /// Window `index:name` rows captured at adoption time.
    pub windows: Vec<String>,
    /// Pane `id:active` rows captured at adoption time.
    pub panes: Vec<String>,
    /// Unix epoch seconds the session was adopted.
    pub adopted_at: i64,
}

/// A point-in-time snapshot of any tmux session's state.
///
/// Why: `GET /tmux/sessions/{name}/snapshot` lets the dashboard inspect any
/// session (internal or external) without attaching to it.
/// What: the session name, its window/pane lists, the captured pane output,
/// and the capture epoch.
/// Test: `monitor_session_snapshots_state` (integration, `#[ignore]`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionSnapshot {
    /// tmux session name.
    pub name: String,
    /// Window `index:name` rows.
    pub windows: Vec<String>,
    /// Pane `id:active` rows.
    pub panes: Vec<String>,
    /// Captured pane output (last `lines` requested).
    pub output: String,
    /// Unix epoch seconds the snapshot was taken.
    pub captured_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_session_row() {
        let info = SessionInfo::parse("trusty-mpm-abc:1700000000:1").unwrap();
        assert_eq!(info.name, "trusty-mpm-abc");
        assert_eq!(info.created, 1_700_000_000);
        assert!(info.attached);

        let detached = SessionInfo::parse("s:1:0").unwrap();
        assert!(!detached.attached);
    }

    #[test]
    fn parses_managed_pane_row() {
        let row = TmuxDriver::parse_managed_pane_row("tmpm-brave-otter\tclaude\t12345").unwrap();
        assert_eq!(row.session_name, "tmpm-brave-otter");
        assert_eq!(row.pane_current_command, "claude");
        assert_eq!(row.pane_pid, Some(12345));

        // Missing/garbage PID degrades to None but keeps the row (command is key).
        let no_pid = TmuxDriver::parse_managed_pane_row("tmpm-x\tzsh\t").unwrap();
        assert_eq!(no_pid.pane_pid, None);
        assert_eq!(no_pid.pane_current_command, "zsh");

        // Empty session name is dropped entirely.
        assert!(TmuxDriver::parse_managed_pane_row("\tzsh\t1").is_none());
    }

    #[test]
    fn rejects_malformed_session_row() {
        assert!(SessionInfo::parse("").is_err());
        assert!(SessionInfo::parse("name:not-a-number:0").is_err());
    }

    #[test]
    fn driver_reports_availability() {
        // Works whether or not tmux is installed: discover() either resolves a
        // path or returns a clean Protocol error — never panics.
        let available = TmuxDriver::is_available();
        if !available {
            assert!(TmuxDriver::discover().is_err());
        }
    }
}
