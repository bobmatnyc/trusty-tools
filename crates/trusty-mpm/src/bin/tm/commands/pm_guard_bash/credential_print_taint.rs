//! Shell variables that hold a credential, for the credential-print rule
//! (#8676).
//!
//! Why: `T=$(gcloud auth print-access-token); echo "${T:0:10}"` prints the
//! value one stage after it was captured. Judging each stage alone loses the
//! value at `;`, `&&`, a newline or a loop.
//! What: [`bound_names`] reads the names a stage binds from a carrying word;
//! [`expands_tainted`] reads whether a word expands one; [`dumps_variables`]
//! reads whether a stage prints the variables themselves. The scan treats the
//! set as flow-insensitive: a name bound anywhere in the command taints every
//! stage, which also covers a loop body that runs after its own assignment.
//! Test: `credential_print_tests::denies_a_credential_carried_by_a_variable`,
//! `credential_print_tests::allows_a_carried_variable_that_is_never_printed`.

use std::collections::BTreeSet;

use super::credential_print_programs::basename;
use super::{Lifted, MARK, carries};
use crate::commands::hook_rewrite::is_env_assignment;

/// The name recorded when the positional parameters hold a credential.
pub(super) const POSITIONAL: &str = "@";

/// The name recorded when an assignment's name is chosen at run time: every
/// expansion then counts as carrying.
pub(super) const ANY: &str = "$any";

/// Builtins that bind (`declare T=…`) or list (`declare -p`) variables.
const DECLARERS: &[&str] = &["declare", "typeset", "export", "readonly", "local"];

/// The names a stage binds from a credential-carrying word.
///
/// What: every `NAME=`/`NAME+=`/`NAME[i]=` word whose value carries, wherever
/// it stands — a prefix assignment, an `export`/`local`/`declare`/`readonly`
/// operand, a word inside a `{ …; }` body; a run-time name binds [`ANY`]; a
/// `declare -n R=T` nameref to a tainted `T`; `for`/`select NAME in WORDS`
/// when a listed word carries, or with no `in` list when the positional
/// parameters do; and `set …` with a carrying operand binds [`POSITIONAL`].
pub(super) fn bound_names(
    argv: &[String],
    kw: usize,
    program: &str,
    args: &[String],
    lifted: &Lifted,
) -> Vec<String> {
    let mut bound = Vec::new();
    let nameref = DECLARERS.contains(&program)
        && args
            .iter()
            .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('n'));
    for word in argv {
        let Some((lhs, value)) = word.split_once('=') else {
            continue;
        };
        let lhs = lhs.strip_suffix('+').unwrap_or(lhs);
        let name = lhs.split('[').next().unwrap_or(lhs);
        let carried = carries(value, lifted) || (nameref && is_tainted(value, &lifted.names));
        if !carried {
            continue;
        }
        if is_identifier(name) {
            bound.push(name.to_string());
        } else if name.contains('$') || name.contains('`') || name.contains(MARK) {
            bound.push(ANY.to_string());
        }
    }
    let rest = argv.get(kw..).unwrap_or_default();
    if let [head, name, list @ ..] = rest
        && matches!(head.as_str(), "for" | "select")
    {
        let carried = match list.split_first() {
            Some((first, words)) if first == "in" => words.iter().any(|w| carries(w, lifted)),
            _ => is_tainted(POSITIONAL, &lifted.names),
        };
        if carried {
            bound.push(name.clone());
        }
    }
    if program == "set" && args.iter().any(|a| carries(a, lifted)) {
        bound.push(POSITIONAL.to_string());
    }
    bound
}

/// Whether `name` holds a credential: it is in `names`, it is a positional
/// parameter while [`POSITIONAL`] is, or a run-time name made every name suspect.
fn is_tainted(name: &str, names: &BTreeSet<String>) -> bool {
    let positional = matches!(name, "@" | "*")
        || (!name.is_empty() && name != "0" && name.bytes().all(|b| b.is_ascii_digit()));
    names.contains(name)
        || (positional && names.contains(POSITIONAL))
        || (names.contains(ANY) && (positional || is_identifier(name)))
}

