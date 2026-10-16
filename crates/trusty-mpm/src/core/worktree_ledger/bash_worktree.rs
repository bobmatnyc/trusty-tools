//! Finding `git worktree add|remove|prune` in a Bash tool call (#8994 S2b).
//!
//! Why: an agent that runs `git worktree add` itself creates a tree that no tm
//! route and no harness hook announces, so the ledger learnt of it only at the
//! next backfill. The `PostToolUse` relay carries the command text and the
//! `cwd`; reading them is the only way to record the tree when it appears.
//! The PM Bash guard's tokenizer lives in the `tm` binary, which the daemon
//! library cannot use, so this is a small parser of its own.
//! What: [`parse_worktree_command`] splits a command into simple commands
//! (on `;`, `&&`, `||`, `|`, `&`, newlines, parentheses and backticks outside
//! quotes), drops comments and here-document bodies, follows `cd`, skips
//! leading `NAME=value` assignments and wrapper words, and reads each `git`
//! call's `-C` options and `worktree` verb. A call whose path depends on the
//! shell (`$VAR`, `~`, an unbalanced quote, an unknown directory) is an error,
//! never a guess. Pure: no filesystem, no git. Whether a call did anything is
//! decided later, on disk and through `git worktree list`
//! ([`super::command_ingest`]).
//! Test: `core::worktree_ledger::bash_worktree::tests`.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// One `git worktree` call the command runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeOp {
    /// `git worktree add <path>`: where the new tree may be, most likely first.
    Add(Vec<PathBuf>),
    /// `git worktree remove <path>`: where the removed tree may have been.
    Remove(Vec<PathBuf>),
    /// `git worktree prune` (not `-n` / `--dry-run`).
    Prune,
}

/// What [`parse_worktree_command`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedCommand {
    /// The `git worktree` calls, in command order.
    pub ops: Vec<WorktreeOp>,
    /// Calls that name `git worktree` but could not be resolved.
    pub errors: Vec<String>,
}

/// Words that run the next word as the command (`env FOO=1 git …`).
const PREFIX_WORDS: &[&str] = &[
    "env", "command", "builtin", "exec", "nohup", "time", "sudo", "{", "!", "if", "then", "else",
    "elif", "do", "while", "until",
];

/// Git global options that take the next word as their value.
const GIT_VALUE_OPTS: &[&str] = &[
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--config-env",
];

/// `git worktree add` options that take the next word as their value.
const ADD_VALUE_OPTS: &[&str] = &["-b", "-B", "--reason"];

/// The `git worktree` calls `command` runs, with each path made absolute.
///
/// Why: see the module doc.
/// What: `cwd` is the hook's working directory; a relative path with no
/// absolute `cwd` is an error. Each `Add` / `Remove` carries the path resolved
/// against the directory the shell was in (after `cd` and `-C`), then — when a
/// relative `cd` moved it — the same path resolved against the unmoved `cwd`,
/// because the hook may report `cwd` before or after the `cd` took effect.
/// `prune -n` / `--dry-run`, other verbs, and `add` with no path yield nothing.
/// Test: `real_worktree_calls_resolve_to_absolute_paths`,
/// `mentions_of_the_words_are_not_calls`,
/// `a_relative_cd_adds_the_unmoved_cwd_as_a_second_candidate`,
/// `unresolvable_calls_are_reported_not_guessed`.
pub fn parse_worktree_command(command: &str, cwd: Option<&Path>) -> ParsedCommand {
    let mut parsed = ParsedCommand::default();
    let base = cwd.filter(|c| c.is_absolute()).map(normalize);
    let mut shell = Dirs {
        dir: base.clone(),
        alt: None,
    };
    for segment in segments(command) {
        let Some(words) = shlex::split(&segment) else {
            if segment.contains("worktree") {
                parsed.errors.push(format!(
                    "cannot split `{}`: unbalanced quote",
                    segment.trim()
                ));
            }
            continue;
        };
        let words = strip_prefix_words(&words);
        match words.first().map(String::as_str) {
            Some("cd" | "pushd") => {
                let target = words[1..].iter().find(|w| !w.starts_with('-'));
                let absolute = target.is_some_and(|t| t.starts_with('/'));
                shell.dir = target.and_then(|t| join(shell.dir.as_deref(), t));
                // #8994: after a `cd` that is not an absolute literal, the hook's
                // own cwd (which may already reflect it) is a fallback.
                if !absolute && shell.alt.is_none() {
                    shell.alt = base.clone();
                }
            }
            Some("popd") => shell.dir = None,
            Some(program) if Path::new(program).file_name() == Some(OsStr::new("git")) => {
                git_call(&mut parsed, &words[1..], &shell);
            }
            _ => {}
        }
    }
    parsed
}

