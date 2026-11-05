//! The fail-closed half of the whole-server kill floor (#10665 r3).
//!
//! Why: Architect ruling df-D77. A kill word that no tmux hit accounts for
//! was let through: `awk 'BEGIN{system("tmux kill-server")}'`, `ssh host tmux
//! kill-server`, an alias, or a script a heredoc or `tee` writes and the same
//! command runs. The owner also wants no false refusal of a command that
//! only prints or searches for the words (#10548). Owner ruling Dac
//! (2026-11-02) keeps every such finding log-only; only a fleet kill a tmux
//! command spells is refused.
//! What: [`Walk::judge`] reads the shell text and, for every segment,
//! here-document body and command substitution that spells a kill word,
//! requires one of: a tmux command, judged here on its `-L`/`-S` words
//! (`scratch_server`) with each command-string argument judged in turn; a
//! shell `-c` string or `eval` text, judged in turn; or a known-inert program
//! printing to stdout (`echo`, `printf` without `-v`, `grep`, `rg` without
//! `--pre`, a `sed -n` without `-i` that only prints, `cat`, `git` with the
//! words only in the `-m` value of a subcommand that takes a message, #10706
//! H2), in a text whose every pipe feeds a reader (`head`, `wc`, …) that
//! writes and runs nothing (#10706 H1: `readers`), and where no other
//! segment writes and no `coproc` runs (#10706 C1: `output_redirected`).
//! A here-document body counts as an argument of the program its operator
//! line feeds; a substitution's output counts only while every substitution is
//! an inert program's argument (#10706 C1). A fleet kill is `Err`; anything
//! else is noted in [`Walk::soft`] and the walk goes on.
//! Test: `a_kill_word_no_tmux_hit_accounts_for_is_log_only`,
//! `a_kill_word_in_an_inert_argument_is_allowed`,
//! `wrapped_nested_and_chained_kills_are_refused`,
//! `inert_output_reaching_no_shell_keeps_its_verdict`, `heredoc_piped_to_shell_is_refused`,
//! `command_group_piped_to_shell_is_refused`, `loop_piped_to_shell_is_refused`,
//! `command_substitution_executed_is_refused`,
//! `inert_output_redirected_to_shell_is_refused`,
//! `subshell_output_redirected_to_shell_is_logged`,
//! `function_call_output_redirected_to_shell_is_logged`,
//! `grouped_exec_output_to_shell_is_logged`.

use super::super::super::architect_pane_parse::{quote_escaped_semicolons, tmux_hits};
use super::super::super::command_substitutions;
use super::super::super::floor_d4::{Segment, grammar_prefix, program_positions, segments};
use super::super::super::heredoc::{HeredocBodies, blank_spans};
use super::super::super::shell_lex::{QuoteScan, WrappedCommand, wrapped_command};
use super::readers::{reader_is_safe, sed_prints_only};
use super::{ServerEnv, judge_hit, names_whole_server_kill, socket};

/// Nesting depth (substitutions, command-string arguments) the walk reads.
const MAX_DEPTH: usize = 4;

/// Programs whose output a known-inert program may be piped into.
const READERS: &[&str] = &[
    "head", "tail", "wc", "sort", "uniq", "cut", "less", "more", "nl", "column", "cat", "grep",
    "egrep", "fgrep", "rg",
];

/// Shells whose `-c` string the walk judges as shell text (#10665, ruling Dac).
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

/// One walk over a command or script body (#10665, ruling Dac).
pub(super) struct Walk<'a> {
    /// The whole command or script body, which holds the assignments.
    script: &'a str,
    env: &'a ServerEnv,
    /// The first log-only finding: the call runs and the hook records it.
    pub(super) soft: Option<String>,
}

impl<'a> Walk<'a> {
    /// A walk over `script` under the hook's `env`.
    pub(super) fn new(script: &'a str, env: &'a ServerEnv) -> Self {
        Self {
            script,
            env,
            soft: None,
        }
    }

