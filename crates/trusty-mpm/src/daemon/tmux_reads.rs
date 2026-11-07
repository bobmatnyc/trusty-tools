//! The server-aware argv builder of [`TmuxDriver`] and its listing reads
//! (#10101).
//!
//! Why: `tmux.rs` sits at the 500-SLOC cap. The reads that spawn tmux with
//! their own argv (no [`TmuxCommand`] variant models them) are the calls that
//! could miss the `-L` prefix, so they live beside the one builder they use.
//! What: [`TmuxDriver::server_argv`], and [`TmuxDriver::list_sessions`],
//! [`TmuxDriver::list_claude_panes`], [`TmuxDriver::list_managed_panes`],
//! [`TmuxDriver::pane_current_path`] and [`TmuxDriver::pane_id`].
//! Test: the `*_reaches_the_named_server` tests in `tmux_tests.rs`.

use std::process::Command;

use super::{SessionInfo, TmuxDriver, stderr_means_empty_server};
use crate::core::tmux::{TmuxCommand, tmux_argv};
use crate::core::{Error, Result};

impl TmuxDriver {
    /// The whole argv for `command` on this driver's server (#10101).
    ///
    /// Why: every tmux call this driver makes must reach the server it was
    /// built for; a method that assembled its own argv could reach the default
    /// server instead, where the Architect's pane lives.
    /// What: [`crate::core::tmux_server::server_argv`] with `self.server` —
    /// `-L <name>` first when the driver is [`Self::on_server`], otherwise
    /// `command` unchanged (the pre-#10101 argv, byte for byte).
    /// Test: `list_sessions_reaches_the_named_server` and the other
    /// `*_reaches_the_named_server` tests in `tmux_tests.rs`.
    pub(crate) fn server_argv(&self, command: impl IntoIterator<Item = String>) -> Vec<String> {
        crate::core::tmux_server::server_argv(self.server.as_deref(), command)
    }

    /// A `Command` for `command` on this driver's server (#10101).
    fn tmux_command<S: Into<String>>(&self, command: impl IntoIterator<Item = S>) -> Command {
        let mut cmd = Command::new(&self.tmux_path);
        cmd.args(self.server_argv(command.into_iter().map(Into::into)));
        cmd
    }

