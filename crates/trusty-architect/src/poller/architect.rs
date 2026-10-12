//! Whether the Architect's own turn is running: `fleet-poll.py`'s
//! `architect_busy` (988152c lines 1385-1405).
//!
//! Why: the watchdog's frozen check needs "a turn is running" and the spinner
//! text, which only the pane shows. Ported now by Architect ruling Q3 (#9171
//! A3b-2); its consumer, the watchdog, is #9779.
//! What: [`architect_busy`] captures the first non-shell `tm-architect` pane
//! (read-only, no keys) and returns `(busy, status)` from `parse_pane`; a
//! finished turn (`· done `) is idle even with a live monitor row. No such
//! pane is `(false, "")`. [`is_shell`] is the prototype's shell-pane test.
//! Test: `test_architect_busy_reads_spinner`,
//! `test_architect_busy_completed_turn_is_idle`,
//! `architect_busy_capture_failure_is_an_error`.

use super::SUPERVISOR;
use super::listing::{Pane, capture};
use crate::classify::parse_pane;
use crate::error::TmuxError;
use crate::tmux::Tmux;

/// Commands that mean a pane runs a shell, not Claude Code.
pub const SHELLS: [&str; 9] = [
    "bash", "zsh", "sh", "fish", "dash", "ksh", "tcsh", "csh", "login",
];

/// The finished-turn summary: `✻ Brewed for 9s · done 7:14 AM`.
pub const DONE_SUMMARY: &str = "· done ";

/// True when `cmd` is a shell (a login shell shows as `-zsh`).
pub fn is_shell(cmd: &str) -> bool {
    SHELLS.contains(&cmd) || cmd.starts_with('-')
}

/// `(busy, status line)` of the Architect's live pane; `(false, "")` when
/// there is none.
///
/// # Errors
/// [`TmuxError`] when the capture fails. The prototype logs that and reads
/// `(False, "")`; the port leaves the reading to the caller, so a failed read
/// is never taken for an idle Architect by accident.
pub fn architect_busy<T: Tmux + ?Sized>(
    tmux: &T,
    panes: &[Pane],
) -> Result<(bool, String), TmuxError> {
    let Some(p) = panes
        .iter()
        .find(|p| p.session == SUPERVISOR && !is_shell(&p.cmd))
    else {
        return Ok((false, String::new()));
    };
    let info = parse_pane(&capture(tmux, &p.pane)?);
    if info.status.contains(DONE_SUMMARY) {
        // A completed turn at the prompt; a live monitor row is not a turn.
        return Ok((false, String::new()));
    }
    Ok((info.busy, info.status))
}
