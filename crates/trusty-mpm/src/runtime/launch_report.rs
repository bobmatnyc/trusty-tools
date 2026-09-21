//! What the pane prints once a managed `claude` exits (#6766, relocated #8233).
//!
//! Why: #2023 component D appended `; echo '<hint>'` to every managed launch
//! command, so a `claude` that REFUSED the launch and quit before drawing a
//! frame printed the same "run `tm` to relaunch this session" line as a session
//! the operator worked in for an hour. #6766 replaced that with a three-way
//! branch on the exit status and the elapsed time.
//!
//! That branch used to be a `if`/`elif`/`$(( ))` shell fragment appended to the
//! typed pane command, which cost ~330 bytes of the line whose length is exactly
//! what #8233 is about. The BEHAVIOUR is unchanged and the wording is
//! byte-identical; only the evaluator moved. `tm internal-spawn-disclaimed
//! --launch-spec` is the process that waits for `claude`, so it has the real
//! exit status and the real elapsed time — strictly better inputs than
//! `$?` and `$(date +%s)` arithmetic in a shell that may have swallowed the
//! clock stamp.
//!
//! Three outcomes, unchanged from #6766:
//!
//!   * non-zero status → [`FAILED_HINT_HEAD`] + the status + [`FAILED_HINT_TAIL`]
//!   * status 0 within [`LAUNCH_FLOOR_SECS`] → [`REFUSED_HINT`]
//!   * anything else → [`RELAUNCH_HINT`]
//!
//! **Why elapsed time and not the refusal text.** The refusal that motivated
//! this exits 0 — status alone cannot see it. #6766 weighed matching Claude
//! Code's own wording and rejected it: that string is an undocumented UI surface
//! a reworded release would silently take the detection with it. Elapsed time
//! couples to nothing Anthropic owns. It costs a false REFUSED line for an
//! operator who starts `claude` and quits inside [`LAUNCH_FLOOR_SECS`], which is
//! a wrong sentence in a pane, not a wrong state anywhere.
//!
//! Test: `report_names_a_nonzero_status`, `report_calls_an_immediate_zero_exit_refused`,
//! `report_keeps_the_relaunch_hint_for_a_real_session`,
//! `report_treats_a_signal_death_as_a_failure` in this file's `tests` module.

use std::time::Duration;

/// The one-line hint printed after a `claude` that genuinely ran (#2023 D).
///
/// Why: the pane stays alive as a bare shell when the runtime exits, and a bare
/// `tm` run from inside it relaunches the same session in place (#2023 C) — but
/// neither is discoverable unless the pane says so.
/// What: a short, single-line string, unchanged from #6766.
/// Test: `report_keeps_the_relaunch_hint_for_a_real_session`.
pub const RELAUNCH_HINT: &str = "tm: run `tm` to relaunch this session";

/// Printed when `claude` exited 0 but faster than a session can start (#6766).
///
/// Why: this is the refusal case — Claude Code declined the launch, said why on
/// its own last line, and exited successfully. The operator needs the pane to
/// name that as a failed relaunch and point at the message immediately above.
/// Test: `report_calls_an_immediate_zero_exit_refused`.
pub const REFUSED_HINT: &str = "tm: claude exited immediately without starting a session — \
                                this launch was refused (its own message is above); \
                                run `tm` to retry";

/// Leading half of the non-zero-status line; the status sits between this and
/// [`FAILED_HINT_TAIL`] (#6766).
///
/// Test: `report_names_a_nonzero_status`.
pub const FAILED_HINT_HEAD: &str = "tm: claude exited with status";

/// Trailing half of the non-zero-status line (see [`FAILED_HINT_HEAD`]).
pub const FAILED_HINT_TAIL: &str = "— the session did not start; run `tm` to retry";

/// Seconds under which a status-0 exit reads as a refusal, not a finished
/// session (#6766).
///
/// Why: Claude Code's background-session refusal prints and exits well inside a
/// second; a session an operator actually worked in lasts minutes. The value is
/// high enough to absorb a slow cold start that fails, low enough that the only
/// false positive is an operator who quits `claude` immediately.
/// Test: both `report_calls_an_immediate_zero_exit_refused` and
/// `report_keeps_the_relaunch_hint_for_a_real_session` straddle it.
pub const LAUNCH_FLOOR_SECS: u64 = 5;

/// Report how a managed `claude` exited, as the one line the pane prints.
///
/// Why: see this module's header — a refused relaunch must not read as a
/// finished session.
/// What: `code` is the child's exit code (`None` when a signal killed it, which
/// is a failure like any other non-zero status and is reported as 127, the value
/// a shell reports for the same case). `elapsed` is how long the child ran.
/// Returns the exact line, with no trailing newline.
/// Test: the four `report_*` tests below.
pub fn report_exit(code: Option<i32>, elapsed: Duration) -> String {
    let status = code.unwrap_or(127);
    if status != 0 {
        return format!("{FAILED_HINT_HEAD} {status} {FAILED_HINT_TAIL}");
    }
    if elapsed.as_secs() < LAUNCH_FLOOR_SECS {
        return REFUSED_HINT.to_owned();
    }
    RELAUNCH_HINT.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_names_a_nonzero_status() {
        // #6766: a claude that failed outright must say so — and must NOT print
        // the relaunch hint, which is the whole defect.
        let printed = report_exit(Some(3), Duration::from_secs(0));
        assert!(
            printed.contains(FAILED_HINT_HEAD) && printed.contains(" 3 "),
            "a non-zero exit must name its status, got {printed:?}"
        );
        assert!(
            !printed.contains(RELAUNCH_HINT),
            "a non-zero exit must not print the clean-exit relaunch hint, got {printed:?}"
        );
    }

    #[test]
    fn report_calls_an_immediate_zero_exit_refused() {
        // #6766 / #6765: the observed refusal exits 0. Status alone cannot see
        // it; the elapsed floor must.
        let printed = report_exit(Some(0), Duration::from_secs(0));
        assert_eq!(printed, REFUSED_HINT);
    }

    #[test]
    fn report_keeps_the_relaunch_hint_for_a_real_session() {
        // The #2023 component D behaviour survives for the case it was written
        // for: claude ran, the operator exited it, the pane advertises `tm`.
        let printed = report_exit(Some(0), Duration::from_secs(LAUNCH_FLOOR_SECS + 60));
        assert_eq!(printed, RELAUNCH_HINT);
    }

    #[test]
    fn report_treats_a_signal_death_as_a_failure() {
        // A signal leaves no exit code; the shell form saw 128+N (non-zero) and
        // took the failure branch, so this must too — never the relaunch hint.
        let printed = report_exit(None, Duration::from_secs(600));
        assert!(
            printed.contains(FAILED_HINT_HEAD) && !printed.contains(RELAUNCH_HINT),
            "a signal death must take the failure branch, got {printed:?}"
        );
    }
}
