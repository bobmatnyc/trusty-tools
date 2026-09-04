//! Status-branched on-exit reporting for a managed Claude Code pane (#6766).
//!
//! Why: #2023 component D appended `; echo '<hint>'` to every managed
//! launch/resume command. `;` sequences unconditionally, so the pane printed
//! the same "run `tm` to relaunch this session" line whether `claude` ran for
//! an hour and the operator exited it, or whether `claude` refused the launch
//! and quit before it drew a frame. The observed #6765 transcript is exactly
//! that collision:
//!
//! ```text
//! Your most recent conversation is running in the background (session 768e…).
//! tm: run `tm` to relaunch this session
//! ```
//!
//! Nothing in those two lines says the relaunch failed, so a refused relaunch
//! reads as a completed session. #6765 removed the one KNOWN cause of that
//! refusal; this module makes the pane report the refusal itself, for that
//! cause and any future one.
//!
//! What: two fragments that bracket the `claude` invocation. [`launch_clock_prefix`]
//! stamps a start second into the pane shell before it; [`exit_dispatch_suffix`]
//! replaces the unconditional `echo` after it with a three-way branch:
//!
//!   * non-zero status → [`FAILED_HINT_HEAD`] + the status + [`FAILED_HINT_TAIL`]
//!   * status 0 within [`LAUNCH_FLOOR_SECS`] of the stamp → [`REFUSED_HINT`]
//!   * anything else → the original [`RELAUNCH_HINT`]
//!
//! **Why elapsed time and not the refusal text.** The refusal that motivated
//! this exits 0 — status alone cannot see it. #6766 weighs matching Claude
//! Code's own wording and rejects it: that string is an undocumented UI
//! surface, and a reworded release would silently take the detection with it.
//! Elapsed time couples to nothing Anthropic owns. It costs a false REFUSED
//! line for an operator who starts `claude` and quits inside
//! [`LAUNCH_FLOOR_SECS`], which is a wrong sentence in a pane, not a wrong
//! state anywhere.
//!
//! **The branch fails open to the old hint.** `${__tm_t0:-0}` makes a missing
//! stamp read as the epoch, so the elapsed test cannot fire and the pane prints
//! exactly what it printed before this module existed. A pane shell that
//! swallowed the prefix therefore degrades to #2023 D, never to a false
//! refusal.
//!
//! Shell surface: `if`/`elif`/`[ ]`/`$(( ))`/`$(date +%s)`, all POSIX. The
//! command these fragments join already required a POSIX-ish shell (`export
//! X=v`, `{ …; }`, `&&`), so this adds no new assumption.
//!
//! Test: `exit_dispatch_reports_a_nonzero_status`,
//! `exit_dispatch_reports_an_immediate_zero_exit_as_refused`,
//! `exit_dispatch_keeps_the_relaunch_hint_for_a_real_session`,
//! `exit_dispatch_falls_back_to_the_hint_without_a_clock`,
//! `launch_clock_prefix_stamps_a_second` in this file's `tests` module — each
//! runs the composed fragment through `/bin/sh` and reads the pane's actual
//! output rather than asserting on the command string.

use super::shell_single_quote;

/// The one-line hint printed to the pane after a `claude` that genuinely ran
/// (#2023 component D).
///
/// Why: component A leaves the pane alive as a bare shell when the runtime
/// exits, and component C lets a bare `tm` run from inside that pane relaunch
/// the same session in place — but neither is discoverable unless the pane
/// itself says so. Backticks around the literal command name have no special
/// meaning inside the single-quoted `echo` argument this is embedded in.
/// What: a short, single-line, non-panicking string. Unchanged by #6766 — this
/// is still what a clean exit prints; it just no longer prints for the other
/// two outcomes.
/// Test: `exit_dispatch_keeps_the_relaunch_hint_for_a_real_session`.
pub(super) const RELAUNCH_HINT: &str = "tm: run `tm` to relaunch this session";

