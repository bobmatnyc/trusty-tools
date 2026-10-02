//! How a Bash command reaches tmux where [`super::architect_pane_parse`]
//! cannot read the tmux argv (#9001 critic r2).
//!
//! Why: tmux run through `xargs`, `find -exec … +`, or a shell that reads its
//! program on stdin gets words the guard never sees, and a here-document body
//! that is stdin data is not shell text at all.
//! What: [`without_data_bodies`] blanks the data bodies before the tmux scan;
//! [`opaque_route`] names the route of a program word whose tmux argv is
//! unreadable; [`dynamic_name`] and [`names_tmux`] decide when a program word
//! the shell expands counts; [`with_dynamics`] marks each word the shell
//! expands.
//! FAIL-CLOSED: each route is refused, not guessed at.
//! Test: `tmux_exact_target_tests.rs` (`every_opaque_or_dynamic_tmux_route_denies`,
//! `prose_and_a_literal_program_path_are_not_refused`,
//! `an_xargs_replacement_string_or_a_fed_shell_naming_tmux_denies`,
//! `a_data_body_an_evaluator_or_dynamic_program_runs_is_read`).

use super::architect_pane_parse::Word;
use super::architect_pane_verbs::DENY_VERBS;
use super::credential_print::{basename, code_operands, evaluator_name};
use super::floor_d4::{EXEC_FLAGS, program_positions, segments};
use super::heredoc::{blank_spans, data_bodies};
use super::shell_lex::DASH_C_SHELLS;
use crate::commands::hook_rewrite::is_env_assignment;

/// `command` with each here-document body that is stdin data blanked, so
/// prose in it (`The PM's tmux pane`) is neither parsed nor named.
///
/// What: [`data_bodies`] already keeps a body a shell reads as source. A body
/// stays too when its operator line names any evaluator (`> >(bash)`,
/// `python3 <<EOF`), or when it expands a `$(…)` or backtick. Every body
/// stays when the rest of the command names an evaluator or runs a program
/// word the shell expands: `read -d '' C <<EOF … eval "$C"` and
/// `while read a b; do $a $b; done <<EOF` run the body's words (#9001 r3).
/// Test: `prose_and_a_literal_program_path_are_not_refused`,
/// `a_data_body_an_evaluator_or_dynamic_program_runs_is_read`.
pub(super) fn without_data_bodies(command: &str) -> String {
    let spans: Vec<(usize, usize)> = data_bodies(command)
        .into_iter()
        .filter(|b| {
            let body = &command[b.span.0..b.span.1];
            let line = &command[b.operator_line.0..b.operator_line.1];
            let runs = !b.expands || !(body.contains("$(") || body.contains('`'));
            runs && !names_evaluator(line)
        })
        .map(|b| b.span)
        .collect();
    let blanked = blank_spans(command, &spans);
    // #9001 critic r3: a variable or a read loop carries the body to a program.
    if !spans.is_empty() && (names_evaluator(&blanked) || runs_dynamic_program(&blanked)) {
        return command.to_owned();
    }
    blanked
}

/// Whether a word of `line`, stripped of redirection and grouping marks,
/// names an evaluator.
fn names_evaluator(line: &str) -> bool {
    line.split_whitespace().any(|token| {
        let word = token.trim_matches(|c: char| "()<>|&;\"'".contains(c));
        evaluator_name(&basename(word)).is_some()
    })
}

/// Whether a segment of `text` runs a program word whose name the shell
/// expands (`$a $b`, `"$CMD" -x`).
fn runs_dynamic_program(text: &str) -> bool {
    segments(text).iter().any(|seg| {
        let Some(argv) = shlex::split(seg.text.trim()) else {
            return false;
        };
        let words = with_dynamics(&seg.text, argv);
        let texts: Vec<String> = words.iter().map(|w| w.text.clone()).collect();
        program_positions(&texts)
            .iter()
            .any(|(pos, _)| dynamic_name(&words[*pos]) && !is_env_assignment(&texts[*pos]))
    })
}

/// Whether `word` is a program word whose NAME the shell expands: its last
/// path part holds a `$` or backtick, or it has no `/` at all.
/// `~/.cargo/bin/tm` and `"$DIR/tm"` name `tm`; `$T` and `${T}ux` name nothing.
pub(super) fn dynamic_name(word: &Word) -> bool {
    let name = word.text.rsplit('/').next().unwrap_or(&word.text);
    word.dynamic && (!word.text.contains('/') || name.contains(['$', '`']))
}

/// Whether `text`, quotes and backslashes removed, names `tmux` in any case
/// (APFS runs `TMUX` as tmux, #9001 r3) or a deny verb.
pub(super) fn names_tmux(text: &str) -> bool {
    let flat: String = text
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect();
    flat.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .any(|w| w.eq_ignore_ascii_case("tmux") || DENY_VERBS.iter().any(|v| v.name == w))
}