    /// Keep the first log-only finding.
    fn note(&mut self, why: impl Into<String>) {
        if self.soft.is_none() {
            self.soft = Some(why.into());
        }
    }

    /// `Err` naming why a kill word `text` spells kills the fleet's server or
    /// a `tm-` session; every other finding is noted and the walk goes on.
    pub(super) fn judge(&mut self, text: &str, depth: usize) -> Result<(), String> {
        if depth > MAX_DEPTH {
            self.note("it nests deeper than the guard reads");
            return Ok(());
        }
        let text = quote_escaped_semicolons(text);
        let scan = HeredocBodies::scan(&text);
        let spans: Vec<(usize, usize)> = scan.bodies().iter().map(|b| b.span).collect();
        let flat = blank_spans(&text, &spans);
        let segs = segments(&flat);
        // #10706 C1: printed text is inert only while no pipe in the text
        // feeds a non-reader (`{ echo …; } | sh`), and substitution output
        // only while each substitution is an inert program's argument.
        // #10706 C1: nor while a redirect may send it on (`{ …; } > x.sh`,
        // `true) > x.sh`, `f > >(sh)`, `{ exec > >(sh); }`, `coproc`).
        let readers_only = pipes_into_readers_only(&segs) && !output_redirected(&segs);
        let subs_printed = readers_only && substitutions_printed_only(&segs);
        for body in scan.bodies() {
            let (start, end) = body.span;
            if !names_whole_server_kill(&text[start..end]) {
                continue;
            }
            let feeds = inert_heredoc(&text, body.operator_line);
            if !(readers_only && feeds.is_some_and(|in_sub| !in_sub || subs_printed)) {
                self.note("a here-document names one, and the program it feeds may run or keep it");
            }
        }
        for sub in command_substitutions(&text) {
            if names_whole_server_kill(sub.text()) {
                self.judge(sub.text(), depth + 1)?;
                if !subs_printed {
                    self.note("a command substitution prints one where the shell may run it");
                }
            }
        }
        for seg in &segs {
            if !names_whole_server_kill(&without_substitutions(&seg.text)) {
                continue;
            }
            match wrapped_command(&seg.text) {
                // Its body is listed as the segments that follow.
                WrappedCommand::Inner(_) => continue,
                WrappedCommand::Unlexable => {
                    self.note("a wrapper the guard cannot read names one");
                    continue;
                }
                WrappedCommand::None => {}
            }
            let Some(argv) = shlex::split(&seg.text) else {
                self.note("a command naming one does not parse");
                continue;
            };
            let positions = program_positions(&argv);
            if let Some(&(pos, _)) = positions
                .iter()
                .find(|(_, p)| p.eq_ignore_ascii_case("tmux"))
            {
                self.tmux_command(&argv, pos, depth)?;
                continue;
            }
            // #10665 (ruling Dac): a fleet kill in `sh -c '…'` or `eval` text
            // is still a tmux command the guard reads.
            if let Some(inner) = shell_text(&argv, &positions) {
                self.judge(&inner, depth + 1)?;
                continue;
            }
            if !(readers_only && inert(&seg.text)) {
                let first = argv.first().map_or("", String::as_str);
                let program = positions.first().map_or(first, |(_, p)| p.as_str());
                self.note(format!(
                    "`{program}` may run it where the guard cannot read its server"
                ));
            }
        }
        Ok(())
    }

    /// Judge one tmux command: each hit of its argv, with the socket made
    /// explicit (`socket::explicit_argv`), then each command-string argument.
    fn tmux_command(&mut self, argv: &[String], pos: usize, depth: usize) -> Result<(), String> {
        let argv = socket::explicit_argv(argv, pos, self.script, self.env);
        let Some(text) = join(&argv) else {
            self.note("a tmux command naming one holds a NUL byte");
            return Ok(());
        };
        for hit in tmux_hits(&text) {
            if let Some(why) = hit.opaque
                && hit.targets.is_empty()
            {
                self.note(why);
                continue;
            }
            match judge_hit(&hit, self.env) {
                Ok(()) => {}
                Err(miss) if miss.fleet => return Err(miss.why),
                Err(miss) => self.note(miss.why),
            }
        }
        // A nested hit borrows the outer `-L`/`-S`; judge the string on its own.
        for word in argv.get(pos + 1..).unwrap_or_default() {
            if word.contains(char::is_whitespace) && names_whole_server_kill(word) {
                self.judge(word, depth + 1)?;
            }
        }
        Ok(())
    }
}

