//! Comments and quoted-delimiter here-documents for the credential-print rule
//! (#8596).
//!
//! Why: a `<<'EOF'` body is literal stdin data. Tokenized as argv it made a
//! `gh issue comment N --body-file - <<'EOF'` naming a credential command read
//! as a credential call, and an apostrophe in the body read as broken quoting.
//! A comment is not code either: `true # <<'X'` opened a here-document that
//! swallowed the next line, and `# don't` opened a quote that hid the line
//! after it from the stage splitter (round 3, finding 1).
//! What: [`strip_comments_and_heredocs`] is the one lexer pass that runs
//! before substitution lifting and stage splitting. It removes each unquoted
//! word-start `#` comment up to its newline, removes each quoted-delimiter
//! body and its terminator line, leaves a [`HEREDOC_MARK`] placeholder where
//! that operator was, and records what the body holds. An unquoted-delimiter
//! body expands `$(…)`, so it stays in place, verbatim, for the ordinary scan;
//! an operator with no terminator line (or an arithmetic `<<`) is left
//! untouched too. A `<<` or `#` inside `${…}` is part of the word.
//! Test: `credential_print_tests::allows_quoted_heredoc_bodies`,
//! `credential_print_tests::denies_evaluated_trigger_text`,
//! `credential_print_tests::denies_the_round_three_bypasses`,
//! `credential_print_tests::allows_script_operands_and_comments`.

use super::{Heredoc, input_is_program_text};

/// Placeholder a stripped here-document leaves in the command text.
pub(super) const HEREDOC_MARK: &str = "__TMHEREDOC";

/// Lexer context while walking the command.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    /// Shell code; the count is open grouping parens inside it.
    Code(usize),
    /// Shell code inside backticks.
    Backtick,
    /// A `${…}` expansion; `true` when it sits inside double quotes.
    Brace(bool),
    Double,
    Single,
    /// ANSI-C quoting, `$'…'`, where a backslash escapes a quote.
    Ansi,
}

/// A here-document operator waiting for its body.
struct Pending {
    /// Byte range of the operator (`<<-'EOF'`) in the input.
    op: (usize, usize),
    /// Byte range of the operator's copy in the output.
    out: (usize, usize),
    word: String,
    strip_tabs: bool,
    quoted: bool,
}

/// Remove comments and quoted-delimiter here-document bodies from `text`.
///
/// What: walks `text` with a quote/substitution context stack. In code
/// context a `#` that starts a word drops the text up to the newline (or the
/// closing backtick); an unquoted `<<` (not `<<<`) reads its delimiter word,
/// and at the next newline in code context the pending bodies are consumed,
/// each up to a line equal to its word. Quoted bodies are removed and pushed
/// to `heredocs`; the operator becomes `HEREDOC_MARK<n>__`. Byte slicing
/// happens only at ASCII positions.
pub(super) fn strip_comments_and_heredocs(text: &str, heredocs: &mut Vec<Heredoc>) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut stack = vec![Ctx::Code(0)];
    let mut pending: Vec<Pending> = Vec::new();
    // #8596 round 3: `#` opens a comment only at the start of a word.
    let mut word_start = true;
    let mut i = 0;
    while i < bytes.len() {
        let top = stack.last().copied().unwrap_or(Ctx::Code(0));
        let b = bytes[i];
        let at_word_start = std::mem::replace(&mut word_start, false);
        match top {
            Ctx::Single => {
                if b == b'\'' {
                    stack.pop();
                }
            }
            Ctx::Ansi => match b {
                b'\\' => i += 1,
                b'\'' => {
                    stack.pop();
                }
                _ => {}
            },
            Ctx::Double | Ctx::Brace(_) => {
                let in_double = top != Ctx::Brace(false);
                match b {
                    b'\\' => i += 1,
                    b'"' if top == Ctx::Double => {
                        stack.pop();
                    }
                    b'}' if top != Ctx::Double => {
                        stack.pop();
                    }
                    b'"' => stack.push(Ctx::Double),
                    b'\'' if !in_double => stack.push(Ctx::Single),
                    b'`' => {
                        stack.push(Ctx::Backtick);
                        word_start = true;
                    }
                    b'$' => {
                        if let Some(ctx) = dollar_opener(bytes, i, in_double) {
                            stack.push(ctx);
                            i += 1;
                            word_start = ctx == Ctx::Code(0);
                        }
                    }
                    _ => {}
                }
            }
            Ctx::Code(_) | Ctx::Backtick => match b {
                b'#' if at_word_start => {
                    let end = comment_end(text, i, top == Ctx::Backtick);
                    out.push_str(&text[copied..i]);
                    copied = end;
                    i = end;
                    continue;
                }
                b'`' if top == Ctx::Backtick => {
                    stack.pop();
                }
                b'(' | b')' => word_start = paren(b, top, &mut stack),
                b'\n' if !pending.is_empty() => {
                    let (next, kept, bodies) = consume_bodies(text, i + 1, &mut pending, heredocs);
                    // Latest first, so an earlier operator's range stays valid.
                    for (p, index) in kept.into_iter().rev() {
                        out.replace_range(p.out.0..p.out.1, &format!(" {HEREDOC_MARK}{index}__ "));
                    }
                    out.push_str(&text[copied..=i]);
                    for (start, end) in bodies {
                        out.push_str(&text[start..end]);
                    }
                    copied = next;
                    i = next;
                    word_start = true;
                    continue;
                }
                _ => {
                    let waiting = pending.len();
                    if let Some(step) = code_byte(text, i, &mut stack, &mut pending) {
                        if let Some(p) = pending.get_mut(waiting) {
                            // Copy the operator now: a comment after it on
                            // the line moves `copied` past it.
                            out.push_str(&text[copied..p.op.1]);
                            p.out = (out.len() - (p.op.1 - p.op.0), out.len());
                            copied = p.op.1;
                        }
                        word_start = step.word_start;
                        i = step.next;
                        continue;
                    }
                    word_start = is_word_break(b);
                }
            },
        }
        i += 1;
    }
    out.push_str(&text[copied.min(text.len())..]);
    out
}

