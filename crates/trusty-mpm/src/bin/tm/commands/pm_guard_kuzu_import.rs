//! The program side of the #8625 kuzu-store rule: running `kuzu-memory`, and
//! the one normalised-text rule on `trusty-memory` + `kuzu`.
//!
//! Why: `kuzu-memory memory export` dumps raw rows without naming a store
//! path, and `trusty-memory import kuzu --discover` finds stores without
//! naming one, so the path-word test alone sees neither. Architect ruling
//! 2026-10-12 15:42Z: every `trusty-memory import kuzu` denies under the
//! guard; ruling Q8 keeps the `migrate kuzu-data` and `migrate kuzu-memory`
//! aliases denied. Owner ruling D5b replaced the argv parser for those calls
//! with one text rule. The narrowed exception is #9794.
//! What: [`runs_kuzu_memory`] reads one argv. [`trusty_memory_kuzu_text`]
//! reads the whole Bash command as text.
//! Test: `pm_guard_kuzu_store_tests.rs` — `denies_running_kuzu_memory`,
//! `the_text_rule_denies_the_block_evasions`,
//! `the_text_rule_exempts_quoted_text_slots`.

use crate::commands::pm_guard_kuzu_names::names_kuzu_module;
use crate::commands::pm_guard_secret_positions::TEXT_PAYLOAD_FLAGS;
use crate::commands::pm_guard_secret_read::command_basename;
use crate::commands::program_word::resolve_program_word;

/// The program `kuzu-memory` installs.
const KUZU_PROGRAM: &str = "kuzu-memory";

/// The words whose joint presence in a normalised Bash command denies (D5b).
const CALL_WORDS: [&str; 2] = ["trusty-memory", "kuzu"];

/// Short text-slot flags beside [`TEXT_PAYLOAD_FLAGS`]: `git commit -m`.
const SHORT_SLOT_FLAGS: &[&str] = &["-m"];

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

/// The D5b deny for a Bash `command` that names both `trusty-memory` and
/// `kuzu`; `None` when it does not.
///
/// Why: owner ruling D5b, "Mistakes only; switch to a text rule". It catches
/// `trusty-memory import kuzu` and the `migrate kuzu-*` aliases however they
/// are wrapped, redirected, fed or quoted.
/// What: [`without_text_slots`] removes each quoted prose value, then the
/// text loses every `'`, `"`, `$` and `\` and is case-folded; the call denies
/// when the result holds every [`CALL_WORDS`] entry.
/// Test: `the_text_rule_denies_the_block_evasions`, `the_text_rule_folds_case`,
/// `the_text_rule_strips_quotes_dollars_and_backslashes`,
/// `the_text_rule_exempts_quoted_text_slots`,
/// `the_text_rule_denies_a_call_naming_both_outside_a_slot`.
pub(crate) fn trusty_memory_kuzu_text(command: &str) -> Option<String> {
    // See #8625 (D5b): the threat model is mistakes only, not deliberate evasion.
    let normal: String = without_text_slots(command)
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '$' | '\\'))
        .flat_map(char::to_lowercase)
        .collect();
    CALL_WORDS
        .iter()
        .all(|word| normal.contains(word))
        .then(text_rule_reason)
}

/// `command` with the quoted value of each text-slot flag replaced by a blank.
///
/// What: a slot is a [`TEXT_PAYLOAD_FLAGS`] or [`SHORT_SLOT_FLAGS`] spelling
/// that starts a word, then `=`, blanks or nothing, then a quoted string
/// ([`closing_quote`]). An unquoted value is not a slot.
// #9791: shared with the `file -f`/`-m` text rule.
pub(crate) fn without_text_slots(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    let mut copied = 0;
    while let Some((open, close)) = next_text_slot(command, copied) {
        out.push_str(&command[copied..open]);
        out.push(' ');
        copied = close + 1;
    }
    out.push_str(&command[copied..]);
    out
}

/// The byte offsets of the opening and closing quote of the first text slot
/// at or after `from`.
fn next_text_slot(command: &str, from: usize) -> Option<(usize, usize)> {
    let flags = || TEXT_PAYLOAD_FLAGS.iter().chain(SHORT_SLOT_FLAGS);
    command[from..].char_indices().find_map(|(offset, _)| {
        let at = from + offset;
        if !command[..at]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace)
        {
            return None;
        }
        flags().find_map(|flag| {
            let rest = command[at..].strip_prefix(flag)?;
            let value = rest
                .strip_prefix('=')
                .unwrap_or_else(|| rest.trim_start_matches([' ', '\t']));
            let open = command.len() - value.len();
            closing_quote(command, open).map(|close| (open, close))
        })
    })
}

/// The byte offset of the quote closing the string that opens at `open`.
///
/// What: `None` when `open` holds no quote, when the string never closes, or
/// when a double-quoted string runs a `$(…)` or backtick substitution, since
/// that is code, not text. Inside double quotes a backslash escapes one char.
fn closing_quote(command: &str, open: usize) -> Option<usize> {
    let quote = command[open..]
        .chars()
        .next()
        .filter(|c| matches!(c, '\'' | '"'))?;
    let start = open + 1;
    let mut escaped = false;
    for (offset, c) in command[start..].char_indices() {
        if quote == '"' && std::mem::take(&mut escaped) {
            continue;
        }
        if quote == '"' && c == '\\' {
            escaped = true;
        } else if c == quote {
            let close = start + offset;
            let body = &command[start..close];
            let runs = quote == '"' && (body.contains("$(") || body.contains('`'));
            return (!runs).then_some(close);
        }
    }
    None
}

/// The deny text for the D5b rule.
fn text_rule_reason() -> String {
    "kuzu-store-read: this Bash call names both `trusty-memory` and `kuzu` outside a quoted \
     `--body`, `--title`, `--message`, `--note` or `-m` value (#8625, D5b). `trusty-memory \
     import kuzu` and its `migrate kuzu-*` aliases open kuzu-memory stores; the operator runs \
     them outside the guard. To write about the call, quote it in one of those flags, or use \
     `--body-file` with a file the Write tool made."
        .to_string()
}