/// The shell text a segment runs in place: the `-c` string of a [`SHELLS`]
/// program, or the words after `eval`, joined. `None` for anything else.
fn shell_text(argv: &[String], positions: &[(usize, String)]) -> Option<String> {
    let [(pos, program)] = positions else {
        return None;
    };
    let args = argv.get(pos + 1..)?;
    if program == "eval" {
        return Some(args.join(" "));
    }
    if !SHELLS.contains(&program.as_str()) {
        return None;
    }
    let mut takes_string = false;
    for arg in args {
        if arg.starts_with('-') {
            takes_string |= !arg.starts_with("--") && arg.contains('c');
            continue;
        }
        return takes_string.then(|| arg.clone());
    }
    None
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
/// or `{` before its `<<` to the line's end. `Some(true)` when that command
/// opens at a `$(` or backtick, so the body becomes substitution output.
/// #10706 C1: `None` when the line ends in `|`, `|&`, `&&`, `||` or `\`,
/// since the pipeline goes on after the body (`cat <<EOF |` … `sh`).
fn inert_heredoc(text: &str, (start, end): (usize, usize)) -> Option<bool> {
    let line = &text[start..end];
    let tail = line.trim_end();
    if ["|", "|&", "&&", "\\"].iter().any(|op| tail.ends_with(op)) {
        return None;
    }
    let at = line
        .match_indices("<<")
        .map(|(i, _)| i)
        .find(|&i| !line[i + 2..].starts_with('<') && !line[..i].ends_with('<'))?;
    let from = ["$(", "`", "(", "|", ";", "&", "{"]
        .iter()
        .filter_map(|p| line[..at].rfind(p).map(|i| i + p.len()))
        .max()
        .unwrap_or(0);
    let segs = segments(&line[from..]);
    let (first, rest) = segs.split_first()?;
    let in_sub = line[..from].ends_with("$(") || line[..from].ends_with('`');
    (inert(&first.text) && pipes_into_readers_only(rest)).then_some(in_sub)
}

/// Whether every piped segment of `segs` is a reader that writes no file
/// and runs no program (#10706 H1: `readers::reader_is_safe`; C1: any pipe
/// in the text, since a group or loop's output reaches the pipe after it).
fn pipes_into_readers_only(segs: &[Segment]) -> bool {
    segs.iter().filter(|s| s.piped).all(|s| {
        let Some(argv) = shlex::split(&s.text) else {
            return false;
        };
        single_program(&argv).is_some_and(|(pos, p)| {
            let args = argv.get(pos + 1..).unwrap_or_default();
            READERS.contains(&p.as_str()) && reader_is_safe(&p, args)
        }) && !writes_file(&s.text)
    })
}

/// #10706 C1: whether a redirect may send printed output to a file, a process
/// substitution or a descriptor: any segment that writes and is not itself
/// an inert program naming the kill word (a group or loop closer, `exec`, a
/// function call, a subshell's last command), or any `coproc`.
fn output_redirected(segs: &[Segment]) -> bool {
    // #10706 C1: no match on the first word, which missed `true) > x.sh`,
    // `f > >(sh)` and `{ exec > >(sh)`; the extra records are accepted.
    segs.iter().any(|s| {
        let text = s.text.trim();
        opens_coproc(text)
            || (writes_file(text) && !(names_whole_server_kill(text) && prints(text)))
    })
}

/// #10706 C1: whether segment `text` starts a coprocess: `coproc` among its
/// leading shell grammar ([`grammar_prefix`]) or as the word after it. A
/// segment that does not lex is read word by word, failing closed.
fn opens_coproc(text: &str) -> bool {
    let is_coproc = |w: &str| w.trim_start_matches(['(', '{']) == "coproc";
    let Some(argv) = shlex::split(text) else {
        return text.split_whitespace().any(is_coproc);
    };
    let lead = grammar_prefix(&argv);
    argv.iter().take(lead + 1).any(|w| is_coproc(w.as_str()))
}

/// #10706 C1: whether every segment holding a command substitution is a
/// known-inert program, so substitution output is only printed. An
/// assignment (`x=$(…)`, run later as `$x`), `eval "$(…)"` or `sh <(…)` is
/// not.
fn substitutions_printed_only(segs: &[Segment]) -> bool {
    segs.iter()
        .all(|s| command_substitutions(&s.text).is_empty() || inert(&s.text))
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
    !writes_file(text) && prints(text)
}

/// Whether segment `text` runs a known-inert program, its redirects aside
/// (#10706 C1: `output_redirected` reads those).
fn prints(text: &str) -> bool {
    let Some(argv) = shlex::split(text) else {
        return false;
    };
    let Some((pos, program)) = single_program(&argv) else {
        return false;
    };
    let args = argv.get(pos + 1..).unwrap_or_default();
    match program.as_str() {
        "echo" | "cat" | "grep" | "egrep" | "fgrep" => true,
        // #10706 H1: `rg --pre` runs a program; `sed` may only print.
        "rg" => reader_is_safe(&program, args),
        "printf" => !args.iter().any(|a| a.starts_with("-v")),
        "sed" => sed_prints_only(args),
        "git" => only_in_messages(args),
        _ => false,
    }
}

/// `git` subcommands whose `-m` takes a message (#10706 H2); for `rebase`,
/// `-m` is `--merge` and the next word is read as an option.
const MESSAGE_SUBCOMMANDS: &[&str] = &["commit", "tag", "merge", "notes", "stash"];

/// `git` global options that take the next word as their value.
const GIT_VALUE_OPTIONS: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--config-env",
];