/// The directory the shell is in, and the fallback after a relative `cd`.
struct Dirs {
    dir: Option<PathBuf>,
    alt: Option<PathBuf>,
}

/// Read one `git …` call (the words after `git`).
fn git_call(parsed: &mut ParsedCommand, args: &[String], shell: &Dirs) {
    let mut dir = shell.dir.clone();
    let mut alt = shell.alt.clone();
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        if arg == "-C" {
            let Some(value) = args.get(i + 1) else { return };
            dir = join(dir.as_deref(), value);
            alt = join(alt.as_deref(), value);
            i += 2;
        } else if GIT_VALUE_OPTS.contains(&arg.as_str()) {
            i += 2;
        } else if arg.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    if args.get(i).map(String::as_str) != Some("worktree") {
        return;
    }
    let verb = args.get(i + 1).map(String::as_str);
    let rest = args.get(i + 2..).unwrap_or_default();
    let (make, value_opts): (fn(Vec<PathBuf>) -> WorktreeOp, &[&str]) = match verb {
        Some("add") => (WorktreeOp::Add, ADD_VALUE_OPTS),
        Some("remove") => (WorktreeOp::Remove, &[]),
        Some("prune") => {
            if !rest.iter().any(|a| a == "-n" || a == "--dry-run") {
                parsed.ops.push(WorktreeOp::Prune);
            }
            return;
        }
        _ => return,
    };
    let Some(path) = first_positional(rest, value_opts) else {
        return;
    };
    let mut candidates: Vec<PathBuf> = [dir, alt]
        .iter()
        .filter_map(|d| join(d.as_deref(), path))
        .collect();
    candidates.dedup();
    if candidates.is_empty() {
        parsed.errors.push(format!(
            "cannot resolve `git worktree {}` path `{path}` without running a shell",
            verb.unwrap_or_default()
        ));
    } else {
        parsed.ops.push(make(candidates));
    }
}

/// The first non-option word, skipping the values of `value_opts`.
fn first_positional<'a>(args: &'a [String], value_opts: &[&str]) -> Option<&'a str> {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        if arg == "--" {
            return args.get(i + 1).map(String::as_str);
        }
        if value_opts.contains(&arg.as_str()) {
            i += 2;
        } else if arg.starts_with('-') {
            i += 1;
        } else {
            return Some(arg);
        }
    }
    None
}