/// #9001 critic r2: the route by which the program word at `pos` of `argv`
/// runs a tmux argv the guard cannot read, if any.
///
/// What: an `xargs` whose argv holds a `tmux` word (stdin becomes its
/// arguments), or one with a replacement string in a `command` that
/// [`names_tmux`] (stdin may become the program word); a `tmux` after a
/// `find -exec` that ends in `+` (paths become its arguments); an evaluator
/// with no inline code and no script operand whose stdin is a pipe or a `<`
/// redirection, here-string or process substitution, in a `command` that
/// [`names_tmux`]. The evaluator test reuses `credential_print_programs`
/// (`evaluator_name`, `code_operands`).
/// Test: `every_opaque_or_dynamic_tmux_route_denies`,
/// `an_xargs_replacement_string_or_a_fed_shell_naming_tmux_denies`.
pub(super) fn opaque_route(
    argv: &[String],
    pos: usize,
    piped: bool,
    command: &str,
) -> Option<&'static str> {
    let program = basename(&argv[pos]);
    let before = &argv[..pos];
    let after = &argv[pos + 1..];
    // The segment walk unwraps `xargs tmux …` into a bare `tmux …`, so the
    // `xargs` word itself is where the appended arguments show.
    if program == "xargs" && after.iter().any(|w| basename(w) == "tmux") {
        return Some("`xargs` appends arguments from stdin the guard cannot read");
    }
    // #9001 critic r3: `echo tmux | xargs -I{} env {} kill-server`.
    if program == "xargs" && xargs_replaces(after) && names_tmux(command) {
        return Some("`xargs` puts words from stdin the guard cannot read into its command");
    }
    if program == "tmux" {
        let in_exec = before.iter().any(|w| EXEC_FLAGS.contains(&w.as_str()));
        if in_exec && after.iter().any(|w| w == "+") {
            return Some("`find -exec … +` appends paths the guard cannot read");
        }
        return None;
    }
    let evaluator = evaluator_name(&program)?;
    let fed = piped
        || after.iter().any(|w| {
            w.trim_start_matches(|c: char| c.is_ascii_digit())
                .starts_with('<')
        });
    // #9001 critic r3: `cat log | bash scripts/report.sh tmux` runs the script.
    let reads_stdin = code_operands(evaluator, after).is_empty() && !runs_script(evaluator, after);
    (fed && reads_stdin && names_tmux(command))
        .then_some("a shell runs program text it reads on stdin")
}

/// Whether the `xargs` options in `args`, before its utility, set a
/// replacement string: `-I R`, `-J R`, `-i[R]` or `--replace[=R]`.
fn xargs_replaces(args: &[String]) -> bool {
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        if word == "--" || word == "-" || !word.starts_with('-') {
            return false;
        }
        if let Some(long) = word.strip_prefix("--") {
            if long.starts_with("replace") {
                return true;
            }
            if XARGS_LONG_VALUED.contains(&long) {
                words.next();
            }
            continue;
        }
        for (k, c) in word.char_indices().skip(1) {
            if "IJi".contains(c) {
                return true;
            }
            // A value-taking option ends the cluster; its value is the rest
            // of the word or, for a required one, the next word.
            if "adELnPsRS".contains(c) {
                if word[k + 1..].is_empty() {
                    words.next();
                }
                break;
            }
            if "el".contains(c) {
                break;
            }
        }
    }
    false
}

/// GNU `xargs` long options whose value may be the next word.
const XARGS_LONG_VALUED: &[&str] = &[
    "arg-file",
    "delimiter",
    "max-args",
    "max-procs",
    "max-chars",
    "process-slot-var",
];

/// Whether an evaluator's `args` name a script file it runs, so its stdin is
/// that script's data: the first operand past its options and redirections.
/// `-` and a shell's `-s` read stdin; `python -m` runs a module; a process
/// substitution is program text the guard cannot read.
fn runs_script(evaluator: &str, args: &[String]) -> bool {
    let shell = DASH_C_SHELLS.contains(&evaluator) || evaluator == "fish";
    let valued: &[char] = if shell {
        &['o', 'O']
    } else {
        &['W', 'X', 'r', 'I', 'M']
    };
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        let redirect = word.trim_start_matches(|c: char| c.is_ascii_digit());
        if redirect.starts_with("<(") || redirect.starts_with(">(") {
            return false;
        }
        if redirect.starts_with(['<', '>']) || redirect.starts_with("&>") {
            if redirect.trim_start_matches(['<', '>', '&']).is_empty() {
                words.next();
            }
            continue;
        }
        if word == "--" {
            return words.next().is_some();
        }
        if word == "-" {
            return false;
        }
        let Some(cluster) = word.strip_prefix(['-', '+']) else {
            return true;
        };
        if let Some(long) = cluster.strip_prefix('-') {
            if ["rcfile", "init-file", "require"].contains(&long) {
                words.next();
            }
            continue;
        }
        if shell && cluster.contains('s') {
            return false;
        }
        if evaluator.starts_with("python") && cluster.contains('m') {
            return true;
        }
        if cluster.ends_with(valued) {
            words.next();
        }
    }
    false
}

/// Pair each shlex word with whether the shell expands it.
pub(super) fn with_dynamics(segment: &str, argv: Vec<String>) -> Vec<Word> {
    let flags = word_dynamics(segment.trim());
    let aligned = flags.len() == argv.len();
    argv.into_iter()
        .enumerate()
        .map(|(i, text)| {
            let dynamic = if aligned {
                flags[i]
            } else {
                text.contains(['$', '`'])
            };
            Word { text, dynamic }
        })
        .collect()
}

/// For each unquoted-whitespace-separated word of `segment`, whether it holds
/// a `$` or backtick outside single quotes and not escaped, or starts with an
/// unquoted `~`.
fn word_dynamics(segment: &str) -> Vec<bool> {
    let (mut out, mut single, mut double, mut escaped) = (Vec::new(), false, false, false);
    let mut current: Option<bool> = None;
    for c in segment.chars() {
        if escaped {
            escaped = false;
            current.get_or_insert(false);
            continue;
        }
        let quoted = single || double;
        if !quoted && c.is_whitespace() {
            out.extend(current.take());
            continue;
        }
        let at_start = current.is_none();
        let live = current.get_or_insert(false);
        match c {
            '\\' if !single => escaped = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '$' | '`' if !single => *live = true,
            '~' if !quoted && at_start => *live = true,
            _ => {}
        }
    }
    out.extend(current);
    out
}
