//! [`ClaudeCodeRestarter`] — find and restart running Claude Code processes.
//!
//! Why: after applying config changes the operator wants Claude Code to pick
//! them up; this drives the restart.
//! What: `find_claude_processes` lists `claude` PIDs via `pgrep`;
//! `restart_in_session` sends Ctrl-C then `claude` into a tmux session's pane.
//! Test: `find_claude_processes_does_not_panic`.

use std::process::Command;

use crate::core::Result;
use crate::core::tmux::TmuxTarget;

/// Finds and restarts running Claude Code processes.
///
/// Why: after applying config changes the operator wants Claude Code to pick
/// them up; this drives the restart.
/// What: `find_claude_processes` lists `claude` PIDs via `pgrep`;
/// `restart_in_session` sends Ctrl-C then `claude` into a tmux session's pane.
/// Test: `find_claude_processes_does_not_panic` (the PID list may be empty).
pub struct ClaudeCodeRestarter;

impl ClaudeCodeRestarter {
    /// List the PIDs of running `claude` processes.
    ///
    /// Why: the dashboard shows whether Claude Code is running and how many
    /// instances; the restart flow can also use it to confirm a target exists.
    /// What: runs `pgrep -x claude`; a non-zero exit (no matches) or a missing
    /// `pgrep` both yield an empty `Vec` rather than an error.
    /// Test: `find_claude_processes_does_not_panic`.
    pub fn find_claude_processes() -> Vec<u32> {
        let output = match Command::new("pgrep").args(["-x", "claude"]).output() {
            Ok(out) => out,
            Err(e) => {
                tracing::info!("pgrep unavailable: {e}; reporting no claude processes");
                return Vec::new();
            }
        };
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .collect()
    }

    /// Restart Claude Code inside a named tmux session.
    ///
    /// Why: a Claude Code session hosted in tmux is restarted in place — send
    /// an interrupt to stop the current process, then relaunch `claude`. The
    /// scrollback/mouse server options (#2398) are re-applied defensively
    /// before the relaunch: this is the "restarter/relaunch-on-exit" path, so
    /// re-applying here guards the edge case where the tmux server was
    /// started or restarted independently of trusty-mpm (e.g. `tmux
    /// kill-server` followed by an operator's own `tmux new-session`) since
    /// this session's pane was created. `set-option -g` is idempotent and
    /// best-effort (never fails the restart) — see
    /// [`crate::daemon::tmux::TmuxDriver::apply_scrollback_options`]. It does
    /// NOT retroactively grow THIS session's already-created pane; only
    /// panes created after the option lands benefit.
    /// What: discovers tmux, re-applies the scrollback/mouse options, sends
    /// `C-c` to the session's pane, waits briefly for the process to exit,
    /// then types `claude` + Enter. tmux being absent surfaces as an `Err`.
    /// Test: `restart_in_session_errors_without_tmux` (skipped when tmux is
    /// installed).
    pub fn restart_in_session(tmux_session: &str) -> Result<()> {
        let driver = crate::daemon::tmux::TmuxDriver::discover()?;
        driver.apply_scrollback_options();
        let target = TmuxTarget::session(tmux_session);
        // Interrupt the running Claude Code process.
        driver.send_interrupt(&target)?;
        std::thread::sleep(std::time::Duration::from_millis(500));
        // Relaunch Claude Code.
        driver.send_line(&target, crate::daemon::spawn_command::relaunch_command())?;
        Ok(())
    }
}