/// Drop leading `NAME=value` assignments and [`PREFIX_WORDS`].
fn strip_prefix_words(words: &[String]) -> &[String] {
    let skip = words
        .iter()
        .take_while(|w| PREFIX_WORDS.contains(&w.as_str()) || is_assignment(w))
        .count();
    &words[skip..]
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// `word` resolved against `dir`; `None` when the shell would expand it or
/// `dir` is unknown.
fn join(dir: Option<&Path>, word: &str) -> Option<PathBuf> {
    if word.is_empty() || word == "-" || word.starts_with('~') || word.contains(['$', '`']) {
        return None;
    }
    let path = Path::new(word);
    if path.is_absolute() {
        Some(normalize(path))
    } else {
        dir.map(|d| normalize(&d.join(path)))
    }
}

/// `path` with `.` dropped and `..` applied lexically (never above `/`).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Characters that end a simple command outside quotes.
fn is_separator(c: char) -> bool {
    matches!(c, ';' | '|' | '(' | ')' | '`')
}

/// Split `command` into simple commands, quotes kept for `shlex`.
///
/// What: tracks single quotes, double quotes and backslash escapes; drops a
/// `#` comment that starts a word, a backslash-newline, and every
/// here-document body (`<<WORD`, `<<-WORD`, quoted delimiters; `<<<` is a
/// here-string and is kept). `&` splits unless it is part of a redirection
/// (`2>&1`, `&>`). An unclosed quote leaves the rest in one segment, which
/// `shlex` then refuses.
fn segments(command: &str) -> Vec<String> {
    let chars: Vec<char> = command.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut heredocs: Vec<(String, bool)> = Vec::new();
    let (mut single, mut double) = (false, false);
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        i += 1;
        if single {
            single = c != '\'';
            cur.push(c);
            continue;
        }
        if double {
            cur.push(c);
            if c == '\\' {
                cur.extend(chars.get(i));
                i += 1;
            } else if c == '"' {
                double = false;
            }
            continue;
        }
        match c {
            '\'' => {
                single = true;
                cur.push(c);
            }
            '"' => {
                double = true;
                cur.push(c);
            }
            '\\' if chars.get(i) == Some(&'\n') => i += 1,
            '\\' => {
                cur.push(c);
                cur.extend(chars.get(i));
                i += 1;
            }
            '#' if cur.is_empty() || cur.ends_with(char::is_whitespace) => {
                while chars.get(i).is_some_and(|&n| n != '\n') {
                    i += 1;
                }
            }
            '\n' => {
                flush(&mut out, &mut cur);
                i = skip_heredoc_bodies(&chars, i, &mut heredocs);
            }
            '&' if cur.ends_with(['>', '<']) || chars.get(i) == Some(&'>') => cur.push(c),
            '&' => flush(&mut out, &mut cur),
            '<' if chars.get(i) == Some(&'<') && chars.get(i + 1) == Some(&'<') => {
                cur.push_str("<<<");
                i += 2;
            }
            '<' if chars.get(i) == Some(&'<') => {
                let (delimiter, strip_tabs, next) = heredoc_delimiter(&chars, i + 1);
                if !delimiter.is_empty() {
                    heredocs.push((delimiter, strip_tabs));
                }
                i = next;
            }
            c if is_separator(c) => flush(&mut out, &mut cur),
            c => cur.push(c),
        }
    }
    flush(&mut out, &mut cur);
    out
}

fn flush(out: &mut Vec<String>, cur: &mut String) {
    if !cur.trim().is_empty() {
        out.push(std::mem::take(cur));
    }
    cur.clear();
}

/// Read the delimiter after `<<` at `start`: `(word, strip_tabs, next index)`.
fn heredoc_delimiter(chars: &[char], start: usize) -> (String, bool, usize) {
    let mut i = start;
    let strip_tabs = chars.get(i) == Some(&'-');
    if strip_tabs {
        i += 1;
    }
    while chars.get(i).is_some_and(|c| *c == ' ' || *c == '\t') {
        i += 1;
    }
    let mut word = String::new();
    while let Some(&c) = chars.get(i) {
        if c.is_whitespace() || is_separator(c) || matches!(c, '&' | '<' | '>') {
            break;
        }
        if !matches!(c, '\'' | '"' | '\\') {
            word.push(c);
        }
        i += 1;
    }
    (word, strip_tabs, i)
}

/// Skip the bodies of the pending here-documents, from line start `start`.
fn skip_heredoc_bodies(chars: &[char], start: usize, pending: &mut Vec<(String, bool)>) -> usize {
    let mut i = start;
    for (delimiter, strip_tabs) in pending.drain(..) {
        while i < chars.len() {
            let end = chars[i..]
                .iter()
                .position(|c| *c == '\n')
                .map_or(chars.len(), |p| i + p);
            let line: String = chars[i..end].iter().collect();
            i = end + 1;
            let line = if strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line.as_str()
            };
            if line == delimiter {
                break;
            }
        }
    }
    i.min(chars.len())
}

#[cfg(test)]
#[path = "bash_worktree_tests.rs"]
mod tests;
