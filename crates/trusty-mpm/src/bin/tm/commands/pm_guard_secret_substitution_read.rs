//! A file read whose filename comes from a command substitution (#8931).
//!
//! Why: the #7266 secret-file rule judges the words of a command. A name the
//! shell computes at run time — `awk … $(ls -a | grep -E '^\.env\.local$')`,
//! `` cat `ls | grep local` `` — never appears as a word, so the read of a
//! dotenv file allowed while the same read with the name written out denied.
//! What: [`evaluate_substitution_read_command`] finds every argv word that a
//! file-reading program takes as an operand, or that names a file through
//! `<` or `@`, and that carries a `$( … )` or backtick substitution. Each
//! substitution is resolved when its output is fixed: arithmetic, an `echo`
//! or `printf` of literal text (judged on that text), or a program whose
//! output names no file the agent chose ([`NAME_FREE_PROGRAMS`]). The resolved
//! word is judged by the #7266 name rules; a substitution that does not
//! resolve refuses (fail closed).
//!
//! Not read: a substitution in a word that is not a file operand — `git
//! commit -m "$(cat msg)"`, `echo $(date)`, a search program's pattern, an
//! interpreter's inline program — and a name reached through a variable
//! (`cat "$F"`), the residual `pm_guard_secret_read` documents.
//! Test: `pm_guard_secret_substitution_read_tests.rs`.

use crate::commands::hook_rewrite::strip_wrapper_prefix;
use crate::commands::pm_guard_bash::{
    Substitution, git_subcommand, segment_substitution_spans, split_heredoc_bodies,
    split_shell_segments, tokenize,
};
use crate::commands::pm_guard_secret_read::{
    Scan, command_basename, inline_program_indices, names_a_secret_file, pattern_argument_index,
};

/// Programs whose operands are files they read, print, copy or archive.
const READERS: &[&str] = &[
    "cat", "tac", "head", "tail", "less", "more", "most", "bat", "batcat", "nl", "sed", "awk",
    "gawk", "mawk", "nawk", "grep", "egrep", "fgrep", "rg", "ag", "ack", "cut", "sort", "uniq",
    "wc", "od", "xxd", "hexdump", "hd", "strings", "base64", "base32", "jq", "yq", "diff", "cmp",
    "comm", "paste", "join", "fold", "fmt", "pr", "column", "expand", "rev", "tee", "cp", "mv",
    "scp", "rsync", "tar", "zip", "gzip", "bzip2", "xz", "zcat", "gunzip", "dd", "vim", "vi",
    "nvim", "nano", "emacs", "view", "open", "code", "pbcopy", "openssl", "gpg", "age", "sops",
];

/// Programs whose output names no file the command chose: a count, a time, a
/// host fact, or the working directory. `git` counts only as `git rev-parse`.
const NAME_FREE_PROGRAMS: &[&str] = &[
    "pwd", "date", "wc", "seq", "expr", "nproc", "id", "whoami", "hostname", "uname", "mktemp",
    "getconf", "tput", "true", ":", "cd", "git",
];

/// The placeholder a substitution's span becomes, so the segment tokenizes
/// with each substitution inside the word it belongs to.
fn placeholder(n: usize) -> String {
    format!("TMSUBST{n}X")
}

/// One masked substitution: its text as written, and its body.
type Masked = (String, Substitution);

/// `text` with each live `$( … )` and backtick span replaced by a
/// placeholder numbered after the entries already in `subs`, which it extends.
/// A process substitution is a pipe, not a computed name, and stays: the
/// nested rules judge its body.
fn mask(text: &str, subs: &mut Vec<Masked>) -> String {
    let mut masked = String::with_capacity(text.len());
    let mut at = 0;
    for (range, body) in segment_substitution_spans(text) {
        if text[range.clone()].starts_with(['<', '>']) {
            continue;
        }
        masked.push_str(&text[at..range.start]);
        masked.push_str(&placeholder(subs.len()));
        subs.push((text[range.clone()].to_string(), body));
        at = range.end;
    }
    masked.push_str(&text[at..]);
    masked
}

