//! Which lines of a shell script body are true comments (#10548 round 2).
//!
//! Why: the script-body rule blanked every line whose first byte is `#`, so
//! that an apostrophe in prose could not unbalance the quoting the inline
//! rules lex. A `#` line inside an open quote is no comment, though:
//! `echo "a` then `#"; ./steal.sh` runs `./steal.sh`, and the blanked body
//! hid it from every rule.
//! What: [`true_comment_lines`] walks the body as bash lexes it — quotes,
//! `$(…)` nesting, backticks and here-documents — and marks a line a comment
//! only when it starts outside every quote, and a here-document body line
//! as program text when no shell reads it. Any shape it cannot follow
//! answers `None`, and the caller then blanks and masks nothing.
//! Test: `pm_guard_secret_fp_10548_tests.rs`.

use crate::commands::pm_guard_secret_script_case::{mask_method_calls, runs_a_shell};

/// What one line of a shell body is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineKind {
    /// Shell text the shell runs.
    Code,
    /// A comment line: outside every quote, or a here-document body line,
    /// whose first non-blank byte is `#`.
    Comment,
    /// A here-document body line handed to a program that is no shell.
    Program,
}

/// One open lexical context.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    /// `'…'`: nothing is special but the closing quote.
    Single,
    /// `$'…'`: a backslash escapes the next byte.
    AnsiC,
    /// `"…"`: a backslash escapes, `$(` and a backtick nest.
    Double,
    /// `$(…)`: unquoted shell text, closed by its `)`; the count is the
    /// grouping parens open inside it.
    Subst(usize),
    /// `` `…` ``: unquoted shell text, closed by the next backtick.
    Backtick,
}

/// The [`LineKind`] of each line of `body`, as `split_inclusive('\n')` cuts
/// it. A line is a comment when it starts outside every quote and
/// substitution and its first non-blank byte is `#`; a here-document body
/// line starting with `#` counts too, as before #10548 round 2.
///
/// `None` when the walk ends inside a quote, a substitution or a
/// here-document, or meets a shape it cannot follow: a `<<` or a `case`
/// inside a substitution.
/// Test: `a_hash_line_inside_an_open_quote_still_runs_10548`,
/// `a_missing_script_named_only_in_a_comment_is_waived_10548`.
pub(crate) fn line_kinds(body: &str) -> Option<Vec<LineKind>> {
    let bytes = body.as_bytes();
    let mut stack: Vec<Ctx> = Vec::new();
    let mut flags = Vec::new();
    let mut heredocs: Vec<(String, bool, bool)> = Vec::new();
    let mut line_start = true;
    let mut line_from = 0;
    let mut i = 0;
    while i < bytes.len() {
        if line_start {
            line_start = false;
            let line_end = bytes[i..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(bytes.len(), |n| i + n + 1);
            let line = &body[i..line_end];
            let comment = stack.is_empty() && line.trim_start().starts_with('#');
            flags.push(if comment {
                LineKind::Comment
            } else {
                LineKind::Code
            });
            line_from = i;
        }
        let b = bytes[i];
        let top = stack.last().copied();
        match top {
            Some(Ctx::Single) => {
                if b == b'\'' {
                    stack.pop();
                }
            }
            Some(Ctx::AnsiC) => match b {
                b'\\' => i += 1,
                b'\'' => {
                    stack.pop();
                }
                _ => {}
            },
            Some(Ctx::Double) => match b {
                b'\\' => i += 1,
                b'"' => {
                    stack.pop();
                }
                b'`' => stack.push(Ctx::Backtick),
                b'$' if bytes.get(i + 1) == Some(&b'(') => {
                    stack.push(Ctx::Subst(0));
                    i += 1;
                }
                _ => {}
            },
            None | Some(Ctx::Subst(_)) | Some(Ctx::Backtick) => {
                match b {
                    b'\\' => i += 1,
                    b'\'' => stack.push(Ctx::Single),
                    b'"' => stack.push(Ctx::Double),
                    b'$' if bytes.get(i + 1) == Some(&b'\'') => {
                        stack.push(Ctx::AnsiC);
                        i += 1;
                    }
                    b'$' if bytes.get(i + 1) == Some(&b'(') => {
                        stack.push(Ctx::Subst(0));
                        i += 1;
                    }
                    b'`' if top == Some(Ctx::Backtick) => {
                        stack.pop();
                    }
                    b'`' => stack.push(Ctx::Backtick),
                    b'(' => {
                        if let Some(Ctx::Subst(n)) = stack.last_mut() {
                            *n += 1;
                        }
                    }
                    b')' => match stack.last_mut() {
                        Some(Ctx::Subst(0)) => {
                            stack.pop();
                        }
                        Some(Ctx::Subst(n)) => *n -= 1,
                        _ => {}
                    },
                    b'#' if word_start(bytes, i) => {
                        // A comment runs to the newline, which is handled below.
                        while bytes.get(i + 1).is_some_and(|&c| c != b'\n') {
                            i += 1;
                        }
                    }
                    // `<<<` is a here-string, not a here-document.
                    b'<' if bytes.get(i + 1) == Some(&b'<')
                        && bytes.get(i + 2) != Some(&b'<')
                        && (i == 0 || bytes[i - 1] != b'<') =>
                    {
                        if !stack.is_empty() {
                            return None;
                        }
                        let (delimiter, dash, end) = heredoc_delimiter(bytes, i + 2)?;
                        // A body a shell reads is shell text, never program text.
                        let words: Vec<String> = body[line_from..i]
                            .split_whitespace()
                            .map(String::from)
                            .collect();
                        heredocs.push((delimiter, dash, !runs_a_shell(&words)));
                        i = end;
                        continue;
                    }
                    b'c' if !stack.is_empty() && body[i..].starts_with("case") => return None,
                    _ => {}
                }
            }
        }
        if bytes.get(i) == Some(&b'\n') {
            line_start = true;
            if stack.is_empty() && !heredocs.is_empty() {
                i = skip_heredoc_bodies(body, i + 1, &mut heredocs, &mut flags)?;
                continue;
            }
        }
        i += 1;
    }
    (stack.is_empty() && heredocs.is_empty()).then_some(flags)
}

/// Whether the byte at `at` starts a word, so a `#` there opens a comment.
fn word_start(bytes: &[u8], at: usize) -> bool {
    at == 0
        || matches!(
            bytes[at - 1],
            b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' | b')'
        )
}