/// Whether every `git` argument naming a kill is a `-m`/`--message` value of
/// a [`MESSAGE_SUBCOMMANDS`] subcommand; every other argument is read.
fn only_in_messages(args: &[String]) -> bool {
    let mut at = 0;
    while let Some(arg) = args.get(at).filter(|a| a.starts_with('-')) {
        at += 1 + usize::from(GIT_VALUE_OPTIONS.contains(&arg.as_str()));
    }
    let at = at.min(args.len());
    // #10706 H2: the global options, their values and the subcommand are read.
    let head = &args[..(at + 1).min(args.len())];
    if head.iter().any(|a| names_whole_server_kill(a)) {
        return false;
    }
    let Some((subcommand, rest)) = args[at..].split_first() else {
        return true;
    };
    if !MESSAGE_SUBCOMMANDS.contains(&subcommand.as_str()) {
        return !rest.iter().any(|a| names_whole_server_kill(a));
    }
    let mut value = false;
    for arg in rest {
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
/// target is not `/dev/null`, `/dev/stdout`, `/dev/stderr`, `&1`, `&2` or
/// `&-`. #10706 C1: any other descriptor (`>&3`, `>&"${COPROC[1]}"`) may
/// reach a file or a shell, so it writes.
fn writes_file(text: &str) -> bool {
    let quotes = QuoteScan::new(text);
    text.match_indices('>').any(|(at, _)| {
        if !quotes.is_unquoted(at) || text[..at].ends_with('>') {
            return false;
        }
        let rest = text[at + 1..].trim_start_matches(['>', '|']).trim_start();
        let target = rest.split_whitespace().next().unwrap_or("");
        let target = target.trim_end_matches([')', '`']);
        !matches!(target, "&1" | "&2" | "&-" | "/dev/null" | "/dev/stdout" | "/dev/stderr")
    })
}