/// Where the walk resumes after a byte of code context.
struct Step {
    next: usize,
    word_start: bool,
}

/// A byte of code context shared by `$(…)` and backticks: escapes, quote and
/// substitution openers, and here-document operators.
///
/// What: pushes any context the byte opens and returns where the walk resumes
/// and whether that is a word start; `None` for an ordinary byte.
fn code_byte(
    text: &str,
    i: usize,
    stack: &mut Vec<Ctx>,
    pending: &mut Vec<Pending>,
) -> Option<Step> {
    let bytes = text.as_bytes();
    let step = |next, word_start| Some(Step { next, word_start });
    match bytes[i] {
        b'\\' => step(i + 2, false),
        b'\'' => {
            stack.push(Ctx::Single);
            step(i + 1, false)
        }
        b'"' => {
            stack.push(Ctx::Double);
            step(i + 1, false)
        }
        b'`' => {
            stack.push(Ctx::Backtick);
            step(i + 1, true)
        }
        b'$' => {
            let ctx = dollar_opener(bytes, i, false)?;
            stack.push(ctx);
            step(i + 2, ctx == Ctx::Code(0))
        }
        b'<' if bytes.get(i + 1) == Some(&b'<') => {
            if bytes.get(i + 2) == Some(&b'<') {
                return step(i + 3, true);
            }
            match read_operator(text, i) {
                Some(p) => {
                    let next = p.op.1;
                    pending.push(p);
                    step(next, false)
                }
                None => step(i + 2, true),
            }
        }
        _ => None,
    }
}

/// The context a `$` at byte `i` opens: `$(`, `${`, or (outside double
/// quotes) `$'`.
fn dollar_opener(bytes: &[u8], i: usize, in_double: bool) -> Option<Ctx> {
    match bytes.get(i + 1) {
        Some(b'(') => Some(Ctx::Code(0)),
        Some(b'{') => Some(Ctx::Brace(in_double)),
        Some(b'\'') if !in_double => Some(Ctx::Ansi),
        _ => None,
    }
}

/// Whether an unquoted byte ends a word, so a `#` after it opens a comment.
fn is_word_break(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'<' | b'>')
}

/// Where a comment starting at `at` ends: its newline, kept as a separator,
/// or inside backticks the closing backtick if that comes first.
fn comment_end(text: &str, at: usize, in_backtick: bool) -> usize {
    let rest = &text[at..];
    let newline = rest.find('\n').unwrap_or(rest.len());
    let tick = if in_backtick {
        rest.find('`').unwrap_or(rest.len())
    } else {
        rest.len()
    };
    at + newline.min(tick)
}

