//! The program side of the #8625 kuzu-store rule: running `kuzu-memory`, any
//! `trusty-memory import kuzu`, and the deprecated `migrate` aliases.
//!
//! Why: `kuzu-memory memory export` dumps raw rows without naming a store
//! path, and `trusty-memory import kuzu --discover` finds stores without
//! naming one, so the path-word test alone sees neither. Architect ruling
//! 2026-10-12 15:42Z: "Deny every trusty-memory import kuzu under the guard;
//! remove the exception code." Ruling Q8: the deprecated `migrate kuzu-data`
//! and `migrate kuzu-memory` aliases stay denied. A narrowed exception is
//! follow-up work, ranked before the #277 import window.
//! What: [`runs_kuzu_memory`] reads one argv. [`kuzu_import_call`] reads any
//! command text — a segment's argv, an unlexable segment, a here-document
//! body, a `bash -c` string — as statements of words, so no environment
//! prefix, wrapper, flag, case or quoting decides the answer.
//! Test: `pm_guard_kuzu_store_tests.rs` — `denies_running_kuzu_memory`,
//! `denies_the_deprecated_migrate_aliases`,
//! `denies_an_import_kuzu_after_an_environment_prefix`,
//! `denies_an_import_kuzu_inside_a_substitution_or_a_shell_string`,
//! `denies_an_import_kuzu_in_another_spelling`,
//! `keeps_unrelated_trusty_memory_commands_allowed`.

use crate::commands::pm_guard_kuzu_names::{names_kuzu_module, word_may_expand_to};
use crate::commands::pm_guard_secret_read::command_basename;
use crate::commands::program_word::resolve_program_word;

/// The program `kuzu-memory` installs.
const KUZU_PROGRAM: &str = "kuzu-memory";

/// The program whose `import kuzu` and `migrate` aliases deny (ruling Q8).
const TRUSTY_MEMORY: &str = "trusty-memory";
const MIGRATE_ALIASES: &[&str] = &["kuzu-data", "kuzu-memory"];

/// Programs that append their input to a command line (`xargs trusty-memory import`).
const ARGUMENT_FEEDERS: &[&str] = &["xargs", "gxargs", "parallel"];

/// Package runners that run a later argument as a program (`uvx kuzu-memory`).
const LAUNCHERS: &[&str] = &[
    "uvx",
    "uv",
    "pipx",
    "npx",
    "bunx",
    "pipenv",
    "poetry",
    "hatch",
    "rye",
    "pdm",
    "conda",
    "mamba",
    "micromamba",
    "pixi",
];

/// Whether `argv` runs the `kuzu-memory` program or its Python module.
///
/// What: the program word past wrappers, `xargs` included; a launcher's later
/// argument; a Python interpreter's `-m kuzu_memory`. A wrapper the resolver
/// cannot measure counts any `kuzu-memory` word (fail closed). A lookup
/// (`command -v kuzu-memory`) runs nothing.
/// Test: `denies_running_kuzu_memory`, `keeps_ordinary_commands_allowed`.
pub(crate) fn runs_kuzu_memory(argv: &[String]) -> bool {
    let start = match resolve_program_word(argv) {
        Ok(word) if word.lookup => return false,
        Ok(word) => word.index,
        Err(_) => return argv.iter().any(|t| is_kuzu_program(t)),
    };
    let Some(program) = argv.get(start) else {
        return false;
    };
    let base = command_basename(program).to_ascii_lowercase();
    let rest = &argv[start + 1..];
    if is_kuzu_program(program) {
        return true;
    }
    if LAUNCHERS.contains(&base.as_str()) {
        return rest.iter().any(|t| is_kuzu_program(t));
    }
    is_python(&base) && python_runs_kuzu_module(rest)
}

/// Whether `token` names the `kuzu-memory` program, a version pin included.
fn is_kuzu_program(token: &str) -> bool {
    let base = command_basename(token).to_ascii_lowercase();
    let name = base.split(['@', '=', '<', '>', '[', '~', '!']).next();
    name == Some(KUZU_PROGRAM)
}

/// Whether `base` is a Python interpreter (`python`, `python3.13`, `pypy3`).
fn is_python(base: &str) -> bool {
    ["python", "pypy"].iter().any(|stem| {
        base.strip_prefix(stem)
            .is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
    })
}

/// Whether an interpreter's `args` carry `-m kuzu_memory` anywhere.
///
/// What: `-m kuzu_memory`, `-mkuzu_memory` and a short cluster ending in `m`
/// (`-Im kuzu_memory`). Any position counts, so an option whose value count
/// is unknown (`-X dev`) cannot hide it; a script argument spelled the same
/// way denies too, on the safe side.
fn python_runs_kuzu_module(args: &[String]) -> bool {
    args.iter().enumerate().any(|(i, arg)| {
        if !arg.starts_with('-') || arg.starts_with("--") {
            return false;
        }
        let attached = arg
            .strip_prefix("-m")
            .is_some_and(|m| !m.is_empty() && names_kuzu_module(m));
        attached || (arg.ends_with('m') && args.get(i + 1).is_some_and(|m| names_kuzu_module(m)))
    })
}

