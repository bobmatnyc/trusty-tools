//! The `tm fleet relaunch` floor of `tm hook --pm-guard` (#10033).
//!
//! Why: `tm fleet relaunch` (#9092) stops the Architect's `claude` and starts
//! a new one in its tmux pane. Owner ruling #8902 bars every session from
//! replacing that pane, and #9092 plan Q1 (accepted with ruling D6f) keeps
//! the verb to an operator terminal outside any hooked session — the
//! Architect included, so no exemption applies and neither bypass lifts it.
//! What: [`evaluate_fleet_relaunch`] walks every segment of a Bash command —
//! wrapper bodies (`sh -c`, `env -S`, `xargs`, `eval`), runners (`watch`,
//! `script`) and command substitutions — and judges each `tm` or
//! `trusty-mpm` program word, and the argv after `--` of a `cargo run` naming
//! one, with the `tm` clap definition itself, so prefixes (`tm fl rel`),
//! aliases and global options read exactly as `tm` reads them.
//! FAIL-CLOSED: a command or segment the guard cannot lex denies when it names
//! `tm`; a word the shell expands — or operands `xargs` or `find -exec {}`
//! supply — denies when it sits where the subcommand could still become
//! `fleet relaunch` (at the top level or under `fleet`).
//! Residual: a script file the command runs, a shell function or alias, a
//! shell reading commands from a pipe, and an interpreter (`python3 -c`).
//! Test: `fleet_relaunch_tests.rs`; end to end in
//! `tests/tm_hook_pm_guard_fleet_relaunch_10033.rs`.

use clap::{ArgMatches, CommandFactory};

use super::architect_pane_parse::Word;
use super::architect_pane_reach::{dynamic_name, runner_texts, with_dynamics, without_data_bodies};
use super::floor_d4::{EXEC_FLAGS, program_positions, segments};
use super::{Decoded, command_substitutions, decode_ansi_c, unclassifiable_command};
use crate::cli::Cli;

/// The rule name recorded with a `tm fleet relaunch` deny.
pub(crate) const FLEET_RELAUNCH_RULE: &str = "fleet-relaunch";

/// Nesting depth past which runner operands and substitutions are not read.
const MAX_DEPTH: usize = 3;

/// What one `tm` argv runs, as far as the guard can tell.
#[derive(Debug, PartialEq, Eq)]
enum Reading {
    /// `tm fleet relaunch`.
    Relaunch,
    /// Any other subcommand, or an argv clap refuses.
    Other,
    /// A word the shell expands sits where the subcommand is chosen.
    Unresolved(String),
}

/// Classify a Bash command against the `tm fleet relaunch` floor:
/// `Some(reason)` denies.
///
/// Why: the one entry point `pm_guard_floor` calls for a Bash command.
/// What: see the module doc. Every caller is bound, the Architect included.
/// Test: `every_spelling_of_tm_fleet_relaunch_is_denied`,
/// `a_relaunch_reached_through_a_wrapper_is_denied`,
/// `an_expanded_word_where_the_subcommand_goes_denies`,
/// `other_tm_commands_and_mentions_pass`.
pub(crate) fn evaluate_fleet_relaunch(command: &str) -> Option<String> {
    shell(command, 0)
}

/// Read `command` as shell text; the first deny wins.
fn shell(command: &str, depth: usize) -> Option<String> {
    let command = without_data_bodies(command);
    // #10033: a command the guard cannot read could run `tm fleet relaunch`;
    // judge what a `$'…'` quote spells (#9001). The universal #6660 floor
    // refuses what stays unclassifiable whether or not it names `tm`.
    if unclassifiable_command(&command).is_some() {
        return match decode_ansi_c(&command) {
            Decoded::Text(text) if unclassifiable_command(&text).is_none() => shell(&text, depth),
            _ => names_tm(&command).then(|| unreadable(command.trim())),
        };
    }
    for seg in segments(&command) {
        let Some(argv) = shlex::split(seg.text.trim()) else {
            if names_tm(&seg.text) {
                return Some(unreadable(seg.text.trim()));
            }
            continue;
        };
        let words = with_dynamics(&seg.text, argv);
        let texts: Vec<String> = words.iter().map(|w| w.text.clone()).collect();
        for (pos, base) in positions(&texts) {
            if let Some(reason) = judge_program(&words, &texts, pos, &base) {
                return Some(reason);
            }
            if depth < MAX_DEPTH {
                for text in runner_texts(&texts, pos) {
                    if let Some(reason) = shell(&text, depth + 1) {
                        return Some(reason);
                    }
                }
            }
        }
    }
    if depth < MAX_DEPTH {
        for body in command_substitutions(&command) {
            if let Some(reason) = shell(body.text(), depth + 1) {
                return Some(reason);
            }
        }
    }
    None
}

