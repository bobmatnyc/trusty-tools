//! Pane-identity reads and kill-by-id for [`TmuxDriver`] (#9004).
//!
//! Why: `tmux.rs` sits at the 500-SLOC cap; a child module can still reach
//! the driver's private binary path and runner.
//! What: [`TmuxDriver::pane_identity_line`], [`TmuxDriver::pane_current_command`]
//! (#9566) and [`TmuxDriver::kill_session_id`].
//! Test: `the_real_driver_reads_a_live_pane_identity`,
//! `live_a_record_on_its_own_server_is_killed_by_session_id`.

use super::TmuxDriver;
use crate::core::tmux::{TmuxCommand, TmuxTarget};
use crate::core::{Error, Result};

impl TmuxDriver {
    /// The raw [`PANE_IDENTITY_FORMAT`] line tmux prints for `pane_id`.
    ///
    /// What: `display-message -t %N -p <format>`; `Err` on a spawn failure, a
    /// non-zero exit, or a `pane_id` that is not a `%N` id. Exit 0 does not
    /// prove the pane exists — the caller parses the line.
    ///
    /// [`PANE_IDENTITY_FORMAT`]: crate::session_manager::pane_identity::PANE_IDENTITY_FORMAT
    pub fn pane_identity_line(&self, pane_id: &str) -> Result<String> {
        self.pane_display_line(
            pane_id,
            crate::session_manager::pane_identity::PANE_IDENTITY_FORMAT,
        )
    }

    /// The process tmux reports in the foreground of pane `pane_id` (#9566).
    ///
    /// What: `display-message -t %N -p '#{pane_current_command}'`, trimmed;
    /// `Err` as [`Self::pane_identity_line`] does, and on an empty answer.
    /// Test: `a_stopped_record_whose_pane_runs_a_live_agent_is_never_reactivated_in_place`.
    pub fn pane_current_command(&self, pane_id: &str) -> Result<String> {
        let line = self.pane_display_line(pane_id, "#{pane_current_command}")?;
        let command = line.trim();
        if command.is_empty() {
            return Err(Error::Protocol(format!(
                "tmux reported no foreground command for pane {pane_id}"
            )));
        }
        Ok(command.to_owned())
    }

    /// The pid of pane `pane_id`'s own process, its shell (#10127).
    ///
    /// What: `display-message -t %N -p '#{pane_pid}'`, parsed; `Err` as
    /// [`Self::pane_identity_line`] does, and on an unparsable answer.
    pub fn pane_pid(&self, pane_id: &str) -> Result<u32> {
        let line = self.pane_display_line(pane_id, "#{pane_pid}")?;
        line.trim().parse().map_err(|_| {
            Error::Protocol(format!("tmux reported no pid for pane {pane_id}: {line:?}"))
        })
    }

    /// Every pane of session `name`, as the runtime reaper reads panes (#8241).
    ///
    /// What: `list-panes -s -t =name: -F '#{pane_id} #{pane_pid}
    /// #{pane_current_command}'`, parsed by [`parse_session_panes`]. `Err` on a
    /// spawn failure, a non-zero exit, or a listing that does not parse whole.
    /// Test: `parse_session_panes_accepts_only_a_whole_listing`.
    pub fn session_panes(&self, name: &str) -> Result<Vec<crate::daemon::orphan_gc::PaneInfo>> {
        let argv = [
            "list-panes".to_owned(),
            "-s".to_owned(),
            "-t".to_owned(),
            crate::core::tmux::exact_window_target(name),
            "-F".to_owned(),
            SESSION_PANES_FORMAT.to_owned(),
        ];
        let output = crate::core::tmux::run_tmux_argv_with_bin(&self.tmux_path, &argv)?;
        if !output.status.success() {
            return Err(Error::Protocol(format!(
                "tmux list-panes for session '{name}' failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        parse_session_panes(name, &String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
            Error::Protocol(format!(
                "tmux list-panes for session '{name}' returned an unparsable listing"
            ))
        })
    }

    /// `display-message -t %N -p <format>` for a `%N` `pane_id`, raw.
    fn pane_display_line(&self, pane_id: &str, format: &str) -> Result<String> {
        if !(pane_id.starts_with('%') && trusty_common::tmux::is_immutable_id(pane_id)) {
            return Err(Error::Protocol(format!(
                "{pane_id:?} is not a tmux pane id"
            )));
        }
        // #9004: a pane id is exact without a session, so the session part of
        // the target is never rendered.
        let target = TmuxTarget::pane("", pane_id);
        let argv = crate::core::tmux::display_message_argv(Some(&target), format);
        let output = crate::core::tmux::run_tmux_argv_with_bin(&self.tmux_path, &argv)?;
        if !output.status.success() {
            return Err(Error::Protocol(format!(
                "tmux display-message for pane {pane_id} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Kill the session with `$N` id `session_id`, which carries `name`.
    ///
    /// What: the #8942 floor answers for `name` first, as in
    /// [`Self::kill_session`]; then `kill-session -t $N`. A `session_id` that
    /// is not a `$N` id is refused before tmux runs.
    #[track_caller]
    pub fn kill_session_id(&self, name: &str, session_id: &str) -> Result<()> {
        let caller = format!(
            "TmuxDriver::kill_session_id from {}",
            std::panic::Location::caller()
        );
        if let Some(why) = self.floor.refuse(name, &caller) {
            return Err(Error::Protocol(why));
        }
        if !(session_id.starts_with('$') && trusty_common::tmux::is_immutable_id(session_id)) {
            return Err(Error::Protocol(format!(
                "{session_id:?} is not a tmux session id; '{name}' was not killed"
            )));
        }
        // #9004: `exact_session_target` passes a `$N` id through unchanged.
        self.run(&TmuxCommand::KillSession {
            name: session_id.to_owned(),
        })?;
        Ok(())
    }
}

/// The `list-panes` format [`TmuxDriver::session_panes`] asks for (#8241).
///
/// Space-separated, command last: tmux rewrites a TAB to `_` for a client
/// without a UTF-8 locale (#6529), and a command name may hold spaces.
const SESSION_PANES_FORMAT: &str = "#{pane_id} #{pane_pid} #{pane_current_command}";

/// Parse a [`SESSION_PANES_FORMAT`] listing of session `name` (#8241).
///
/// Why: the listing decides whether a stopped record may read idle, and an
/// idle answer lets a resume start a runtime in that pane. A row this parser
/// skipped could be the one pane that still runs a runtime.
/// What: one [`PaneInfo`](crate::daemon::orphan_gc::PaneInfo) per row, with
/// `session_name` set to `name` (the exact `=name:` target proves membership).
/// `None` for an empty listing or any row without a `%N` id, a numeric pid and
/// a non-empty command. Blank lines are skipped.
/// Test: `parse_session_panes_accepts_only_a_whole_listing`.
pub(crate) fn parse_session_panes(
    name: &str,
    stdout: &str,
) -> Option<Vec<crate::daemon::orphan_gc::PaneInfo>> {
    let mut panes = Vec::new();
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let mut fields = line.trim().splitn(3, ' ');
        let pane_id = fields.next()?;
        let pane_pid = fields.next()?.parse::<u32>().ok()?;
        let command = fields.next()?.trim();
        if !pane_id.starts_with('%') || command.is_empty() {
            return None;
        }
        panes.push(crate::daemon::orphan_gc::PaneInfo {
            session_name: name.to_owned(),
            pane_current_command: command.to_owned(),
            pane_pid: Some(pane_pid),
            pane_id: Some(pane_id.to_owned()),
            pane_current_path: None,
        });
    }
    (!panes.is_empty()).then_some(panes)
}