/// The deny for a `trusty-memory import kuzu` or `migrate` alias call
/// anywhere in `text`; `None` when there is none.
///
/// Why: see the module doc — the call denies whatever surrounds it.
/// What: `text` loses its quotes and backslashes (a `\<newline>` joins) and
/// the `&` or `|` of a redirection operator, is cut into statements at `;`,
/// `&`, `|` and newlines, and each statement into words twice: at blanks,
/// parentheses and backticks, and again at `,`, `[`, `]`, `{` and `}` too (a
/// Python argv list). Redirections drop out ([`without_redirections`]), as
/// bash removes them before the program sees its argv; [`call_in`] judges
/// each word list.
/// Test: `denies_an_import_kuzu_after_an_environment_prefix`,
/// `denies_an_import_kuzu_inside_a_substitution_or_a_shell_string`,
/// `denies_an_import_kuzu_in_another_spelling`,
/// `denies_an_import_kuzu_split_by_a_redirection`,
/// `keeps_unrelated_trusty_memory_commands_allowed`.
pub(crate) fn kuzu_import_call(text: &str) -> Option<String> {
    let mut clean: String = text
        .replace("\\\n", " ")
        .chars()
        .filter(|c| !matches!(c, '\\' | '\'' | '"'))
        .collect();
    for (from, to) in REDIRECTION_SPELLINGS {
        clean = clean.replace(from, to);
    }
    let narrow = |c: char| c.is_whitespace() || "()`".contains(c);
    let wide = |c: char| narrow(c) || ",[]{}".contains(c);
    clean.split([';', '&', '|', '\n']).find_map(|statement| {
        let words = |cut: &dyn Fn(char) -> bool| -> Vec<String> {
            let words: Vec<String> = statement
                .split(cut)
                .filter(|w| !w.is_empty())
                .map(str::to_ascii_lowercase)
                .collect();
            without_redirections(words)
        };
        call_in(&words(&narrow)).or_else(|| call_in(&words(&wide)))
    })
}

/// Redirection spellings rewritten before the statement cut, so `&>`, `>&`,
/// `>|` and `<&` stay one operator and a process substitution's `<(` opens a
/// body rather than a redirection.
const REDIRECTION_SPELLINGS: [(&str, &str); 6] = [
    ("<(", " ("),
    (">(", " ("),
    ("&>", ">"),
    (">&", ">"),
    (">|", ">"),
    ("<&", "<"),
];

/// `words` minus each redirection: a word holding `<` or `>` goes, and so
/// does the next word when the operator stands alone (`2> /dev/null`): an fd
/// number, then only `<` and `>` (`2>1`, from `2>&1`, carries its target).
fn without_redirections(words: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(words.len());
    let mut target_next = false;
    for word in words {
        if std::mem::take(&mut target_next) {
            continue;
        }
        if word.contains(['<', '>']) {
            target_next = word
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .chars()
                .all(|c| c == '<' || c == '>');
            continue;
        }
        out.push(word);
    }
    out
}

/// The deny for the first `trusty-memory` call in one statement's `words`.
///
/// What: a word whose basename may be `trusty-memory`, then — skipping every
/// `-` word, so `-v`, `--verbose` and `--` hide nothing — a sub-command that
/// may be `import` followed by one that may be `kuzu`, or `migrate` followed
/// by an alias. Under an argument feeder (`xargs`) a missing word is supplied
/// at run time, so it denies (fail closed).
fn call_in(words: &[String]) -> Option<String> {
    let fed = words
        .iter()
        .any(|w| ARGUMENT_FEEDERS.contains(&command_basename(w).as_str()));
    for (at, word) in words.iter().enumerate() {
        if !word_may_expand_to(&command_basename(word), TRUSTY_MEMORY) {
            continue;
        }
        let rest: Vec<&String> = words[at + 1..]
            .iter()
            .filter(|w| !w.starts_with('-'))
            .collect();
        let deny = match rest.first() {
            None => fed,
            Some(sub) if may_be(sub, "import") => rest.get(1).map_or(fed, |t| names_kuzu(t)),
            Some(sub) if may_be(sub, "migrate") => {
                let in_slot = rest.get(1).filter(|t| is_migrate_alias(t, true));
                let anywhere = || rest[1..].iter().find(|t| is_migrate_alias(t, false));
                if let Some(alias) = in_slot.or_else(anywhere) {
                    return Some(migrate_reason(alias));
                }
                rest.len() == 1 && fed
            }
            Some(_) => false,
        };
        if deny {
            return Some(import_reason());
        }
    }
    None
}

/// Whether a sub-command word may be `literal`: a glob or brace reading of
/// it, or a `$` expansion the guard cannot read (fail closed).
fn may_be(word: &str, literal: &str) -> bool {
    word.contains('$') || word.contains("{}") || word_may_expand_to(word, literal)
}

/// Whether the word after `import` may be `kuzu` (`${X:-kuzu}` included).
fn names_kuzu(word: &str) -> bool {
    word.contains("kuzu") || may_be(word, "kuzu")
}

/// Whether `token` may be a deprecated `migrate` target, in any case or
/// spelling; `slot` (the word right after `migrate`) also takes a `$` word.
fn is_migrate_alias(token: &str, slot: bool) -> bool {
    let normal = token.to_ascii_lowercase().replace('_', "-");
    MIGRATE_ALIASES
        .iter()
        .any(|alias| word_may_expand_to(&normal, alias) || (slot && may_be(&normal, alias)))
}

/// The deny text for an `import kuzu` call.
fn import_reason() -> String {
    "kuzu-store-read: `trusty-memory import kuzu` is denied under the guard (#8625, \
     Architect ruling 2026-10-12): it opens kuzu-memory stores, and no exception is in \
     force. The operator runs the import outside the guard; the reviewer reads only the \
     masked ~/.trusty-tools/kuzu-review/<run>/masked.jsonl."
        .to_string()
}

/// The deny text for a deprecated `migrate` alias (ruling Q8).
fn migrate_reason(alias: &str) -> String {
    format!(
        "kuzu-store-read: `trusty-memory migrate {alias}` is a deprecated alias that reads \
         kuzu-memory stores (#8625, ruling Q8). It is denied under the guard, as is \
         `trusty-memory import kuzu`; the operator runs the import outside the guard."
    )
}
