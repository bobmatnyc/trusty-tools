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
//! `credential_print_tests::allows_a_carried_variable_that_is_never_printed`,
//! `credential_print_tests::denies_zsh_expansion_forms`,
//! `credential_print_tests::denies_brace_expanded_declarer_names`,
//! `credential_print_tests::denies_nameref_loops_and_subscripted_namerefs`,
//! `credential_print_tests::deep_brace_nesting_denies_without_overflow`.

use std::collections::BTreeSet;

use super::credential_print_programs::basename;
use super::credential_print_taint_forms::reads_tainted;
use super::{Lifted, MARK, carries};
use crate::commands::hook_rewrite::is_env_assignment;

/// The name recorded when the positional parameters hold a credential.
pub(super) const POSITIONAL: &str = "@";

/// The name recorded when an assignment's name is chosen at run time: every
/// expansion then counts as carrying.
pub(super) const ANY: &str = "$any";

/// #8676 round 3: recorded while tainted when some name has the integer
/// attribute, so any assignment of a bare tainted name is arithmetic.
pub(super) const INTEGER: &str = "$integer";

/// Builtins that bind (`declare T=…`) or list (`declare -p`) variables.
pub(super) const DECLARERS: &[&str] = &["declare", "typeset", "export", "readonly", "local"];

/// The names bash (`BASH_REMATCH`) and zsh (`match`, `MATCH`) fill from `=~`.
const REMATCH: &[&str] = &["BASH_REMATCH", "match", "MATCH"];

/// The names a stage binds from a credential-carrying word.
///
/// What: every `NAME=`/`NAME+=`/`NAME[i]=` word whose value carries or names
/// a tainted name bare, wherever it stands — a prefix assignment, an
/// `export`/`local`/`declare`/`readonly` operand, a word inside a `{ …; }`
/// body; a run-time, brace-expanded or globbed name binds [`ANY`];
/// `for`/`select NAME in WORDS` when a listed word carries or names one, or
/// with no `in` list when the positional parameters do; `set …` with a
/// carrying operand binds [`POSITIONAL`]; `=~` against a carrying word binds
/// the [`REMATCH`] names; and an integer declaration binds [`INTEGER`].
pub(super) fn bound_names(
    argv: &[String],
    kw: usize,
    program: &str,
    args: &[String],
    lifted: &Lifted,
) -> Vec<String> {
    let mut bound = Vec::new();
    // #8676 round 3: `declare -i` makes every later `X=T` arithmetic.
    if declares_integer(program, args) && !lifted.names.is_empty() {
        bound.push(INTEGER.to_string());
    }
    for word in argv {
        let Some((lhs, value)) = word.split_once('=') else {
            continue;
        };
        let lhs = lhs.strip_suffix('+').unwrap_or(lhs);
        let name = lhs.split('[').next().unwrap_or(lhs);
        // #8676 round 3: a value naming a tainted name bare copies it — a
        // nameref (`R=T`, `R='A[0]'`) or a name arithmetic evaluates
        // recursively (`U=T; $((U))`).
        let carried = carries(value, lifted) || reads_tainted(value, &lifted.names);
        if !carried {
            continue;
        }
        if is_identifier(name) {
            bound.push(name.to_string());
        } else if name.contains('$') || name.contains('`') || name.contains(MARK) {
            bound.push(ANY.to_string());
        } else if expands_to_names(name) {
            // #8676 round 3: brace expansion or a glob (`{T,U}=`, `T{,}=`)
            // builds the name at run time, as `$N=` does.
            bound.push(ANY.to_string());
        }
    }
    let rest = argv.get(kw..).unwrap_or_default();
    if let [head, name, list @ ..] = rest
        && matches!(head.as_str(), "for" | "select")
    {
        // #8676 round 3: a bare name in the list binds a nameref loop variable.
        let carried = match list.split_first() {
            Some((first, words)) if first == "in" => words
                .iter()
                .any(|w| carries(w, lifted) || reads_tainted(w, &lifted.names)),
            _ => is_tainted(POSITIONAL, &lifted.names),
        };
        if carried {
            bound.push(name.clone());
        }
    }
    if program == "set" && args.iter().any(|a| carries(a, lifted)) {
        bound.push(POSITIONAL.to_string());
    }
    // #8676: `[[ $T =~ (.*) ]]` copies the match into these names.
    if argv.iter().any(|w| w == "=~") && argv.iter().any(|w| carries(w, lifted)) {
        bound.extend(REMATCH.iter().map(|n| (*n).to_string()));
    }
    bound
}

/// Whether `name` holds a credential: it is in `names`, it is a positional
/// parameter while [`POSITIONAL`] is, or a run-time name made every name suspect.
pub(super) fn is_tainted(name: &str, names: &BTreeSet<String>) -> bool {
    let positional = matches!(name, "@" | "*")
        || (!name.is_empty() && name != "0" && name.bytes().all(|b| b.is_ascii_digit()));
    names.contains(name)
        || (positional && names.contains(POSITIONAL))
        || (names.contains(ANY) && (positional || is_identifier(name)))
}

/// Whether a declaration gives its names the integer attribute (#8676 round
/// 3): a [`DECLARERS`] cluster holding `i` (`declare -i`, `local -ri`), or
/// zsh's `integer`.
pub(super) fn declares_integer(program: &str, args: &[String]) -> bool {
    program == "integer"
        || (DECLARERS.contains(&program)
            && args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('i')))
}