    /// List all tmux sessions on this driver's server.
    ///
    /// Why: the multi-session dashboard enumerates every running session.
    /// What: runs `list-sessions`; tmux exits non-zero with "no server running"
    /// when there are zero sessions — that is mapped to an empty `Vec`.
    /// Test: row parsing covered by `parses_session_row`;
    /// `list_sessions_reaches_the_named_server`.
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>> {
        let output = self
            .tmux_command(tmux_argv(&TmuxCommand::ListSessions))
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr_means_empty_server(&stderr) {
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

    /// List every pane on the server as `session_name pane_current_command`.
    ///
    /// Why: session auto-discovery scans all panes to find ones running Claude
    /// Code; `list-panes -a` reports every pane across every session in one
    /// call, so the daemon need not iterate sessions itself.
    /// What: runs `tmux list-panes -a -F "#{session_name} #{pane_current_command}"`
    /// directly (this cross-session form has no `core::tmux::TmuxCommand`
    /// variant) and returns its raw stdout. An empty tmux server (`no server
    /// running`) yields an empty string rather than an error.
    /// Test: parsing of the output is covered by `discovery::parse_pane_line`;
    /// `list_claude_panes_reaches_the_named_server`.
    pub fn list_claude_panes(&self) -> Result<String> {
        let output = self
            .tmux_command([
                "list-panes",
                "-a",
                "-F",
                "#{session_name} #{pane_current_command}",
            ])
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr_means_empty_server(&stderr) {
                return Ok(String::new());
            }
            return Err(Error::Protocol(format!("tmux list-panes -a: {stderr}")));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Enumerate every pane as a structured [`orphan_gc::PaneInfo`](crate::daemon::orphan_gc::PaneInfo) row.
    ///
    /// Why: the orphan-GC must reconcile live tmux against the registries, and
    /// to decide whether a session is idle it needs each pane's
    /// `pane_current_command` (and, for the belt-and-braces liveness check, the
    /// pane's shell PID). The reactivate-reconcile path (#2789) additionally
    /// needs each pane's stable `pane_id` to identify the caller's OWN pane and
    /// treat it as idle. `list-panes -a` reports all of that across every
    /// session in a single tmux call.
    /// What: runs
    /// `tmux list-panes -a -F "#{session_name}\t#{pane_current_command}\t#{pane_pid}\t#{pane_id}"`
    /// and parses each row into a [`orphan_gc::PaneInfo`](crate::daemon::orphan_gc::PaneInfo). A tab delimiter avoids
    /// colliding with the colons in session names. An empty tmux server (`no
    /// server running`) yields an empty `Vec` rather than an error, so a quiet
    /// host reaps nothing rather than failing the sweep.
    ///
    /// The spawn goes through [`crate::core::tmux::force_utf8_client_env`]
    /// because tmux deletes this format's delimiters from its own output when
    /// the client does not declare a UTF-8 locale — see that function and
    /// #6529. A row that still arrives malformed is DROPPED, never coerced, and
    /// the drop is reported once per listing rather than once per row.
    /// Test: row parsing covered by `parses_managed_pane_row`,
    /// `sanitized_pane_row_is_dropped_not_coerced`;
    /// `list_managed_panes_reaches_the_named_server`.
    pub fn list_managed_panes(&self) -> Result<Vec<crate::daemon::orphan_gc::PaneInfo>> {
        let mut cmd = self.tmux_command([
            "list-panes",
            "-a",
            "-F",
            // #6118: `pane_current_path` is the evidence the cwd-gone reap path
            // needs. Asking for it in the SAME listing keeps the sweep at one
            // tmux call rather than one per untracked pane.
            "#{session_name}\t#{pane_current_command}\t#{pane_pid}\t#{pane_id}\t#{pane_current_path}",
        ]);
        // #6529: without a UTF-8 client locale tmux rewrites every tab to `_`.
        crate::core::tmux::force_utf8_client_env(&mut cmd);
        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr_means_empty_server(&stderr) {
                return Ok(Vec::new());
            }
            return Err(Error::Protocol(format!("tmux list-panes -a: {stderr}")));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(Self::parse_managed_pane_rows(&stdout))
    }

    /// Return the pane's current working directory via `display-message`.
    ///
    /// Why: the snapshot-before-stop path (#1816) needs the pane's cwd to
    /// restore it on resume; `tmux display-message -p '#{pane_current_path}'`
    /// is the standard mechanism. The call is best-effort — callers must handle
    /// `None` gracefully (resume falls back to workspace_path/cwd).
    /// What: [`Self::session_display_value`] for `#{pane_current_path}`.
    /// Test: `pane_current_path_reaches_the_named_server`;
    /// `RealTmuxDriver::get_pane_cwd` wraps this method.
    pub fn pane_current_path(&self, session_name: &str) -> Option<std::path::PathBuf> {
        self.session_display_value(session_name, "#{pane_current_path}")
            .map(std::path::PathBuf::from)
    }

    /// Return the pane's stable tmux `pane_id` (e.g. `"%5"`) via `display-message`.
    ///
    /// Why: `#2453` review finding 1 (round 2) — pane-identity confirmation for
    /// the bare-`tm` in-pane relaunch's nested-session guard needs a signal
    /// that is NEVER inherited across panes, unlike a tmux session-scoped env
    /// var (see [`crate::session_manager::record::SessionRecord::pane_id`]).
    /// tmux's own `pane_id` (distinct from `pane_pid`, which the OS can reuse
    /// across a pane's lifetime) is that signal.
    /// What: [`Self::session_display_value`] for `#{pane_id}`.
    /// Test: `pane_id_reaches_the_named_server`;
    /// `RealTmuxDriver::get_pane_id` wraps this method.
    pub fn pane_id(&self, session_name: &str) -> Option<String> {
        self.session_display_value(session_name, "#{pane_id}")
    }

    /// `display-message -t =<name>: -p <format>`, trimmed; `None` when the name
    /// is empty, the spawn fails, tmux exits non-zero, or the value is empty.
    fn session_display_value(&self, session_name: &str, format: &str) -> Option<String> {
        // #8443: an empty name would render a target matching no pane; say so.
        crate::core::tmux::check_session_name(session_name).ok()?;
        let output = self
            .tmux_command([
                "display-message".to_owned(),
                "-t".to_owned(),
                // #8443: pane-typed target — `=name:`, never a bare name.
                crate::core::tmux::exact_window_target(session_name),
                "-p".to_owned(),
                format.to_owned(),
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let raw = String::from_utf8_lossy(&output.stdout);
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }
}
