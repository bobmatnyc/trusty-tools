//! Stage shapes that print a credential without naming a printer, for the
//! credential-print rule (#8676 round 3, round 5).
//!
//! Why: the round-two review found three more routes to the terminal. A
//! builtin echoes a bad operand in its error (`cd "$T"`, `export "$T"`) or
//! lists it (`compgen -W "$T"`); `trap` stores code the shell runs at exit;
//! and an evaluator reads its script from stdin or a `<(…)` file by path
//! (`bash /dev/stdin`, `python3 <(…)`), which no code operand shows. Round
//! five added a fourth: `${NAME?word}`/`${NAME:?word}` aborts the command and
//! writes `word` to stderr at expansion time, whether or not `NAME` is
//! actually unset, and whether or not the stage runs any program at all.
//! What: [`judge_builtin_sinks`] routes both descriptors of an echoing builtin
//! given a carrying operand, and scans `trap` code as shell text with the
//! stage's own sinks. [`reads_script_by_path`] finds an evaluator operand that
//! names stdin or a process substitution. [`reports_unset_error`] finds a
//! `${NAME?word}`/`${NAME:?word}` whose `word` carries.
//! Test: `credential_print_tests::denies_builtins_that_echo_their_operand`,
//! `credential_print_tests::denies_the_missing_evaluators`,
//! `credential_print_tests::denies_an_evaluator_reading_its_script_by_path`,
//! `credential_print_tests::allows_the_round_three_neighbours`,
//! `credential_print_tests::denies_the_round_five_bypasses`,
//! `credential_print_tests::allows_the_round_five_neighbours`.

use std::collections::BTreeSet;

use super::credential_print_taint::{DECLARERS, leading_name};
use super::credential_print_taint_forms::reads_tainted;
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

/// Whether any word in `argv` reads `${NAME?word}`/`${NAME:?word}` whose
/// `word` carries the value (#8676 round 5).
///
/// What: bash aborts the command and writes `word` to stderr when `NAME` is
/// unset (or null, under `:?`) — this runs at word-expansion time, before any
/// program starts, so it also catches a stage that is only an assignment
/// (`Y=${UNSET?$T}`). It does not need to know whether `NAME` is actually
/// unset: the guard cannot prove it never is, so a carrying `word` always
/// refuses.
pub(super) fn reports_unset_error(argv: &[String], names: &BTreeSet<String>) -> bool {
    !names.is_empty() && argv.iter().any(|w| word_reports_unset_error(w, names))
}

/// [`reports_unset_error`] for one word: scans every `${…}` body it holds.
fn word_reports_unset_error(word: &str, names: &BTreeSet<String>) -> bool {
    let bytes = word.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' || bytes.get(i + 1) != Some(&b'{') {
            i += 1;
            continue;
        }
        let start = i + 2;
        let mut level = 1usize;
        let mut j = start;
        while j < bytes.len() && level > 0 {
            match bytes[j] {
                b'{' => level += 1,
                b'}' => level -= 1,
                _ => {}
            }
            j += 1;
        }
        let end = if level == 0 { j - 1 } else { j };
        if body_reports_unset_error(&word[start..end], names) {
            return true;
        }
        i = j;
    }
    false
}

/// The body of one `${…}`: whether its operator is `?`/`:?` and the word past
/// it carries.
fn body_reports_unset_error(body: &str, names: &BTreeSet<String>) -> bool {
    let name = leading_name(body);
    if name.is_empty() {
        return false;
    }
    let mut rest = &body[name.len()..];
    if let Some(after) = rest.strip_prefix('[') {
        rest = after.find(']').map_or("", |end| &after[end + 1..]);
    }
    rest.strip_prefix(":?")
        .or_else(|| rest.strip_prefix('?'))
        .is_some_and(|word| reads_tainted(word, names))
}