/// Track a grouping paren in code context; whether a word starts after it.
///
/// What: `(` opens a group; `)` closes one, or closes the `$(…)` itself when
/// none is open (a word continues after that, as in `$(x)#y`).
fn paren(b: u8, top: Ctx, stack: &mut Vec<Ctx>) -> bool {
    let Ctx::Code(parens) = top else {
        return true;
    };
    if b == b'(' {
        set_top(stack, Ctx::Code(parens + 1));
    } else if parens == 0 && stack.len() > 1 {
        stack.pop();
        return false;
    } else {
        set_top(stack, Ctx::Code(parens.saturating_sub(1)));
    }
    true
}

/// Replace the innermost context.
fn set_top(stack: &mut [Ctx], ctx: Ctx) {
    if let Some(top) = stack.last_mut() {
        *top = ctx;
    }
}

/// Read a here-document operator starting at the `<<` at byte `at`.
///
/// What: `None` when no delimiter word follows, or when its quoting is
/// unclosed on the line.
fn read_operator(text: &str, at: usize) -> Option<Pending> {
    let bytes = text.as_bytes();
    let mut j = at + 2;
    let strip_tabs = bytes.get(j) == Some(&b'-');
    if strip_tabs {
        j += 1;
    }
    while matches!(bytes.get(j), Some(b' ' | b'\t')) {
        j += 1;
    }
    let mut word = Vec::new();
    let mut quoted = false;
    while let Some(&b) = bytes.get(j) {
        match b {
            b' ' | b'\t' | b'\n' | b'<' | b'>' | b'|' | b';' | b'&' | b'(' | b')' => break,
            b'\'' | b'"' => {
                quoted = true;
                let close = text[j + 1..].find(b as char)?;
                word.extend_from_slice(&bytes[j + 1..j + 1 + close]);
                j += close + 2;
            }
            b'\\' => {
                quoted = true;
                word.push(*bytes.get(j + 1)?);
                j += 2;
            }
            _ => {
                word.push(b);
                j += 1;
            }
        }
    }
    let word = String::from_utf8(word).ok()?;
    let usable = !word.is_empty() && !word.contains('\n') && text.is_char_boundary(j);
    usable.then_some(Pending {
        op: (at, j),
        out: (0, 0),
        word,
        strip_tabs,
        quoted,
    })
}

/// Consume the bodies of `pending` from byte `from`.
///
/// What: returns where scanning resumes, the stripped operators with their
/// `heredocs` index, and the byte ranges of the unquoted bodies (terminator
/// line included), which stay in the text verbatim — the walk never reads
/// them as code, so a `#` or apostrophe in one changes nothing. A delimiter
/// with no terminator line abandons the rest, keeping the text as-is from
/// that body on (the conservative pre-strip reading).
fn consume_bodies(
    text: &str,
    from: usize,
    pending: &mut Vec<Pending>,
    heredocs: &mut Vec<Heredoc>,
) -> (usize, Vec<(Pending, usize)>, Vec<(usize, usize)>) {
    let mut kept = Vec::new();
    let mut bodies = Vec::new();
    let mut cursor = from;
    for p in pending.drain(..) {
        let Some((body_end, next)) = find_terminator(text, cursor, &p) else {
            return (cursor, kept, bodies);
        };
        if p.quoted {
            heredocs.push(Heredoc {
                program_text: input_is_program_text(&text[cursor..body_end]),
            });
            kept.push((p, heredocs.len() - 1));
        } else {
            // An unquoted body expands `$(…)`: the ordinary scan reads it.
            bodies.push((cursor, next));
        }
        cursor = next;
    }
    (cursor, kept, bodies)
}

/// The end of the body and the byte after the terminator line's newline.
fn find_terminator(text: &str, from: usize, p: &Pending) -> Option<(usize, usize)> {
    let mut start = from;
    while start <= text.len() {
        let end = text[start..].find('\n').map_or(text.len(), |n| start + n);
        let line = text[start..end].trim_end_matches('\r');
        let line = if p.strip_tabs {
            line.trim_start_matches('\t')
        } else {
            line
        };
        if line == p.word {
            return Some((start, (end + 1).min(text.len())));
        }
        if end >= text.len() {
            return None;
        }
        start = end + 1;
    }
    None
}
