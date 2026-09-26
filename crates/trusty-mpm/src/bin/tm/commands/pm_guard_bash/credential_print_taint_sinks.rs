//! Stage shapes that print a credential without naming a printer, for the
//! credential-print rule (#8676 round 3).
//!
//! Why: the round-two review found three more routes to the terminal. A
//! builtin echoes a bad operand in its error (`cd "$T"`, `export "$T"`) or
//! lists it (`compgen -W "$T"`); `trap` stores code the shell runs at exit;
//! and an evaluator reads its script from stdin or a `<(…)` file by path
//! (`bash /dev/stdin`, `python3 <(…)`), which no code operand shows.
//! What: [`judge_builtin_sinks`] routes both descriptors of an echoing builtin
//! given a carrying operand, and scans `trap` code as shell text with the
//! stage's own sinks. [`reads_script_by_path`] finds an evaluator operand that
//! names stdin or a process substitution.
//! Test: `credential_print_tests::denies_builtins_that_echo_their_operand`,
//! `credential_print_tests::denies_the_missing_evaluators`,
//! `credential_print_tests::denies_an_evaluator_reading_its_script_by_path`,
//! `credential_print_tests::allows_the_round_three_neighbours`.

use super::credential_print_taint::DECLARERS;
use super::{Emitted, Lifted, Refusal, Sink, SubKind, carries, marks_in, route, scan};

/// Builtins whose output or error text repeats an operand: `cd: <x>: No such
/// file`, `exit: <x>: numeric argument required`, `compgen -W <x>` lists it.
const ECHOING_BUILTINS: &[&str] = &[
    "cd", "pushd", "popd", "exit", "return", "kill", "sleep", "compgen", "shift", "wait", "ulimit",
    "umask", "type", "hash", "alias", "help", "enable", "fg", "bg", "disown", "jobs", "getopts",
    "read",
];

/// Route a builtin that repeats a carrying operand, and scan `trap` code.
///
/// What: an [`ECHOING_BUILTINS`] entry with any carrying operand, or a
/// [`DECLARERS`]/`unset` operand whose name part (before `=`) carries —
/// bash reports it as "not a valid identifier" — routes `out` and `err`.
/// `trap` code is scanned as shell text one level deeper with `out`/`err`,
/// as the shell runs it with the stage's descriptors.
pub(super) fn judge_builtin_sinks(
    program: &str,
    args: &[String],
    (out, err): (Sink, Sink),
    lifted: &Lifted,
    depth: usize,
    emitted: &mut Emitted,
) -> Result<(), Refusal> {
    let echoes = if ECHOING_BUILTINS.contains(&program) {
        args.iter().any(|a| carries(a, lifted))
    } else if DECLARERS.contains(&program) || matches!(program, "unset" | "integer") {
        args.iter()
            .any(|a| carries(a.split('=').next().unwrap_or_default(), lifted))
    } else {
        false
    };
    if echoes {
        route(out, emitted)?;
        route(err, emitted)?;
    }
    if program == "trap"
        && let Some(code) = trap_code(args)
        && scan(code, out, err, depth + 1, lifted)?
    {
        route(out, emitted)?;
    }
    Ok(())
}

/// The code operand of `trap`: the first operand after an optional `--`,
/// unless it only lists (`-p`, `-l`) or resets (`-`).
fn trap_code(args: &[String]) -> Option<&str> {
    let rest = match args.first().map(String::as_str) {
        Some("--") => args.get(1..).unwrap_or_default(),
        Some("-p" | "-l" | "-P") => return None,
        _ => args,
    };
    rest.first().map(String::as_str).filter(|c| *c != "-")
}

/// Whether an evaluator operand hands it a script by a path that is stdin or
/// inline text: `-`, `/dev/stdin`, `/dev/fd/N`, `/proc/*/fd/N`, or a `<(…)`
/// file, including the target of `< <(…)`.
pub(super) fn reads_script_by_path(operands: &[String], lifted: &Lifted) -> bool {
    operands.iter().any(|a| {
        matches!(a.as_str(), "-" | "/dev/stdin")
            || a.starts_with("/dev/fd/")
            || (a.starts_with("/proc/") && a.contains("/fd/"))
            || marks_in(a).any(|m| lifted.subs.get(m).is_none_or(|s| s.kind == SubKind::Input))
    })
}