/// Whether `word` expands a tainted name: `$NAME`, `${NAME}`, or `${NAME…}`
/// with any operator (slice, default, pattern). `${#NAME}` is the length and
/// does not carry; `${!…}` indirection does, as the name it reads is unknown.
pub(super) fn expands_tainted(word: &str, names: &BTreeSet<String>) -> bool {
    if names.is_empty() {
        return false;
    }
    let bytes = word.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        i += 1;
        if bytes.get(i) == Some(&b'{') {
            let start = i + 1;
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
            if braced_carries(&word[start..end], names) {
                return true;
            }
            i = j;
            continue;
        }
        let name = leading_name(&word[i..]);
        if is_tainted(name, names) {
            return true;
        }
        i += name.len();
    }
    false
}

/// The body of one `${…}`.
fn braced_carries(body: &str, names: &BTreeSet<String>) -> bool {
    if body.starts_with('!') {
        return true;
    }
    if let Some(rest) = body.strip_prefix('#') {
        let name = leading_name(rest);
        let after = &rest[name.len()..];
        // `${#T}`, `${#T[@]}`, and `${#}` (the positional count) are lengths.
        let length = after.is_empty() || (after.starts_with('[') && after.ends_with(']'));
        return !length && is_tainted(name, names);
    }
    let name = leading_name(body);
    is_tainted(name, names) || expands_tainted(&body[name.len()..], names)
}

/// Whether `text` is a shell variable name.
fn is_identifier(text: &str) -> bool {
    text.as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && leading_name(text).len() == text.len()
}

/// The parameter name at the start of `text`: an identifier, one digit, or
/// one of `@ * # ? - $ !`; empty when none.
fn leading_name(text: &str) -> &str {
    let bytes = text.as_bytes();
    match bytes.first() {
        Some(b) if b.is_ascii_alphabetic() || *b == b'_' => {
            let len = bytes
                .iter()
                .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
                .count();
            &text[..len]
        }
        Some(b) if b.is_ascii_digit() || b"@*#?-$!".contains(b) => &text[..1],
        _ => "",
    }
}

/// Whether a stage prints variables while one is tainted.
///
/// What: `printenv` with no operand or a tainted one, and `env` that runs no
/// command, found anywhere in `argv` because a wrapper's own flags can hide
/// them (`timeout 5 env`); `set` with no operand; and a [`DECLARERS`] builtin
/// that lists (no operand) or prints a tainted name (`-p`).
pub(super) fn dumps_variables(
    argv: &[String],
    program: &str,
    args: &[String],
    names: &BTreeSet<String>,
) -> bool {
    if names.is_empty() {
        return false;
    }
    let operands = |words: &[String]| -> Vec<String> {
        words
            .iter()
            .filter(|w| !w.starts_with('-'))
            .map(|w| w.split('=').next().unwrap_or_default().to_string())
            .collect()
    };
    for (at, word) in argv.iter().enumerate() {
        let rest = argv.get(at + 1..).unwrap_or_default();
        match basename(word).as_str() {
            "printenv" => {
                let ops = operands(rest);
                if ops.is_empty() || ops.iter().any(|o| is_tainted(o, names)) {
                    return true;
                }
            }
            "env" if env_runs_nothing(rest) => return true,
            _ => {}
        }
    }
    match program {
        "set" => args.is_empty(),
        p if DECLARERS.contains(&p) => {
            let ops = operands(args);
            let print = args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('p'));
            ops.is_empty() || (print && ops.iter().any(|o| is_tainted(o, names)))
        }
        _ => false,
    }
}

/// Whether `env`'s words name no command, so it prints the environment.
/// A value-taking cluster (`-u NAME`, `-C DIR`, `-P PATH`) skips its value;
/// `-S` hands a string to the wrapper scan instead.
fn env_runs_nothing(words: &[String]) -> bool {
    let mut i = 0;
    while let Some(w) = words.get(i) {
        i += 1;
        if w == "--" {
            return words[i..].iter().all(|w| is_env_assignment(w));
        }
        if w.starts_with("-S") || w.starts_with("--split-string") {
            return false;
        }
        if matches!(w.as_str(), "--unset" | "--chdir") {
            i += 1;
        } else if let Some(cluster) = w.strip_prefix('-').filter(|c| !c.starts_with('-')) {
            if cluster.ends_with(['u', 'C', 'P']) {
                i += 1;
            }
        } else if !w.starts_with('-') && !is_env_assignment(w) {
            return false;
        }
    }
    true
}