/// The program positions of `texts` ([`program_positions`]), plus each `tm`
/// word after an `xargs` word, which the shared walk does not place.
fn positions(texts: &[String]) -> Vec<(usize, String)> {
    let mut out = program_positions(texts);
    if let Some(from) = texts.iter().position(|w| is_xargs(w)) {
        for (at, word) in texts.iter().enumerate().skip(from + 1) {
            let base = basename(word);
            if is_tm(base) && !out.iter().any(|(p, _)| *p == at) {
                out.push((at, base.to_string()));
            }
        }
    }
    out
}

/// A word's basename, a leading `\` dropped.
fn basename(word: &str) -> &str {
    let word = word.strip_prefix('\\').unwrap_or(word);
    word.rsplit('/').next().unwrap_or(word)
}

/// Whether a word runs `xargs`.
fn is_xargs(word: &str) -> bool {
    matches!(basename(word), "xargs" | "gxargs")
}

/// The deny for the program word at `pos`, if it runs `tm fleet relaunch`.
///
/// What: a `tm`/`trusty-mpm` word (any case: APFS runs `TM` as `tm`) or a
/// `cargo run … --` naming one is read by [`read_tm_args`]. A program word
/// the shell expands is read the same way, but denies only on a definite
/// relaunch, since it may be any program.
fn judge_program(words: &[Word], texts: &[String], pos: usize, base: &str) -> Option<String> {
    let dynamic = dynamic_name(&words[pos]);
    let args = if dynamic || is_tm(base) {
        tm_operands(words, texts, pos)
    } else if base == "cargo" {
        cargo_run_operands(words, texts, pos)?
    } else {
        return None;
    };
    match read_tm_args(&args) {
        Reading::Relaunch => Some(denied()),
        Reading::Unresolved(word) if !dynamic => Some(unresolved(&word)),
        _ => None,
    }
}

/// Whether a program basename runs the `tm` CLI.
fn is_tm(base: &str) -> bool {
    base.eq_ignore_ascii_case("tm") || base.eq_ignore_ascii_case("trusty-mpm")
}

/// The words `tm` at `pos` receives.
///
/// What: the words after it. Under `find -exec` they stop at the `;`/`+`
/// terminator and each `{}` is a path `find` supplies; under `xargs` an
/// unknown tail from stdin follows. Both supplied forms read as expanded.
fn tm_operands(words: &[Word], texts: &[String], pos: usize) -> Vec<Word> {
    let mut args: Vec<Word> = words[pos + 1..].to_vec();
    let under_exec = pos > 0 && EXEC_FLAGS.contains(&texts[pos - 1].as_str());
    if under_exec {
        if let Some(end) = args.iter().position(|w| w.text == ";" || w.text == "+") {
            args.truncate(end);
        }
        for word in &mut args {
            word.dynamic |= word.text.contains("{}");
        }
    }
    if texts[..pos].iter().any(|w| is_xargs(w)) {
        args.push(Word {
            text: "(xargs operands)".into(),
            dynamic: true,
        });
    }
    args
}

