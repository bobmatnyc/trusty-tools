//! The fail-closed half of the whole-server kill floor (#10665 r3).
//!
//! Why: Architect ruling df-D77. A kill word that no tmux hit accounts for
//! was let through: `awk 'BEGIN{system("tmux kill-server")}'`, `ssh host tmux
//! kill-server`, an alias, or a script a heredoc or `tee` writes and the same
//! command runs. The owner also wants no false refusal of a command that
//! only prints or searches for the words (#10548).
//! What: [`judge`] reads the shell text and, for every segment, here-document
//! body and command substitution that spells a kill word, requires one of:
//! a tmux command, judged here on its `-L`/`-S` words (`scratch_server`) with
//! each command-string argument judged in turn; or a known-inert program
//! printing to stdout (`echo`, `printf` without `-v`, `grep`/`rg`, `sed -n`
//! without `-i`, `cat`, `git` with the words only in a `-m` value), piped
//! only into readers (`head`, `wc`, …). A here-document body counts as an
//! argument of the program its operator line feeds. Anything else refuses.
//! Test: `a_kill_word_no_tmux_hit_accounts_for_is_refused`,
//! `a_kill_word_in_an_inert_argument_is_allowed`.

use super::super::super::architect_pane_parse::{quote_escaped_semicolons, tmux_hits};
use super::super::super::command_substitutions;
use super::super::super::floor_d4::{Segment, program_positions, segments};
use super::super::super::heredoc::{HeredocBodies, blank_spans};
use super::super::super::shell_lex::{QuoteScan, WrappedCommand, wrapped_command};
use super::{ServerEnv, judge_hit, names_whole_server_kill, socket};

/// Nesting depth (substitutions, command-string arguments) the walk reads.
const MAX_DEPTH: usize = 4;

/// Programs whose output a known-inert program may be piped into.
const READERS: &[&str] = &[
    "head", "tail", "wc", "sort", "uniq", "cut", "less", "more", "nl", "column", "cat", "grep",
    "egrep", "fgrep", "rg",
];

/// `Err` naming why a kill word `text` spells is not accounted for. `script`
/// is the whole command or script body, which holds the assignments.
pub(super) fn judge(text: &str, script: &str, depth: usize, env: &ServerEnv) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("it nests deeper than the guard reads".into());
    }
    let text = quote_escaped_semicolons(text);
    let scan = HeredocBodies::scan(&text);
    let mut spans = Vec::new();
    for body in scan.bodies() {
        let (start, end) = body.span;
        if names_whole_server_kill(&text[start..end]) && !inert_heredoc(&text, body.operator_line) {
            return Err(
                "a here-document names one, and the program it feeds may run or \
                 keep it"
                    .into(),
            );
        }
        spans.push(body.span);
    }
    for sub in command_substitutions(&text) {
        if names_whole_server_kill(sub.text()) {
            judge(sub.text(), script, depth + 1, env)?;
        }
    }
    let flat = blank_spans(&text, &spans);
    let segs = segments(&flat);
    for (n, seg) in segs.iter().enumerate() {
        if !names_whole_server_kill(&without_substitutions(&seg.text)) {
            continue;
        }
        match wrapped_command(&seg.text) {
            // Its body is listed as the segments that follow.
            WrappedCommand::Inner(_) => continue,
            WrappedCommand::Unlexable => {
                return Err("a wrapper the guard cannot read names one".into());
            }
            WrappedCommand::None => {}
        }
        let Some(argv) = shlex::split(&seg.text) else {
            return Err("a command naming one does not parse".into());
        };
        let positions = program_positions(&argv);
        if let Some(&(pos, _)) = positions
            .iter()
            .find(|(_, p)| p.eq_ignore_ascii_case("tmux"))
        {
            tmux_command(&argv, pos, script, depth, env)?;
            continue;
        }
        if !(inert(&seg.text) && reads_only(&segs[n + 1..])) {
            let first = argv.first().map_or("", String::as_str);
            let program = positions.first().map_or(first, |(_, p)| p.as_str());
            return Err(format!(
                "`{program}` may run it where the guard cannot read its server"
            ));
        }
    }
    Ok(())
}

/// Judge one tmux command: each hit of its argv, with the socket made
/// explicit (`socket::explicit_argv`), then each command-string argument.
fn tmux_command(
    argv: &[String],
    pos: usize,
    script: &str,
    depth: usize,
    env: &ServerEnv,
) -> Result<(), String> {
    let argv = socket::explicit_argv(argv, pos, script, env);
    let text = join(&argv).ok_or("a tmux command naming one holds a NUL byte")?;
    for hit in tmux_hits(&text) {
        if let Some(why) = hit.opaque
            && hit.targets.is_empty()
        {
            return Err(why.to_string());
        }
        judge_hit(&hit, env)?;
    }
    // A nested hit borrows the outer `-L`/`-S`; judge the string on its own.
    for word in argv.get(pos + 1..).unwrap_or_default() {
        if word.contains(char::is_whitespace) && names_whole_server_kill(word) {
            judge(word, script, depth + 1, env)?;
        }
    }
    Ok(())
}