/// Printed when `claude` exited 0 but faster than a session can start (#6766).
///
/// Why: this is the refusal case — Claude Code declined the launch, said why on
/// its own last line, and exited successfully. The operator needs the pane to
/// name that as a failed relaunch, and to point at the message immediately
/// above rather than restating a string this crate does not own.
/// What: a single line, no interpolation.
/// Test: `exit_dispatch_reports_an_immediate_zero_exit_as_refused`.
const REFUSED_HINT: &str = "tm: claude exited immediately without starting a session — \
                            this launch was refused (its own message is above); \
                            run `tm` to retry";

/// Leading half of the non-zero-status line; the status is echoed between this
/// and [`FAILED_HINT_TAIL`] (#6766).
///
/// Why: a crashed or rejected `claude` — a bad `--resume` id, an auth failure —
/// must not print the relaunch hint, and the status code is the one piece of
/// evidence the pane can hand an operator for free. Split in two so the status
/// is passed as its own `echo` argument, which needs no nested quoting.
/// What: a static prefix, single-quoted at the call site.
/// Test: `exit_dispatch_reports_a_nonzero_status`.
const FAILED_HINT_HEAD: &str = "tm: claude exited with status";

/// Trailing half of the non-zero-status line (see [`FAILED_HINT_HEAD`]).
const FAILED_HINT_TAIL: &str = "— the session did not start; run `tm` to retry";

/// Seconds under which a status-0 exit is read as a refusal rather than a
/// finished session (#6766).
///
/// Why: Claude Code's background-session refusal prints and exits well inside a
/// second; a session an operator actually worked in lasts minutes. Any floor in
/// between separates them. The value is chosen high enough to absorb a slow
/// cold start that fails, and low enough that the only false positive is an
/// operator who quits `claude` immediately — who gets a misleading line, and
/// nothing worse.
/// What: a plain second count, interpolated into the `[ … -lt N ]` test.
/// Test: `exit_dispatch_keeps_the_relaunch_hint_for_a_real_session` drives the
/// stamp past this value; `exit_dispatch_reports_an_immediate_zero_exit_as_refused`
/// drives it under.
const LAUNCH_FLOOR_SECS: u32 = 5;

/// Shell variable holding the launch second, written by [`launch_clock_prefix`]
/// and read by [`exit_dispatch_suffix`].
///
/// Why: the two fragments are assembled into one command by separate callers
/// (`spawn_command` and `resume_command`), so the name they agree on must live
/// in exactly one place. The `__tm_` prefix keeps it out of the way of anything
/// the operator's shell or `claude` itself defines.
const CLOCK_VAR: &str = "__tm_t0";

/// Shell variable holding `claude`'s exit status inside [`exit_dispatch_suffix`].
const STATUS_VAR: &str = "__tm_rc";

/// Stamp the launch second into the pane shell, ahead of the `claude`
/// invocation (#6766).
///
/// Why: [`exit_dispatch_suffix`] needs to know how long `claude` ran, and the
/// pane shell is the only process still present when it exits — `tm` sent the
/// keystrokes and walked away. A shell assignment is the cheapest clock
/// available there, and it lands in the same shell for the same reason
/// `session_id_export_prefix`'s export does (see `cd_and_group`: a brace group,
/// never a subshell).
/// What: `__tm_t0=$(date +%s); `, with a trailing space so it concatenates
/// cleanly with the prefixes that follow it.
/// Test: `launch_clock_prefix_stamps_a_second`.
pub(super) fn launch_clock_prefix() -> String {
    format!("{CLOCK_VAR}=$(date +%s); ")
}