/// The `tm` argv of a `cargo run` naming `tm` or `trusty-mpm`: the words
/// after its first `--`. `None` for any other cargo command.
fn cargo_run_operands(words: &[Word], texts: &[String], pos: usize) -> Option<Vec<Word>> {
    let rest = &texts[pos + 1..];
    let dashes = rest.iter().position(|w| w == "--")?;
    let before = &rest[..dashes];
    let runs = before.iter().any(|w| w == "run" || w == "r");
    let names_tm = before
        .iter()
        .any(|w| is_tm(w.rsplit('=').next().unwrap_or(w)));
    (runs && names_tm).then(|| words[pos + 2 + dashes..].to_vec())
}

/// Read a `tm` argv (program word excluded) with the `tm` clap definition.
///
/// What: with no expanded word, clap parses the argv strictly; a refused argv
/// never runs, so it is [`Reading::Other`]. Otherwise clap reads the words
/// before the first expanded one with errors ignored: when they already
/// choose a leaf subcommand, or a group other than `fleet`, the expanded
/// words cannot reach `fleet relaunch`; at the top level, under `fleet`, or
/// when even that prefix fails, the reading is [`Reading::Unresolved`].
/// Test: `an_expanded_word_where_the_subcommand_goes_denies`,
/// `an_expanded_word_after_another_subcommand_passes`.
fn read_tm_args(args: &[Word]) -> Reading {
    let first_dynamic = args.iter().position(|w| w.dynamic);
    let fixed = &args[..first_dynamic.unwrap_or(args.len())];
    let argv = std::iter::once("tm").chain(fixed.iter().map(|w| w.text.as_str()));
    let Some(at) = first_dynamic else {
        return match Cli::command().try_get_matches_from(argv) {
            Ok(m) if subcommand_path(&m) == ["fleet", "relaunch"] => Reading::Relaunch,
            _ => Reading::Other,
        };
    };
    let path = Cli::command()
        .ignore_errors(true)
        .try_get_matches_from(argv)
        .map(|m| subcommand_path(&m))
        .unwrap_or_default();
    let path: Vec<&str> = path.iter().map(String::as_str).collect();
    match path.as_slice() {
        ["fleet", "relaunch"] => Reading::Relaunch,
        [] | ["fleet"] => Reading::Unresolved(args[at].text.clone()),
        _ => Reading::Other,
    }
}

/// The chain of subcommand names `m` selected, outermost first.
fn subcommand_path(m: &ArgMatches) -> Vec<String> {
    let mut path = Vec::new();
    let mut at = m;
    while let Some((name, sub)) = at.subcommand() {
        path.push(name.to_string());
        at = sub;
    }
    path
}

/// Whether `text`, quotes and backslashes removed, names `tm` or
/// `trusty-mpm` as a word, in any case.
fn names_tm(text: &str) -> bool {
    let flat: String = text
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect();
    flat.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .any(is_tm)
}

/// The closing sentences every `tm fleet relaunch` deny carries.
const REMEDY: &str = "`tm fleet relaunch` stops and replaces the Architect's `claude` in its \
     tmux pane, so no hooked session may run it, the Architect included (owner ruling #8902; \
     #9092 plan Q1). Run it from an operator terminal outside any Claude Code session. \
     `TRUSTY_MPM_PM_UNRESTRICTED` / `TRUSTY_MPM_DISABLE_HOOKS` do not lift this floor.";

/// The deny for a command that runs `tm fleet relaunch`.
fn denied() -> String {
    format!("Hard-floor deny (#10033): this command runs `tm fleet relaunch`. {REMEDY}")
}

/// The deny for a `tm` subcommand the shell chooses.
fn unresolved(word: &str) -> String {
    format!(
        "Hard-floor deny (#10033): this command runs `tm` with `{word}` where the subcommand \
         is chosen, and the guard cannot rule out `tm fleet relaunch`. Spell the subcommand \
         literally. {REMEDY}"
    )
}

/// The deny for a command naming `tm` that the guard cannot lex.
fn unreadable(text: &str) -> String {
    let text: String = text.chars().take(80).collect();
    format!(
        "Hard-floor deny (#10033): this command names `tm`, but the guard cannot parse \
         `{text}`, so it cannot rule out `tm fleet relaunch`. {REMEDY}"
    )
}

#[cfg(test)]
#[path = "fleet_relaunch_tests.rs"]
mod tests;
