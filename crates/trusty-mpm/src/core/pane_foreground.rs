//! Whether a pane's harness holds that pane's tty foreground (#10136).
//!
//! Why: text typed into a pane reaches the tty's foreground process group. A
//! `claude` suspended with Ctrl-Z, or started with `&`, still runs under the
//! pane while the shell holds the prompt, so an existence check lets inbound
//! text run as a shell command.
//! What: [`harness_holds_pane`] walks the pane as
//! [`super::process::harness_under_pane`] does, then reads each match's
//! state, process group and tty foreground process group from `ps`.
//! Test: `a_stopped_or_background_harness_does_not_hold_the_pane`,
//! `a_foreground_reading_needs_a_live_state_and_the_tty_group`.

use std::process::Command;

use super::process::{PaneClaude, harness_pids_under_pane};

/// Whether a live harness named `harness` holds pane process `pane_pid`'s tty.
///
/// Why: the pane write gate must refuse a stopped or background harness, and
/// must never read one as `Absent`, which callers act on as "no harness".
/// What: `Present` when some match is not stopped and its process group is
/// its tty's foreground group; `Absent` when the walk proves no match; else
/// `Unknown` — a failed read, or only stopped or background matches.
/// Test: `a_stopped_or_background_harness_does_not_hold_the_pane`,
/// `a_harness_in_a_pane_holds_it_only_in_the_foreground`.
pub fn harness_holds_pane(pane_pid: u32, harness: &str) -> PaneClaude {
    let (pids, unknown) = harness_pids_under_pane(pane_pid, harness);
    if pids.is_empty() {
        return if unknown {
            PaneClaude::Unknown
        } else {
            PaneClaude::Absent
        };
    }
    // #10136: a harness that exists but does not own the tty is not `Present`.
    if pids
        .into_iter()
        .any(|pid| holds_foreground(pid) == Some(true))
    {
        PaneClaude::Present
    } else {
        PaneClaude::Unknown
    }
}

/// `ps -o stat=,pgid=,tpgid= -p <pid>` read through [`parse_foreground`];
/// `None` when `ps` fails or exits non-zero (a dead pid).
fn holds_foreground(pid: u32) -> Option<bool> {
    let out = Command::new("ps")
        .args(["-o", "stat=,pgid=,tpgid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_foreground(&String::from_utf8_lossy(&out.stdout))
}

/// Parse one `ps -o stat=,pgid=,tpgid=` answer.
///
/// What: `None` unless the answer is exactly a state letter-code and two
/// integers.
/// `Some(true)` only for a state other than stopped (`T`, `t`) or dead
/// (`Z`, `X`) whose process group equals a positive tty foreground group;
/// a process with no tty reads `tpgid` 0 on macOS and -1 on Linux.
/// Test: `a_foreground_reading_needs_a_live_state_and_the_tty_group`.
pub(crate) fn parse_foreground(answer: &str) -> Option<bool> {
    let mut fields = answer.split_whitespace();
    let (stat, pgid, tpgid) = (fields.next()?, fields.next()?, fields.next()?);
    if fields.next().is_some() || !stat.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let pgid: i64 = pgid.parse().ok()?;
    let tpgid: i64 = tpgid.parse().ok()?;
    let live = !stat.starts_with(['T', 't', 'Z', 'X']);
    Some(live && tpgid > 0 && pgid == tpgid)
}
