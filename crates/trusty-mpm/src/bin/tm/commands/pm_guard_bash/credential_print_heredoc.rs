//! Quoted-delimiter here-documents for the credential-print rule (#8596).
//!
//! Why: a `<<'EOF'` body is literal stdin data. Tokenized as argv it made a
//! `gh issue comment N --body-file - <<'EOF'` naming a credential command read
//! as a credential call, and an apostrophe in the body read as broken quoting.
//! What: [`strip_quoted_heredocs`] removes each quoted-delimiter body and its
//! terminator line, leaves a [`HEREDOC_MARK`] placeholder where the operator
//! was, and records what the body holds. An unquoted-delimiter body expands
//! `$(…)`, so it stays in place for the ordinary scan; an operator with no
//! terminator line (or an arithmetic `<<`) is left untouched too.
//! Test: `credential_print_tests::allows_quoted_heredoc_bodies`,
//! `credential_print_tests::denies_evaluated_trigger_text`.

use super::{Heredoc, input_is_program_text};

/// Placeholder a stripped here-document leaves in the command text.
pub(super) const HEREDOC_MARK: &str = "__TMHEREDOC";

/// Lexer context while walking the command.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    /// Shell code; the count is open grouping parens inside it.
    Code(usize),
    Double,
    Single,
}

/// A here-document operator waiting for its body.
struct Pending {
    /// Byte range of the operator (`<<-'EOF'`) in the input.
    op: (usize, usize),
    word: String,
    strip_tabs: bool,
    quoted: bool,
}

/// Strip every quoted-delimiter here-document body out of `text`.
///
/// What: walks `text` with a quote/substitution context stack. At each
/// unquoted `<<` (not `<<<`) it reads the delimiter word; at the next newline
/// in code context it consumes the pending bodies, each up to a line equal to
/// its word. Quoted bodies are removed and pushed to `heredocs`; the operator
/// becomes `HEREDOC_MARK<n>__`. Byte slicing happens only at ASCII positions.
pub(super) fn strip_quoted_heredocs(text: &str, heredocs: &mut Vec<Heredoc>) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut stack = vec![Ctx::Code(0)];
    let mut pending: Vec<Pending> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let top = stack.last().copied().unwrap_or(Ctx::Code(0));
        let b = bytes[i];
        match top {
            Ctx::Single => {
                if b == b'\'' {
                    stack.pop();
                }
            }
            Ctx::Double => match b {
                b'\\' => i += 1,
                b'"' => {
                    stack.pop();
                }
                b'$' if bytes.get(i + 1) == Some(&b'(') => {
                    stack.push(Ctx::Code(0));
                    i += 1;
                }
                _ => {}
            },
            Ctx::Code(parens) => match b {
                b'\\' => i += 1,
                b'\'' => stack.push(Ctx::Single),
                b'"' => stack.push(Ctx::Double),
                b'$' if bytes.get(i + 1) == Some(&b'(') => {
                    stack.push(Ctx::Code(0));
                    i += 1;
                }
                b'(' => set_top(&mut stack, Ctx::Code(parens + 1)),
                b')' if parens == 0 && stack.len() > 1 => {
                    stack.pop();
                }
                b')' => set_top(&mut stack, Ctx::Code(parens.saturating_sub(1))),
                b'<' if bytes.get(i + 1) == Some(&b'<') => {
                    if bytes.get(i + 2) == Some(&b'<') {
                        i += 3;
                        continue;
                    }
                    if let Some(p) = read_operator(text, i) {
                        i = p.op.1;
                        pending.push(p);
                        continue;
                    }
                    i += 2;
                    continue;
                }
                b'\n' if !pending.is_empty() => {
                    let (next, kept) = consume_bodies(text, i + 1, &mut pending, heredocs);
                    for (p, index) in kept {
                        out.push_str(&text[copied..p.op.0]);
                        out.push_str(&format!(" {HEREDOC_MARK}{index}__ "));
                        copied = p.op.1;
                    }
                    out.push_str(&text[copied..=i]);
                    copied = next;
                    i = next;
                    continue;
                }
                _ => {}
            },
        }
        i += 1;
    }
    out.push_str(&text[copied.min(text.len())..]);
    out
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
        word,
        strip_tabs,
        quoted,
    })
}

/// Consume the bodies of `pending` from byte `from`; return where scanning
/// resumes and the stripped operators with their `heredocs` index.
///
/// What: a delimiter with no terminator line abandons the rest, keeping the
/// text as-is from that body on (the conservative pre-strip reading).
fn consume_bodies(
    text: &str,
    from: usize,
    pending: &mut Vec<Pending>,
    heredocs: &mut Vec<Heredoc>,
) -> (usize, Vec<(Pending, usize)>) {
    let mut kept = Vec::new();
    let mut cursor = from;
    for p in pending.drain(..) {
        let Some((body_end, next)) = find_terminator(text, cursor, &p) else {
            return (cursor, kept);
        };
        if !p.quoted {
            // An unquoted body expands `$(…)`: the ordinary scan reads it.
            return (cursor, kept);
        }
        let body = &text[cursor..body_end];
        heredocs.push(Heredoc {
            program_text: input_is_program_text(body),
        });
        kept.push((p, heredocs.len() - 1));
        cursor = next;
    }
    (cursor, kept)
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
