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
//! #9150: [`heredoc_operators`] runs the same walk for the guard's
//! here-document scanner, so the two agree on which `<<` opens a body.
//! Test: `credential_print_tests::allows_quoted_heredoc_bodies`,
//! `credential_print_tests::denies_evaluated_trigger_text`,
//! `credential_print_tests::denies_the_round_three_bypasses`,
//! `credential_print_tests::allows_script_operands_and_comments`,
//! `heredoc_operators_skip_comments_escapes_and_expansions`.

use super::super::heredoc::{delimiter_word, is_terminator};
use super::{Heredoc, input_is_program_text};

/// Placeholder a stripped here-document leaves in the command text.
pub(super) const HEREDOC_MARK: &str = "__TMHEREDOC";

/// How the shell reads a `<<` the walk found in code (#9150).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OperatorCtx {
    /// Shell code: the operator's body is the lines after its line.
    Code,
    /// Arithmetic (`$((…))`, `$[…]`, `((…))`), or a `$(…)` or backtick that
    /// closes on the operator's own line. Bash 3.2 and zsh 5.9 read no body
    /// from the next lines there, but the guard cannot prove every shell
    /// agrees, so a body it would claim is refused.
    Ambiguous,
}

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
    /// #9150: arithmetic; `depth` counts open `(`/`[`, `bracket` is `$[…]`.
    Arith {
        depth: usize,
        bracket: bool,
    },
}

/// A here-document operator waiting for its body.
struct Pending {
    /// Byte range of the operator (`<<-'EOF'`) in the input.
    op: (usize, usize),
    /// Byte range of the operator's copy in the output.
    out: (usize, usize),
    /// #9150: `None` for a word [`delimiter_word`] refuses.
    word: Option<String>,
    strip_tabs: bool,
    quoted: bool,
    /// #9150: this operator's index in the walk's operator list.
    index: usize,
    /// #9150: stack depth at the operator, and the lowest depth since.
    depth: usize,
    low: usize,
}

/// Byte offset and [`OperatorCtx`] of every `<<` (not `<<<`) the shell reads
/// as an operator rather than as quoted, escaped, commented, `${…}` or
/// here-document body text (#9150).
///
/// Test: `heredoc_operators_skip_comments_escapes_and_expansions`.
pub(crate) fn heredoc_operators(text: &str) -> Vec<(usize, OperatorCtx)> {
    let mut ops = Vec::new();
    walk(text, &mut Vec::new(), &mut ops);
    ops
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
    walk(text, heredocs, &mut Vec::new())
}

