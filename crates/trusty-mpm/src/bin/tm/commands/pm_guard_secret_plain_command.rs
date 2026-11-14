//! `tm hook --pm-guard` — whether a command is one plain command whose
//! program never runs its operands, so an operand pair in it is no `gh api`
//! call (#10548 P7, P7b).
//!
//! Why: split out of `pm_guard_secret_gh_api_delete` for the 500-SLOC cap.
//! What: [`one_plain_command`] and [`flags_only`], read by that module's
//! `gh_calls`.
//! Test: `a_single_segment_operand_pair_is_no_gh_api_call_10548`,
//! `a_for_loop_over_operand_pairs_is_no_gh_api_call_10548`,
//! `a_for_loop_keeps_the_delete_rows_refusing_10548`,
//! `every_review_bypass_row_refuses_10548`.

/// Programs that never execute an operand as a command (#10548).
const NEVER_RUNS_OPERANDS: &[&str] = &["cargo", "rsync", "cp", "mv", "ln", "mkdir", "touch"];

/// Bytes that end a segment, open a subshell, group or substitution,
/// assign, escape, redirect or comment (#10548).
const NOT_ONE_PLAIN_COMMAND: &[char] = &[
    ';', '&', '|', '\n', '\r', '(', ')', '`', '{', '}', '=', '\\', '#', '<', '>',
];

/// Words that define, assign, source, re-path or re-run a program (#10548).
const REDEFINERS: &[&str] = &[
    "alias",
    "function",
    "hash",
    "PATH",
    "source",
    "eval",
    "enable",
    "exec",
    "builtin",
    "command",
    "BASH_FUNC",
    "BASH_ENV",
    "ENV",
    "cd",
    "pushd",
    "popd",
    "export",
    "declare",
    "typeset",
    "readonly",
    "read",
    "printf",
    "local",
    "let",
    "set",
    "unset",
    "shopt",
    "trap",
    "BASH_CMDS",
    "commands",
    "BASH_ALIASES",
    "aliases",
];

/// Whether `command` is one plain command whose program is a literal
/// [`NEVER_RUNS_OPERANDS`] word, alone or as the whole body of one `for`
/// loop.
///
/// Why: #10548 P7 — `cargo test -p "$c" "$t" --no-fail-fast` read `"$c" "$t"`
/// as a rewritten `gh api` call and refused it as a secret DELETE; P7b — the
/// same command looped over crates (`for c in a b; do …; done`) still did.
/// What: read on the raw text, with no lexer. The command, or the body of
/// `for NAME in WORDS; do BODY; done` ([`for_loop_body`]), starts with the
/// program word and a space and carries none of [`NOT_ONE_PLAIN_COMMAND`];
/// no identifier anywhere in the text is one of [`REDEFINERS`]. So no other
/// segment, no assignment (`NAME=`, `BASH_CMDS[cp]=`, `PA''TH=`, a loop over
/// `commands`) and no redefinition can make the program run its operands.
/// Anything else judges every pair.
/// Test: `a_single_segment_operand_pair_is_no_gh_api_call_10548`,
/// `a_for_loop_over_operand_pairs_is_no_gh_api_call_10548`,
/// `a_for_loop_keeps_the_delete_rows_refusing_10548`,
/// `every_review_bypass_row_refuses_10548`.
pub(super) fn one_plain_command(command: &str) -> bool {
    let text = command.trim();
    let identifier = |c: char| c.is_ascii_alphanumeric() || c == '_';
    // #10548 P7b: a `for` loop is judged by its body.
    let body = for_loop_body(text).unwrap_or(text);
    !body.contains(NOT_ONE_PLAIN_COMMAND)
        && !text
            .split(|c: char| !identifier(c))
            .any(|word| REDEFINERS.contains(&word))
        && NEVER_RUNS_OPERANDS.iter().any(|p| {
            body.strip_prefix(p)
                .is_some_and(|rest| rest.starts_with(' '))
        })
}

/// The body of `text` when it is exactly `for NAME in WORDS; do BODY; done`,
/// else `None` (#10548 P7b).
///
/// What: NAME is a lower-case shell identifier, so the loop cannot rebind an
/// exported upper-case name a program reads (`RUSTC_WRAPPER`, `PATH`), and
/// WORDS carry none of [`NOT_ONE_PLAIN_COMMAND`], so they hold no
/// substitution, assignment or second command. The caller judges BODY.
/// Test: `a_for_loop_over_operand_pairs_is_no_gh_api_call_10548`,
/// `a_for_loop_keeps_the_delete_rows_refusing_10548`.
fn for_loop_body(text: &str) -> Option<&str> {
    let (name, rest) = text.strip_prefix("for ")?.split_once(" in ")?;
    let (words, rest) = rest.split_once("; do ")?;
    let body = rest.strip_suffix("; done")?;
    let lower = |b: u8| b.is_ascii_lowercase() || b == b'_';
    let name_ok = name.bytes().next().is_some_and(lower)
        && name.bytes().all(|b| lower(b) || b.is_ascii_digit());
    (name_ok && !words.contains(NOT_ONE_PLAIN_COMMAND)).then_some(body)
}

/// Whether every word of `tail` is a plain long flag (`--no-fail-fast`), so
/// the tail names no endpoint, method, subcommand or value (#10548).
pub(super) fn flags_only(tail: &[String]) -> bool {
    tail.iter().all(|w| {
        w.len() > 2
            && w != "--method"
            && w.strip_prefix("--").is_some_and(|name| {
                name.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            })
    })
}