/// Whether an assignment's left side is a brace expansion or a glob built
/// from name characters (`{T,U}`, `T{,}`, `T*`), so the shell picks the name.
fn expands_to_names(name: &str) -> bool {
    name.contains(['{', '*', '?'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_{},*?".contains(&b))
}

/// `${…}` nesting followed before a word counts as carrying (#8676): the
/// recursion below is bounded so a deep `${a${a…` cannot overflow the stack.
const MAX_BRACE_DEPTH: usize = 32;

/// Whether `word` expands a tainted name: `$NAME`, `${NAME}`, or `${NAME…}`
/// with any operator (slice, default, pattern). `${#NAME}` is the length and
/// does not carry; `${!…}` indirection does, as the name it reads is unknown.
pub(super) fn expands_tainted(word: &str, names: &BTreeSet<String>) -> bool {
    !names.is_empty() && expands_at(word, names, 0)
}

/// [`expands_tainted`] at `${…}` nesting `depth`; past [`MAX_BRACE_DEPTH`]
/// the word counts as carrying.
fn expands_at(word: &str, names: &BTreeSet<String>, depth: usize) -> bool {
    if depth > MAX_BRACE_DEPTH {
        return true;
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
            if braced_carries(&word[start..end], names, depth + 1) {
                return true;
            }
            i = j;
            continue;
        }
        // #8676: zsh `$=T`, `$~T` and `$^T` expand `T`.
        let flags = bytes[i..].iter().take_while(|b| b"=~^".contains(b)).count();
        let name = leading_name(&word[i + flags..]);
        if is_tainted(name, names) {
            return true;
        }
        i += flags + name.len();
    }
    false
}

/// The body of one `${…}`. Fail-closed (#8676): a body whose name this cannot
/// read — none after the zsh `(flags)` and `^`/`=`/`~` prefixes — carries, as
/// does zsh `(P)` indirection and a nested `${…}` that expands a tainted name.
fn braced_carries(body: &str, names: &BTreeSet<String>, depth: usize) -> bool {
    if body.starts_with('!') {
        return true;
    }
    let mut body = body;
    loop {
        if let Some(after) = body.strip_prefix('(') {
            let Some(close) = after.find(')') else {
                return true;
            };
            if after[..close].contains('P') {
                return true;
            }
            body = &after[close + 1..];
        } else if let Some(after) = body.strip_prefix(['^', '=', '~']) {
            body = after;
        } else {
            break;
        }
    }
    if let Some(rest) = body.strip_prefix('#') {
        let name = leading_name(rest);
        let after = &rest[name.len()..];
        // `${#T}`, `${#T[@]}`, and `${#}` (the positional count) are lengths.
        let length = after.is_empty() || (after.starts_with('[') && after.ends_with(']'));
        return !length && is_tainted(name, names);
    }
    if body.starts_with("${") {
        return expands_at(body, names, depth);
    }
    let name = leading_name(body);
    name.is_empty() || is_tainted(name, names) || expands_at(&body[name.len()..], names, depth)
}

/// Whether `text` is a shell variable name.
pub(super) fn is_identifier(text: &str) -> bool {
    text.as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && leading_name(text).len() == text.len()
}

/// The parameter name at the start of `text`: an identifier, one digit, or
/// one of `@ * # ? - $ !`; empty when none.
pub(super) fn leading_name(text: &str) -> &str {
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
/// them (`timeout 5 env`); `set` with no operand; a [`DECLARERS`] builtin
/// that lists (no operand) or prints a tainted name (`-p`, or zsh's flagless
/// `typeset`/`local`/`declare NAME`); and a [`reads_environment`] reader.
/// Test: `credential_print_tests::denies_environment_readers`,
/// `credential_print_tests::denies_a_flagless_declaration_of_a_tainted_name`.
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
    if reads_environment(argv, program, args) {
        return true;
    }
    match program {
        "set" => args.is_empty(),
        p if DECLARERS.contains(&p) => {
            let ops = operands(args);
            let print = args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('p'));
            // #8676: zsh prints an existing name given with no flag and no value.
            let shows = matches!(p, "typeset" | "local" | "declare")
                && !args.iter().any(|a| a.starts_with(['-', '+']))
                && args
                    .iter()
                    .any(|a| !a.contains('=') && is_tainted(a, names));
            shows || ops.is_empty() || (print && ops.iter().any(|o| is_tainted(o, names)))
        }
        _ => false,
    }
}

/// Whether a stage reads the environment by a route with no `$` (#8676): a
/// `/proc/*/environ` path, awk `ENVIRON`, jq `env`/`$ENV`, or `ps` with an
/// option cluster holding `e`/`E` (BSD `-e`, `-E`, and `e` all show it).
fn reads_environment(argv: &[String], program: &str, args: &[String]) -> bool {
    if argv
        .iter()
        .any(|w| w.contains("/proc/") && w.contains("environ"))
    {
        return true;
    }
    let words = |a: &String| {
        a.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    match program {
        "awk" | "gawk" | "mawk" | "nawk" => args.iter().any(|a| a.contains("ENVIRON")),
        "jq" | "gojq" | "jaq" => args
            .iter()
            .any(|a| words(a).iter().any(|w| w == "env" || w == "$ENV")),
        "ps" => args.iter().any(|a| {
            let cluster = a.strip_prefix('-').unwrap_or(a);
            !a.starts_with("--")
                && cluster.bytes().all(|b| b.is_ascii_alphabetic())
                && cluster.contains(['e', 'E'])
        }),
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