/// Refuse a Bash command that reads a file whose name a command substitution
/// computes: `Some(reason)` denies, `None` allows.
///
/// Why: see the module doc — a computed filename hides a secret-file read
/// from the word rule (#8931).
/// What: heredoc bodies are lifted out and each substitution is masked
/// BEFORE the segment split, so a `|` inside `$( … )` cannot cut it; each
/// segment masks what an `sh -c` expansion exposed, then tokenizes. Every
/// read-position word carrying a placeholder is resolved ([`resolve_word`])
/// and judged. Unresolvable, or resolved onto a secret name, denies.
/// Test: `denies_a_read_of_a_computed_filename_8931`,
/// `judges_a_static_substitution_on_its_output_8931`,
/// `allows_a_substitution_outside_a_file_operand_8931`.
pub(crate) fn evaluate_substitution_read_command(command: &str) -> Option<String> {
    let (argv_text, _) = split_heredoc_bodies(command);
    let mut subs = Vec::new();
    let masked = mask(&argv_text, &mut subs);
    split_shell_segments(&masked).iter().find_map(|segment| {
        let mut subs = subs.clone();
        let segment = mask(segment.trim(), &mut subs);
        segment_read(&segment, &subs)
    })
}

/// The deny reason for one masked segment, if it reads a computed filename.
fn segment_read(masked: &str, subs: &[Masked]) -> Option<String> {
    if !masked.contains("TMSUBST") {
        return None;
    }
    let argv = tokenize(masked).ok()?;
    let start = strip_wrapper_prefix(&argv).unwrap_or(0);
    let program = command_basename(argv.get(start)?);
    let reader = READERS.contains(&program.as_str());
    let pattern_at = pattern_argument_index(masked, &argv);
    let program_at = inline_program_indices(&argv);
    for (index, word) in argv.iter().enumerate().skip(start + 1) {
        if !word.contains("TMSUBST") || Some(index) == pattern_at || program_at.contains(&index) {
            continue;
        }
        // A here-string is text on stdin, not a file.
        if word.starts_with("<<") || argv[index - 1].starts_with("<<") {
            continue;
        }
        let redirected = word.starts_with('<') || argv[index - 1] == "<";
        if !(reader || redirected || word.contains("@TMSUBST")) {
            continue;
        }
        let shown = shown_word(word, subs);
        let why = match resolve_word(word, subs) {
            None => "it cannot resolve",
            Some(text)
                if text
                    .split_whitespace()
                    .any(|w| names_a_secret_file(w.trim_start_matches(['<', '@']), Scan::Argv)) =>
            {
                "it resolves to a secret-bearing name"
            }
            Some(_) => continue,
        };
        return Some(deny_reason(&program, &shown, why));
    }
    None
}

/// `word` with every placeholder replaced by its substitution's fixed output,
/// or `None` when one has no fixed output.
fn resolve_word(word: &str, subs: &[Masked]) -> Option<String> {
    let mut text = word.to_string();
    for (n, (shown, body)) in subs.iter().enumerate().rev() {
        let mark = placeholder(n);
        if !text.contains(&mark) {
            continue;
        }
        let Substitution::Closed(body) = body else {
            return None;
        };
        text = text.replace(&mark, &fixed_output(shown, body)?);
    }
    Some(text)
}

