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
//! `trusty-mpm` program word, the utility an `xargs` runs, and the argv a
//! `cargo run` naming one hands the binary, with the `tm` clap definition
//! itself, so prefixes (`tm fl rel`), aliases and global options read exactly
//! as `tm` reads them. Shell syntax bash strips first — redirections, a
//! background `&`, group closers — is dropped before the read
//! ([`argv::shell_words`]).
//! FAIL-CLOSED: a command or segment the guard cannot lex, and text nested
//! past [`MAX_DEPTH`], denies when the command names `tm` (a `$'…'` quote is
//! decoded first, #9001); a word the shell expands — a variable, a
//! substitution, a brace expansion or a glob, or operands `xargs` or
//! `find -exec {}` supply — denies when it sits where the subcommand could
//! still become `fleet relaunch` (at the top level or under `fleet`). An
//! expanded program word denies on a definite relaunch, or on an unresolved
//! one when the command names `tm`. A command naming `tm` that feeds program
//! text the guard cannot read to a shell (`| sh`, `bash <<<`), an `xargs`
//! replacement string or wrapper, or `parallel` denies ([`fed_route`],
//! shared with the #8902 floor). Whether a command names `tm` is read on the
//! root command, so a substitution inherits it.
//! Residual: a script file the command runs, a shell function or alias, an
//! interpreter's inline code (`python3 -c`), a renamed or symlinked binary,
//! and a wrapper nested past the shared segment walk's depth.
//! Test: `fleet_relaunch_tests.rs`; end to end in
//! `tests/tm_hook_pm_guard_fleet_relaunch_10033.rs`.

use super::architect_pane_parse::Word;
use super::architect_pane_reach::{
    FedRoute, dynamic_name, fed_route, runner_texts, with_dynamics, without_data_bodies,
};
use super::floor_d4::{EXEC_FLAGS, program_positions, segments};
use super::shell_lex::xargs_utility_index;
use super::{Decoded, command_substitutions, decode_ansi_c, unclassifiable_command};

#[path = "fleet_relaunch_argv.rs"]
mod argv;

use argv::{Reading, cargo_run_argvs, read_tm_args, shell_words};

/// The rule name recorded with a `tm fleet relaunch` deny.
pub(crate) const FLEET_RELAUNCH_RULE: &str = "fleet-relaunch";

/// Nesting depth past which runner operands and substitutions are not read;
/// text past it denies when the command names `tm`.
const MAX_DEPTH: usize = 3;

/// One program word of a segment.
struct Program {
    /// Its index in the segment's words.
    pos: usize,
    /// Its basename.
    base: String,
    /// Whether an `xargs` runs it, appending words from stdin.
    xargs: bool,
}

/// Classify a Bash command against the `tm fleet relaunch` floor:
/// `Some(reason)` denies.
///
/// Why: the one entry point `pm_guard_floor` calls for a Bash command.
/// What: see the module doc. Every caller is bound, the Architect included.
/// Test: `every_spelling_of_tm_fleet_relaunch_is_denied`,
/// `a_relaunch_reached_through_a_wrapper_is_denied`,
/// `an_expanded_word_where_the_subcommand_goes_denies`,
/// `every_unreadable_arm_naming_tm_denies`,
/// `shell_syntax_around_a_relaunch_does_not_hide_it`,
/// `an_opaque_route_in_a_command_naming_tm_denies`,
/// `other_tm_commands_and_mentions_pass`.
pub(crate) fn evaluate_fleet_relaunch(command: &str) -> Option<String> {
    // #10033 critic r1: read once on the root, so `T=tm; echo $($T …)` counts.
    let named = names_tm(&without_data_bodies(command));
    shell(command, 0, named)
}

/// Read `command` as shell text; the first deny wins. `named` says whether
/// the root command names `tm`.
fn shell(command: &str, depth: usize, named: bool) -> Option<String> {
    let command = without_data_bodies(command);
    let named = named || names_tm(&command);
    // #10033 fail-closed: past the depth bound, a command naming `tm` denies.
    if depth > MAX_DEPTH {
        return named.then(|| unreadable(command.trim()));
    }
    // #10033: a command the guard cannot read could run `tm fleet relaunch`;
    // judge what a `$'…'` quote spells (#9001). The universal #6660 floor
    // refuses what stays unclassifiable whether or not it names `tm`.
    if unclassifiable_command(&command).is_some() {
        return match decode_ansi_c(&command) {
            Decoded::Text(text) if unclassifiable_command(&text).is_none() => {
                shell(&text, depth, named)
            }
            _ => named.then(|| unreadable(command.trim())),
        };
    }
    for seg in segments(&command) {
        let Some(argv) = shlex::split(seg.text.trim()) else {
            if names_tm(&seg.text) {
                return Some(unreadable(seg.text.trim()));
            }
            continue;
        };
        let read = shell_words(&seg.text, with_dynamics(&seg.text, argv));
        let texts: Vec<String> = read.words.iter().map(|w| w.text.clone()).collect();
        for program in positions(&texts) {
            if let Some(reason) = judge_program(&read.words, &texts, &program, named) {
                return Some(reason);
            }
            // #10033 critic r1: `| sh`, `bash <<<`, `xargs -I{}`, `parallel`.
            let fed = seg.piped || read.fed;
            let names =
                |route| named && (route != FedRoute::XargsReplace || replaces_tm(&texts, &program));
            if let Some(why) = fed_route(&texts, program.pos, fed, &names) {
                return Some(opaque(why));
            }
            for text in runner_texts(&texts, program.pos) {
                if let Some(reason) = shell(&text, depth + 1, named) {
                    return Some(reason);
                }
            }
        }
    }
    command_substitutions(&command)
        .iter()
        .find_map(|body| shell(body.text(), depth + 1, named))
}

