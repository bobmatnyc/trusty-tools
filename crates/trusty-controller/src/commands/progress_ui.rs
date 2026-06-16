//! Shared `trusty-progress` wiring + interactive consent for install/upgrade.
//!
//! Why: The install and upgrade handlers both render rustup-style component rows
//! and both need a confirm prompt. Centralising the [`trusty_progress::Output`]
//! mode selection (honouring `--json`/TTY) and the stdin yes/no prompt keeps the
//! two handlers thin and consistent, and keeps all interactive I/O in one place.
//!
//! What: [`progress_mode`] picks the `trusty-progress` [`Mode`] from the `--json`
//! flag and the ambient stderr TTY; [`narrator`] builds a stderr [`Narrator`];
//! [`prompt_yes_no`] reads a yes/no answer from stdin (stderr prompt, stdout
//! reserved for data); [`is_tty`] reports whether stdin is interactive.
//!
//! Test: `tests` covers `progress_mode`'s json→Plain rule and `is_tty`'s
//! type; the stdin prompt is side-effect-only.

use trusty_progress::{Mode, Narrator, Output};

/// Pick the `trusty-progress` rendering mode for a command.
///
/// Why: Under `--json` (machine output) or a non-TTY (pipe/CI) we must not emit
/// animated spinners or interleave human progress with the JSON on stdout;
/// progress goes to stderr as plain lines. Interactive terminals get the full
/// rustup-style animated display.
///
/// What: Returns [`Mode::Plain`] when `json` is set OR stderr is not a terminal;
/// [`Mode::Interactive`] otherwise. (`Output::to_stderr` already autodetects the
/// TTY, but threading `json` through lets `--json` force plain even on a TTY.)
///
/// Test: `tests::progress_mode_json_is_plain`.
pub fn progress_mode(json: bool) -> Mode {
    if json {
        Mode::Plain
    } else if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        Mode::Interactive
    } else {
        Mode::Plain
    }
}

/// Build a stderr [`Narrator`] in the mode appropriate for the command.
///
/// Why: Progress narration (`info:` lines) must land on stderr so stdout stays
/// clean for `--json` framing (CLAUDE.md). One helper keeps every call site from
/// re-deriving the mode + sink.
///
/// What: Constructs an [`Output::to_stderr_with_mode`] using [`progress_mode`]
/// and wraps it in a [`Narrator`]. The returned narrator's `output()` can be
/// shared with a `ComponentTracker` so rows interleave correctly.
///
/// Test: Side-effect-only (writes to stderr); the mode logic is tested via
/// `progress_mode`.
pub fn narrator(json: bool) -> Narrator {
    Narrator::new(Output::to_stderr_with_mode(progress_mode(json)))
}

/// Whether stdin is an interactive terminal we can prompt on.
///
/// Why: The confirm-then-apply gate (#1316) only prompts when it can actually
/// read a human answer; on a pipe/CI it must fall back to the refuse-without-
/// `--yes` posture.
///
/// What: Returns `true` when stdin is a TTY.
///
/// Test: `tests::is_tty_returns_bool` (the value depends on the harness's fds,
/// so we assert only that it is callable and boolean).
pub fn is_tty() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdin())
}

/// Prompt the operator with a yes/no question on stderr and read the answer.
///
/// Why: The confirm-then-apply gate needs interactive consent; centralising the
/// prompt keeps the wording and the stderr-not-stdout discipline consistent.
///
/// What: Prints `{question} [y/N] ` to stderr (stdout is reserved for data),
/// reads a line from stdin, and returns `true` only on an explicit `y`/`yes`
/// (default No on empty input or read error — the safe default).
///
/// Test: Side-effect-only (stdin); the decision logic that consumes its result
/// is unit-tested via `update_engine::decide_apply`.
pub fn prompt_yes_no(question: &str) -> bool {
    use std::io::Write;
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    let ans = line.trim().to_ascii_lowercase();
    ans == "y" || ans == "yes"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `--json` must always force plain (no animation, no stdout noise).
    /// What: Asserts `progress_mode(true) == Mode::Plain`.
    /// Test: This is the test.
    #[test]
    fn progress_mode_json_is_plain() {
        assert_eq!(progress_mode(true), Mode::Plain);
    }

    /// Why: `is_tty` must be callable and return a bool regardless of harness fds.
    /// What: Calls it and binds the result (the value is environment-dependent).
    /// Test: This is the test.
    #[test]
    fn is_tty_returns_bool() {
        let _v: bool = is_tty();
    }
}