/// The delimiter of a here-document whose `<<` ends just before `start`,
/// whether it is `<<-`, and the index just past the delimiter word.
fn heredoc_delimiter(bytes: &[u8], start: usize) -> Option<(String, bool, usize)> {
    let mut i = start;
    let dash = bytes.get(i) == Some(&b'-');
    if dash {
        i += 1;
    }
    while bytes.get(i).is_some_and(|&b| b == b' ' || b == b'\t') {
        i += 1;
    }
    let mut word = Vec::new();
    let mut quote = None;
    while let Some(&b) = bytes.get(i) {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => word.push(b),
            None if b == b'\'' || b == b'"' => quote = Some(b),
            None if b == b'\\' => {}
            None if b.is_ascii_whitespace() || b";&|<>()".contains(&b) => break,
            None => word.push(b),
        }
        i += 1;
    }
    let word = String::from_utf8(word).ok()?;
    (quote.is_none() && !word.is_empty()).then_some((word, dash, i))
}

/// Skip the bodies of the pending here-documents from `start`, pushing one
/// flag per body line; the index just past the last terminator line.
fn skip_heredoc_bodies(
    body: &str,
    mut start: usize,
    heredocs: &mut Vec<(String, bool, bool)>,
    flags: &mut Vec<LineKind>,
) -> Option<usize> {
    for (delimiter, dash, program) in heredocs.drain(..) {
        loop {
            let rest = body.get(start..)?;
            if rest.is_empty() {
                return None;
            }
            let len = rest.find('\n').map_or(rest.len(), |n| n + 1);
            let line = &rest[..len];
            let text = line.trim_end_matches('\n');
            let text = if dash {
                text.trim_start_matches('\t')
            } else {
                text
            };
            flags.push(match () {
                _ if text != delimiter && line.trim_start().starts_with('#') => LineKind::Comment,
                _ if text != delimiter && program => LineKind::Program,
                _ => LineKind::Code,
            });
            start += len;
            if text == delimiter {
                break;
            }
        }
    }
    Some(start)
}

/// `body` with each true comment line blanked to its newline, or `None` when
/// [`line_kinds`] cannot tell which lines are comments.
pub(crate) fn without_true_comments(body: &str) -> Option<String> {
    let kinds = line_kinds(body)?;
    Some(
        body.split_inclusive('\n')
            .zip(kinds.into_iter().chain(std::iter::repeat(LineKind::Code)))
            .map(|(line, kind)| match (kind, line.ends_with('\n')) {
                (LineKind::Comment, true) => "\n",
                (LineKind::Comment, false) => "",
                _ => line,
            })
            .collect(),
    )
}

/// `body` with [`mask_method_calls`] applied to its program-text lines only
/// (#10548 round 2: never shell argv); unchanged when [`line_kinds`] cannot
/// tell which lines those are.
/// Test: `a_method_call_on_a_token_value_is_no_file_10548`,
/// `a_method_call_spelling_in_shell_argv_still_denies_10548`.
pub(crate) fn mask_heredoc_programs(body: &str) -> String {
    let Some(kinds) = line_kinds(body) else {
        return body.to_string();
    };
    body.split_inclusive('\n')
        .zip(kinds.into_iter().chain(std::iter::repeat(LineKind::Code)))
        .map(|(line, kind)| match kind {
            LineKind::Program => mask_method_calls(line).into_owned(),
            _ => line.to_string(),
        })
        .collect()
}