/// The program words of `texts` ([`program_positions`]), plus the utility
/// each `xargs` program runs and the programs it wraps.
///
/// Why: #10033 critic r1 — a `tm` word anywhere after an `xargs` word is not
/// a program (`git ls-files | xargs grep -l trusty-mpm`).
/// Test: `a_tm_word_xargs_does_not_run_passes`.
fn positions(texts: &[String]) -> Vec<Program> {
    let mut out: Vec<Program> = program_positions(texts)
        .into_iter()
        .map(|(pos, base)| Program {
            pos,
            base,
            xargs: false,
        })
        .collect();
    let mut at = 0;
    while at < out.len() {
        if matches!(out[at].base.as_str(), "xargs" | "gxargs") {
            let start = out[at].pos + 1;
            let utility = start + xargs_utility_index(&texts[start..]);
            let run = texts.get(utility..).map(program_positions);
            for (pos, base) in run.unwrap_or_default() {
                let pos = pos + utility;
                match out.iter_mut().find(|p| p.pos == pos) {
                    Some(known) => known.xargs = true,
                    None => out.push(Program {
                        pos,
                        base,
                        xargs: true,
                    }),
                }
            }
        }
        at += 1;
    }
    out
}

/// Whether the `xargs` at `program` can put stdin words into a `tm` command
/// through its replacement string: shell code among its words names `tm`
/// (`sh -c 'tm fleet {}'`), or the replacement is the utility word itself
/// (`xargs -I{} {} fleet …`).
///
/// Why: #10033 critic r1 — `git ls-files crates/trusty-mpm | xargs -I{} wc
/// -l {}` names `tm` only in a path and must pass.
/// Test: `a_tm_word_xargs_does_not_run_passes`,
/// `an_opaque_route_in_a_command_naming_tm_denies`.
fn replaces_tm(texts: &[String], program: &Program) -> bool {
    let after = &texts[program.pos + 1..];
    let at = xargs_utility_index(after);
    let code = after
        .iter()
        .any(|w| w.contains(char::is_whitespace) && names_tm(w));
    let utility = after.get(at).map_or("", String::as_str);
    let replaced = !utility.is_empty()
        && (utility.contains("{}") || after[..at].iter().any(|o| o.ends_with(utility)));
    code || replaced
}

/// The deny for `program`, if it runs `tm fleet relaunch`.
///
/// What: a `tm`/`trusty-mpm` word (any case: APFS runs `TM` as `tm`) or a
/// `cargo run` naming one is read by [`read_tm_args`]. A program word the
/// shell expands is read the same way; it may be any program, so an
/// unresolved reading denies only when the command names `tm` (`named`).
fn judge_program(
    words: &[Word],
    texts: &[String],
    program: &Program,
    named: bool,
) -> Option<String> {
    let dynamic = expanded_program(&words[program.pos]);
    let argvs = if dynamic || is_tm(&program.base) {
        vec![tm_operands(words, texts, program)]
    } else if program.base == "cargo" {
        cargo_run_argvs(words, program.pos)
    } else {
        return None;
    };
    argvs.iter().find_map(|args| match read_tm_args(args) {
        Reading::Relaunch => Some(denied()),
        Reading::Unresolved(word) if !dynamic || named => Some(unresolved(&word)),
        _ => None,
    })
}

/// Whether a program word's name is expanded: [`dynamic_name`], or a brace
/// expansion or glob in its last path part (#10033 critic r1).
fn expanded_program(word: &Word) -> bool {
    let name = word.text.rsplit('/').next().unwrap_or(&word.text);
    dynamic_name(word) || (word.dynamic && name.contains(['*', '?', '[', '{']))
}

/// Whether a program basename runs the `tm` CLI.
fn is_tm(base: &str) -> bool {
    base.eq_ignore_ascii_case("tm") || base.eq_ignore_ascii_case("trusty-mpm")
}

/// The words `tm` at `program` receives.
///
/// What: the words after it. Under `find -exec` they stop at the `;`/`+`
/// terminator and each `{}` is a path `find` supplies; under `xargs` an
/// unknown tail from stdin follows. Both supplied forms read as expanded.
fn tm_operands(words: &[Word], texts: &[String], program: &Program) -> Vec<Word> {
    let pos = program.pos;
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
    if program.xargs {
        args.push(Word {
            text: "(xargs operands)".into(),
            dynamic: true,
        });
    }
    args
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

/// The deny for a command naming `tm` that feeds a program text the guard
/// cannot read ([`fed_route`]).
fn opaque(why: &str) -> String {
    format!(
        "Hard-floor deny (#10033): this command names `tm`, and {why}, so the guard cannot \
         rule out `tm fleet relaunch`. {REMEDY}"
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