/// The walk behind [`strip_comments_and_heredocs`]; every `<<` it reads as
/// an operator lands in `ops` (#9150).
fn walk(text: &str, heredocs: &mut Vec<Heredoc>, ops: &mut Vec<(usize, OperatorCtx)>) -> String {
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
        let depth = stack.len();
        for p in &mut pending {
            p.low = p.low.min(depth);
        }
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
                        if let Some((ctx, len)) = dollar_opener(bytes, i, in_double) {
                            stack.push(ctx);
                            i += len - 1;
                            word_start = ctx == Ctx::Code(0);
                        }
                    }
                    _ => {}
                }
            }
            Ctx::Arith { depth, bracket } => {
                i = arith_byte(bytes, i, (depth, bracket), &mut stack, ops);
                continue;
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
                // #9150: `((` opening a word is an arithmetic command.
                b'(' if at_word_start && bytes.get(i + 1) == Some(&b'(') => {
                    stack.push(Ctx::Arith {
                        depth: 0,
                        bracket: false,
                    });
                    i += 2;
                    continue;
                }
                b'(' | b')' => word_start = paren(b, top, &mut stack),
                b'\n' if !pending.is_empty() => {
                    // #9150: a `<<` whose `$(…)` or backtick closed on its
                    // line reads no body from the next lines.
                    pending.retain(|p| {
                        let live = p.low >= p.depth;
                        if !live {
                            ops[p.index].1 = OperatorCtx::Ambiguous;
                        }
                        live
                    });
                    let done = consume_bodies(text, i + 1, &mut pending, heredocs);
                    // Latest first, so an earlier operator's range stays valid.
                    for ((from, to), index) in done.stripped.into_iter().rev() {
                        out.replace_range(from..to, &format!(" {HEREDOC_MARK}{index}__ "));
                    }
                    out.push_str(&text[copied..=i]);
                    for (start, end) in done.bodies {
                        out.push_str(&text[start..end]);
                    }
                    copied = done.next;
                    i = done.next;
                    word_start = true;
                    continue;
                }
                _ => {
                    let waiting = pending.len();
                    if let Some(step) = code_byte(text, i, &mut stack, &mut pending, ops) {
                        if let Some(p) = pending.get_mut(waiting) {
                            (p.index, p.depth, p.low) = (ops.len() - 1, stack.len(), stack.len());
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
    ops: &mut Vec<(usize, OperatorCtx)>,
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
            let (ctx, len) = dollar_opener(bytes, i, false)?;
            stack.push(ctx);
            step(i + len, ctx == Ctx::Code(0))
        }
        b'<' if bytes.get(i + 1) == Some(&b'<') => {
            if bytes.get(i + 2) == Some(&b'<') {
                return step(i + 3, true);
            }
            ops.push((i, OperatorCtx::Code));
            match read_operator(text, i) {
                Some(p) => {
                    // #9150: after a refused word the walk reads that word.
                    let (next, word_start) = (p.op.1, p.word.is_none());
                    pending.push(p);
                    step(next, word_start)
                }
                None => step(i + 2, true),
            }
        }
        _ => None,
    }
}

/// The context a `$` at byte `i` opens, and the opener's length: `$(`,
/// `${`, (outside double quotes) `$'`, and #9150's arithmetic `$((` and `$[`.
fn dollar_opener(bytes: &[u8], i: usize, in_double: bool) -> Option<(Ctx, usize)> {
    let arith = |bracket| Ctx::Arith { depth: 0, bracket };
    match (bytes.get(i + 1), bytes.get(i + 2)) {
        (Some(b'('), Some(b'(')) => Some((arith(false), 3)),
        (Some(b'('), _) => Some((Ctx::Code(0), 2)),
        (Some(b'['), _) => Some((arith(true), 2)),
        (Some(b'{'), _) => Some((Ctx::Brace(in_double), 2)),
        (Some(b'\''), _) if !in_double => Some((Ctx::Ansi, 2)),
        _ => None,
    }
}

/// One byte of arithmetic, `(depth, bracket)` from [`Ctx::Arith`]: quotes
/// and substitutions still open, `(`/`[` nest, and the closing `))` or `]`
/// pops. A `<<` is a shift, recorded [`OperatorCtx::Ambiguous`] (#9150).
/// Where the walk resumes.
fn arith_byte(
    bytes: &[u8],
    i: usize,
    (depth, bracket): (usize, bool),
    stack: &mut Vec<Ctx>,
    ops: &mut Vec<(usize, OperatorCtx)>,
) -> usize {
    match bytes[i] {
        b'\\' => return i + 2,
        b'\'' => stack.push(Ctx::Single),
        b'"' => stack.push(Ctx::Double),
        b'`' => stack.push(Ctx::Backtick),
        b'$' => {
            if let Some((ctx, len)) = dollar_opener(bytes, i, false) {
                stack.push(ctx);
                return i + len;
            }
        }
        b'<' if bytes.get(i + 1) == Some(&b'<') => {
            ops.push((i, OperatorCtx::Ambiguous));
            return i + 2;
        }
        b'(' | b'[' => set_top(
            stack,
            Ctx::Arith {
                depth: depth + 1,
                bracket,
            },
        ),
        b')' | b']' if depth > 0 => set_top(
            stack,
            Ctx::Arith {
                depth: depth - 1,
                bracket,
            },
        ),
        b']' if bracket => {
            stack.pop();
        }
        b')' if !bracket => {
            stack.pop();
            if bytes.get(i + 1) == Some(&b')') {
                return i + 2;
            }
        }
        _ => {}
    }
    i + 1
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
/// What: `None` when no delimiter word follows. #9150: the word is read by
/// the here-document scanner's own [`delimiter_word`], over the operator's
/// line, so the two scanners share one grammar and one `\r` rule. A word it
/// refuses still yields a [`Pending`], with no `word` and an `op` covering
/// only the `<<`: the walk reads the word's bytes as code, the operator can
/// still turn [`OperatorCtx::Ambiguous`], and [`consume_bodies`] strips no
/// body from that operator on.
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
    let line_end = text[at..].find('\n').map_or(text.len(), |n| at + n);
    let (word, quoted, op_end) = match delimiter_word(&bytes[..line_end], j) {
        Some((word, _, _)) if word.is_empty() => return None,
        Some((word, quoted, end)) => (Some(word), quoted, end),
        // #9150: a word the scanner refuses opens a body no rule can bound.
        None => (None, true, at + 2),
    };
    Some(Pending {
        op: (at, op_end),
        out: (0, 0),
        word,
        strip_tabs,
        quoted,
        index: 0,
        depth: 0,
        low: 0,
    })
}

/// What [`consume_bodies`] took from the text after one operator line.
struct Consumed {
    /// Where scanning resumes.
    next: usize,
    /// Each stripped operator's output range and its `heredocs` index.
    stripped: Vec<((usize, usize), usize)>,
    /// Byte ranges of the unquoted bodies, terminator line included.
    bodies: Vec<(usize, usize)>,
}

/// Consume the bodies of `pending` from byte `from`.
///
/// What: an unquoted body stays in the text verbatim — the walk never reads
/// it as code, so a `#` or apostrophe in one changes nothing. A delimiter
/// with no terminator line abandons the rest, keeping the text as-is from
/// that body on (the conservative pre-strip reading).
fn consume_bodies(
    text: &str,
    from: usize,
    pending: &mut Vec<Pending>,
    heredocs: &mut Vec<Heredoc>,
) -> Consumed {
    let mut done = Consumed {
        next: from,
        stripped: Vec::new(),
        bodies: Vec::new(),
    };
    for p in pending.drain(..) {
        let Some((body_end, next)) = find_terminator(text, done.next, &p) else {
            return done;
        };
        if p.quoted {
            heredocs.push(Heredoc {
                program_text: input_is_program_text(&text[done.next..body_end]),
            });
            done.stripped.push((p.out, heredocs.len() - 1));
        } else {
            // An unquoted body expands `$(…)`: the ordinary scan reads it.
            done.bodies.push((done.next, next));
        }
        done.next = next;
    }
    done
}

/// The end of the body and the byte after the terminator line's newline;
/// `None` with no terminator line, or for a refused word (#9150).
fn find_terminator(text: &str, from: usize, p: &Pending) -> Option<(usize, usize)> {
    let word = p.word.as_deref()?;
    let mut start = from;
    while start <= text.len() {
        let end = text[start..].find('\n').map_or(text.len(), |n| start + n);
        // #9150: the scanner's own match, so both read a `\r` one way.
        if is_terminator(&text[start..end], word, p.strip_tabs, end == text.len()) {
            return Some((start, (end + 1).min(text.len())));
        }
        if end >= text.len() {
            return None;
        }
        start = end + 1;
    }
    None
}