/// The output of a substitution body when it is fixed by the text alone.
/// `shown` is the substitution as written, opener included.
fn fixed_output(shown: &str, body: &str) -> Option<String> {
    if is_arithmetic(shown, body) {
        return Some("0".to_string());
    }
    let body = body.trim();
    if body.contains(['$', '`', '\\']) {
        return None;
    }
    let argv = tokenize(body).ok()?;
    match argv.first().map(String::as_str) {
        Some("echo") if !body.contains(['|', ';', '&', '<', '>']) => {
            let words: Vec<&String> = argv[1..].iter().skip_while(|w| *w == "-n").collect();
            if words.iter().any(|w| w.starts_with('-')) {
                return None;
            }
            Some(
                words
                    .iter()
                    .map(|w| w.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }
        Some("printf") if argv.len() == 2 && !body.contains(['%', '|', ';', '&', '<', '>']) => {
            Some(argv[1].clone())
        }
        _ => name_free(body).then(|| "x".to_string()),
    }
}

/// Whether `shown` is `$((…))` arithmetic, whose output is a number.
///
/// Why: a command substitution that opens with a subshell, `$( (cmd) )`, has
/// the same trimmed body as arithmetic, and resolving it to a number let a
/// computed-filename read through (#8931 critic round).
/// What: true only when the opener is `$((`, the body's leading `(` closes at
/// its last byte (bash reads `$((cmd) )` as a command substitution), and the
/// inner text is arithmetic alone: names, numbers, `$name`, spaces and
/// operators, with no two operands side by side as a command's words are.
/// Anything else is not arithmetic, so it fails closed.
/// Test: `denies_a_subshell_substitution_shaped_like_arithmetic_8931`,
/// `resolves_genuine_arithmetic_8931`.
fn is_arithmetic(shown: &str, body: &str) -> bool {
    let Some(inner) = shown
        .starts_with("$((")
        .then(|| body.strip_prefix('(')?.strip_suffix(')'))
        .flatten()
    else {
        return false;
    };
    let bytes = inner.as_bytes();
    let (mut depth, mut operand, mut spaced) = (0usize, false, false);
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => {
                let name_follows = bytes
                    .get(i + 1)
                    .is_some_and(|n| n.is_ascii_alphanumeric() || *n == b'_');
                if spaced || (b == b'$' && !name_follows) {
                    return false;
                }
                operand = true;
            }
            b' ' | b'\t' => {
                spaced |= operand;
                operand = false;
            }
            b'(' | b')' | b'+' | b'-' | b'*' | b'/' | b'%' | b'=' | b'!' | b'~' | b'^' | b'?'
            | b':' | b',' => {
                if b == b'(' {
                    depth += 1;
                } else if b == b')' {
                    // The leading `(` closed early: a subshell, not arithmetic.
                    let Some(d) = depth.checked_sub(1) else {
                        return false;
                    };
                    depth = d;
                }
                (operand, spaced) = (false, false);
            }
            _ => return false,
        }
    }
    depth == 0
}

/// Whether every program `body` runs prints no file name the command chose.
fn name_free(body: &str) -> bool {
    split_shell_segments(body).iter().all(|segment| {
        let Ok(argv) = tokenize(segment.trim()) else {
            return false;
        };
        let start = strip_wrapper_prefix(&argv).unwrap_or(0);
        let Some(program) = argv.get(start).map(|p| command_basename(p)) else {
            return true;
        };
        NAME_FREE_PROGRAMS.contains(&program.as_str())
            && (program != "git" || git_subcommand(segment.trim()).as_deref() == Some("rev-parse"))
    })
}

/// `word` as the command wrote it: each placeholder back to its text.
fn shown_word(word: &str, subs: &[Masked]) -> String {
    subs.iter()
        .enumerate()
        .rev()
        .fold(word.to_string(), |text, (n, (shown, _))| {
            text.replace(&placeholder(n), shown)
        })
}

/// The refusal text. It names the word as written, never a file's bytes.
fn deny_reason(program: &str, word: &str, why: &str) -> String {
    format!(
        "`tm hook --pm-guard` refused this command (#8931, #7266): `{program}` reads a file \
         whose name a command substitution computes (`{word}`), and {why}, so the guard \
         cannot rule out a secret-bearing file and fails closed. Write the filename \
         literally; if it is a secret-bearing file, stop and report to the Architect."
    )
}

#[cfg(test)]
#[path = "pm_guard_secret_substitution_read_tests.rs"]
mod tests;