/// `argv` as shell text that `tmux_hits` reads back word for word: a word
/// the shell expands stays expanded (double-quoted), every other word is
/// quoted literally.
fn join(argv: &[String]) -> Option<String> {
    let words: Option<Vec<String>> = argv
        .iter()
        .map(|w| {
            if w.contains(['$', '`']) {
                Some(format!(
                    "\"{}\"",
                    w.replace('\\', "\\\\").replace('"', "\\\"")
                ))
            } else {
                shlex::try_quote(w).ok().map(|q| q.into_owned())
            }
        })
        .collect();
    words.map(|w| w.join(" "))
}

/// `text` with each command substitution's body removed: the walk judges it.
fn without_substitutions(text: &str) -> String {
    command_substitutions(text)
        .iter()
        .fold(text.to_string(), |acc, sub| acc.replacen(sub.text(), "", 1))
}

/// Whether the here-document whose operator line is `line` feeds a
/// known-inert program: the command from the last `$(`, `(`, `|`, `;`, `&`
/// or `{` before its `<<` to the line's end.
fn inert_heredoc(text: &str, (start, end): (usize, usize)) -> bool {
    let line = &text[start..end];
    let Some(at) = line
        .match_indices("<<")
        .map(|(i, _)| i)
        .find(|&i| !line[i + 2..].starts_with('<') && !line[..i].ends_with('<'))
    else {
        return false;
    };
    let from = ["$(", "`", "(", "|", ";", "&", "{"]
        .iter()
        .filter_map(|p| line[..at].rfind(p).map(|i| i + p.len()))
        .max()
        .unwrap_or(0);
    let segs = segments(&line[from..]);
    segs.split_first()
        .is_some_and(|(first, rest)| inert(&first.text) && reads_only(rest))
}

/// Whether every segment `rest` pipes into is a reader that writes no file.
fn reads_only(rest: &[Segment]) -> bool {
    rest.iter().take_while(|s| s.piped).all(|s| {
        let program = shlex::split(&s.text).and_then(|argv| single_program(&argv));
        program.is_some_and(|(_, p)| READERS.contains(&p.as_str())) && !writes_file(&s.text)
    })
}

/// The one program `argv` runs, lower-cased, or `None` when the wrapper
/// reader offers more than one candidate.
fn single_program(argv: &[String]) -> Option<(usize, String)> {
    match program_positions(argv).as_slice() {
        [(pos, program)] => Some((*pos, program.to_ascii_lowercase())),
        _ => None,
    }
}

/// Whether segment `text` runs a known-inert program that prints the kill
/// word to stdout, never running or keeping it (see the module doc).
fn inert(text: &str) -> bool {
    let Some(argv) = shlex::split(text) else {
        return false;
    };
    let Some((pos, program)) = single_program(&argv) else {
        return false;
    };
    if writes_file(text) {
        return false;
    }
    let args = argv.get(pos + 1..).unwrap_or_default();
    let short = |a: &str, c: char| {
        a.starts_with('-')
            && !a.starts_with("--")
            && a[1..].chars().all(|x| x.is_ascii_alphabetic())
            && a.contains(c)
    };
    match program.as_str() {
        "echo" | "cat" | "grep" | "egrep" | "fgrep" | "rg" => true,
        "printf" => !args.iter().any(|a| a.starts_with("-v")),
        "sed" => {
            let quiet = args
                .iter()
                .any(|a| short(a, 'n') || a == "--quiet" || a == "--silent");
            let in_place = args
                .iter()
                .any(|a| short(a, 'i') || a.starts_with("-i") || a.starts_with("--in-place"));
            quiet && !in_place
        }
        "git" => only_in_messages(args),
        _ => false,
    }
}

/// Whether every `git` argument naming a kill is a `-m`/`--message` value.
fn only_in_messages(args: &[String]) -> bool {
    let mut value = false;
    for arg in args {
        if std::mem::take(&mut value) {
            continue;
        }
        let cluster = arg.starts_with('-')
            && !arg.starts_with("--")
            && arg.ends_with('m')
            && arg[1..].chars().all(|c| c.is_ascii_alphabetic());
        if cluster || arg == "--message" {
            value = true;
            continue;
        }
        let inline = arg.starts_with("--message=") || (arg.starts_with("-m") && arg.len() > 2);
        if !inline && names_whole_server_kill(arg) {
            return false;
        }
    }
    true
}

/// Whether segment `text` redirects output to a file: an unquoted `>` whose
/// target is not `/dev/null`, `/dev/stdout`, `/dev/stderr` or a descriptor.
fn writes_file(text: &str) -> bool {
    let quotes = QuoteScan::new(text);
    text.match_indices('>').any(|(at, _)| {
        if !quotes.is_unquoted(at) || text[..at].ends_with('>') {
            return false;
        }
        let rest = text[at + 1..].trim_start_matches(['>', '|']).trim_start();
        let target = rest.split_whitespace().next().unwrap_or("");
        !(target.starts_with('&') || matches!(target, "/dev/null" | "/dev/stdout" | "/dev/stderr"))
    })
}
