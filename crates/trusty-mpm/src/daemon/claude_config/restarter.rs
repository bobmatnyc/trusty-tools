//! [`ClaudeCodeRestarter`] — find and restart running Claude Code processes.
//!
//! Why: after applying config changes the operator wants Claude Code to pick
//! them up; this drives the restart.
//! What: `find_claude_processes` lists `claude` PIDs via `pgrep`;
//! `restart_in_session` sends Ctrl-C then `claude` into a tmux session's pane.
//! Test: `find_claude_processes_does_not_panic`; `restart_target_*` (below)
//! cover the pane-vs-session target decision without a live tmux binary.

use std::process::Command;

use crate::core::Error;
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

    /// Restart Claude Code inside a named tmux session, optionally pinned to
    /// a specific recorded pane.
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
    /// panes created after the option lands benefit. `pane_id` closes the
    /// same sibling-window hijack risk #2467 fixed for resume/restart respawn
    /// (issue #2468, dashboard restart route site): addressing "the session"
    /// alone lets tmux resolve the target to whichever pane is currently
    /// ACTIVE, possibly a sibling window with nothing to do with this record.
    /// What: discovers tmux, re-applies the scrollback/mouse options,
    /// resolves the tmux target via [`restart_target`] (pane-scoped when
    /// `pane_id` is known and confirmed alive; session-scoped for a legacy
    /// caller with no `pane_id`; a loud `Err` — the session-scoped target is
    /// NEVER constructed — when `pane_id` is known but confirmed gone), sends
    /// `C-c`, waits briefly for the process to exit, then types `claude` +
    /// Enter. tmux being absent surfaces as an `Err`.
    /// Test: `find_claude_processes_does_not_panic`; the pane/session target
    /// DECISION is unit-tested (no live tmux needed) via `restart_target_*`.
    pub fn restart_in_session(tmux_session: &str, pane_id: Option<&str>) -> Result<()> {
        let driver = crate::daemon::tmux::TmuxDriver::discover()?;
        driver.apply_scrollback_options();
        let pane_alive = pane_id.is_some_and(|p| driver.pane_exists(tmux_session, p));
        let target = restart_target(tmux_session, pane_id, pane_alive)?;
        // Interrupt the running Claude Code process.
        driver.send_interrupt(&target)?;
        std::thread::sleep(std::time::Duration::from_millis(500));
        // Relaunch Claude Code.
        driver.send_line(&target, crate::daemon::spawn_command::relaunch_command())?;
        Ok(())
    }
}

/// Decide which tmux target [`ClaudeCodeRestarter::restart_in_session`]
/// should address for a given recorded `pane_id` — pure decision logic,
/// unit-testable without a live tmux binary (issue #2468).
///
/// Why: mirrors the refusal semantics `SessionManager::resume`
/// ([`crate::session_manager::manager::ManagedError::PaneGone`]) established
/// (#2467) for the identical sibling-window hijack risk, so the dashboard
/// restart route fails loudly instead of guessing when the recorded pane is
/// gone.
/// What: `pane_id = None` (legacy caller, no recorded pane) → session-scoped
/// target, matching #2467's legacy fallback. `Some(pane_id)` with
/// `pane_alive == true` → pane-scoped target. `Some(pane_id)` with
/// `pane_alive == false` → `Err` naming the session and pane; the
/// session-scoped target is NEVER constructed in this branch.
/// Test: `restart_target_uses_pane_when_alive`,
/// `restart_target_refuses_when_pane_gone`,
/// `restart_target_falls_back_to_session_for_legacy_caller`.
fn restart_target(
    tmux_session: &str,
    pane_id: Option<&str>,
    pane_alive: bool,
) -> Result<TmuxTarget> {
    match pane_id {
        Some(p) if pane_alive => Ok(TmuxTarget::pane(tmux_session, p)),
        Some(p) => Err(Error::Protocol(format!(
            "recorded pane {p} for session {tmux_session} no longer exists; refusing to \
             restart into an unrelated active pane"
        ))),
        None => Ok(TmuxTarget::session(tmux_session)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_target_uses_pane_when_alive() {
        let target = restart_target("trusty-mpm-xyz", Some("%6015"), true).expect("pane target");
        assert_eq!(target.as_target(), "%6015");
    }

    #[test]
    fn restart_target_refuses_when_pane_gone() {
        let err = restart_target("trusty-mpm-xyz", Some("%6015"), false)
            .expect_err("a confirmed-gone recorded pane must refuse, not fall back");
        let msg = err.to_string();
        assert!(msg.contains("%6015"), "error must name the pane: {msg}");
        assert!(
            msg.contains("trusty-mpm-xyz"),
            "error must name the session: {msg}"
        );
    }

    #[test]
    fn restart_target_falls_back_to_session_for_legacy_caller() {
        let target = restart_target("trusty-mpm-xyz", None, false).expect("session target");
        assert_eq!(target.as_target(), "trusty-mpm-xyz");
    }
}