/// Report what actually happened to `claude`, appended AFTER the invocation
/// (#6766 — replaces #2023 component D's unconditional `; echo '<hint>'`).
///
/// Why: see this module's header. `;` still sequences the report to run only
/// once `claude` exits and control returns to the pane shell — the moment
/// component A leaves the pane idle at — but WHAT it reports is now a function
/// of how that exit went, so a refused relaunch stops reading as a finished
/// session.
/// What: captures `$?` into [`STATUS_VAR`] as the very first thing (any command
/// in between would overwrite it), then branches: non-zero → the failed line
/// with the status echoed as its own argument; status 0 within
/// [`LAUNCH_FLOOR_SECS`] of [`CLOCK_VAR`] → [`REFUSED_HINT`]; otherwise
/// [`RELAUNCH_HINT`]. Every literal is single-quoted via
/// [`shell_single_quote`], matching this file's established convention.
/// Test: the four `exit_dispatch_*` tests in this file's `tests` module.
pub(super) fn exit_dispatch_suffix() -> String {
    let failed_head = shell_single_quote(FAILED_HINT_HEAD);
    let failed_tail = shell_single_quote(FAILED_HINT_TAIL);
    let refused = shell_single_quote(REFUSED_HINT);
    let hint = shell_single_quote(RELAUNCH_HINT);
    format!(
        "; {STATUS_VAR}=$?; \
         if [ \"${STATUS_VAR}\" -ne 0 ]; then echo {failed_head} \"${STATUS_VAR}\" {failed_tail}; \
         elif [ $(($(date +%s) - ${{{CLOCK_VAR}:-0}})) -lt {LAUNCH_FLOOR_SECS} ]; \
         then echo {refused}; \
         else echo {hint}; fi"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a pane-shell fragment through `/bin/sh` and return its stdout.
    ///
    /// Why: the defect this module fixes is what the PANE PRINTS, and a string
    /// assertion on the command cannot tell a correct branch from a broken one
    /// — a shell syntax error would satisfy `cmd.contains(...)` and still leave
    /// the operator with nothing. Executing the fragment is the only assertion
    /// that covers both.
    /// What: `sh -c "<clock> <body><dispatch>"`, stdout as a trimmed `String`.
    /// `body` carries NO trailing `;` — the dispatch opens with one, exactly as
    /// it does when appended to the real `claude` invocation.
    fn run(clock_stamp: &str, body: &str) -> String {
        let script = format!("{clock_stamp} {body}{}", exit_dispatch_suffix());
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("/bin/sh must be executable");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A stamp `secs` seconds in the past, as the prefix would have written it.
    fn stamp_secs_ago(secs: u32) -> String {
        format!("{CLOCK_VAR}=$(( $(date +%s) - {secs} ));")
    }

    #[test]
    fn launch_clock_prefix_stamps_a_second() {
        // The prefix must leave a parseable epoch second in the shell — the
        // whole elapsed branch is dead weight if it does not.
        let script = format!("{}echo \"${CLOCK_VAR}\"", launch_clock_prefix());
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("/bin/sh must be executable");
        let stamped = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert!(
            stamped.parse::<u64>().is_ok_and(|s| s > 1_600_000_000),
            "launch clock must stamp a plausible epoch second, got {stamped:?}"
        );
    }

    #[test]
    fn exit_dispatch_reports_a_nonzero_status() {
        // #6766: a claude that failed outright must say so — and must NOT
        // print the relaunch hint, which is the whole defect.
        let printed = run(&stamp_secs_ago(0), "(exit 3)");
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
    fn exit_dispatch_reports_an_immediate_zero_exit_as_refused() {
        // #6766 / #6765: the observed refusal exits 0. Status alone cannot see
        // it; the elapsed floor must.
        let printed = run(&stamp_secs_ago(0), "true");
        assert_eq!(
            printed, REFUSED_HINT,
            "an immediate status-0 exit must be reported as a refused launch"
        );
        assert!(
            !printed.contains(RELAUNCH_HINT),
            "a refused launch must not print the clean-exit relaunch hint, got {printed:?}"
        );
    }

    #[test]
    fn exit_dispatch_keeps_the_relaunch_hint_for_a_real_session() {
        // The #2023 component D behaviour survives for the case it was written
        // for: claude ran, the operator exited it, the pane advertises `tm`.
        let printed = run(&stamp_secs_ago(LAUNCH_FLOOR_SECS + 60), "true");
        assert_eq!(
            printed, RELAUNCH_HINT,
            "a session that ran past the launch floor must still get the hint"
        );
    }

    #[test]
    fn exit_dispatch_falls_back_to_the_hint_without_a_clock() {
        // Fail open: a pane shell that never saw the clock prefix degrades to
        // pre-#6766 behaviour, never to a false refusal.
        let printed = run("", "true");
        assert_eq!(
            printed, RELAUNCH_HINT,
            "a missing launch clock must fall back to the relaunch hint"
        );
    }
}
