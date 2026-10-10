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
