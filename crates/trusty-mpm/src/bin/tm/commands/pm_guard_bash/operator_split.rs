//! Where bash ends a word at an unquoted shell operator (#11262).
//!
//! Why: `shlex` splits on whitespace only, so `word>out`, `word<in` and
//! `word&` reached every rule as one word, and a rule that compares a word
//! (a program, a script path, a file operand) saw a name the shell never
//! uses. [`super::tokenize`] runs this pass before `shlex`.
//! What: [`split_operators`] returns the segment with a space inserted at each
//! place bash would end a word before or after an operator.
//! Test: `operator_split_cuts_each_redirect_class`,
//! `operator_split_keeps_refusing_glued_secret_reads`.

use std::borrow::Cow;

use super::super::heredoc::HeredocBodies;
use super::super::shell_lex::QuoteScan;

/// Why [`split_operators`] could not place the operators of a segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SplitError {
    /// bash and `shlex` disagree on what is quoted, or a `case` runs inside a
    /// substitution, whose `)` patterns this pass does not track.
    Ambiguous,
}

/// What an operator does to the word around it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    /// A redirection: the word ends before it, the target follows it.
    Redirect,
    /// A control operator: the word ends before and after it.
    Control,
}

/// One open quoting or substitution context.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    /// `"…"` or `$"…"`.
    Double,
    /// `$(…)`, `$((…))`, `<(…)`, `>(…)`, `((…))`, or a `(` glued to a word.
    Paren,
    /// `${…}`.
    Brace,
    /// `` `…` ``.
    Backtick,
}

/// `segment` with a space at every point where bash ends a word at an unquoted
/// operator (#11262).
///
/// Why: the guard's rules read `shlex` words; a word with an operator glued to
/// it is a name the shell never uses, so the rule that compares it misses.
/// What: a bash-faithful scan. Outside quotes and substitutions, a redirection
/// (`>`, `>>`, `>|`, `>&`, `<`, `<<`, `<<-`, `<<<`, `<&`, `<>`, `&>`, `&>>`, and the `|` clobber forms)
/// starts a new word unless the text before it is an fd number (`2>`) or a
/// `{name}`; its target stays glued to it, the shape `shlex` already gives
/// `cmd >out`. A control operator (`;`, `;;`, `&`, `&&`, `|`, `||`, `|&`) is
/// cut on both sides. Quoted and backslash-escaped bytes, here-document
/// bodies, `$(…)`, backticks, `${…}`, `$((…))`, `((…))`, process
/// substitution and a `(` glued to a word are never cut. A subshell `(` and
/// `)` stay glued as `shlex` leaves them, because the rules read `(cmd` today.
/// A cut is made only where [`QuoteScan`] (the `shlex` view) also says the
/// byte is unquoted; disagreement is [`SplitError::Ambiguous`]. Unbalanced
/// quoting returns the input unchanged, so `shlex` reports it.
/// Test: `operator_split_cuts_each_redirect_class`,
/// `operator_split_cuts_control_operators`,
/// `operator_split_leaves_quoted_and_nested_text_whole`,
/// `operator_split_refuses_what_it_cannot_place`.
pub(super) fn split_operators(segment: &str) -> Result<Cow<'_, str>, SplitError> {
    let quotes = QuoteScan::new(segment);
    if !quotes.balanced || !segment.bytes().any(|b| b"<>&;|".contains(&b)) {
        return Ok(Cow::Borrowed(segment));
    }
    let cuts = Scanner::new(segment, &quotes).run()?;
    if cuts.is_empty() {
        return Ok(Cow::Borrowed(segment));
    }
    let mut out = String::with_capacity(segment.len() + cuts.len());
    let mut from = 0;
    for cut in cuts {
        out.push_str(&segment[from..cut]);
        out.push(' ');
        from = cut;
    }
    out.push_str(&segment[from..]);
    Ok(Cow::Owned(out))
}

/// The byte walk behind [`split_operators`].
struct Scanner<'a> {
    text: &'a str,
    bytes: &'a [u8],
    quotes: &'a QuoteScan,
    bodies: Option<HeredocBodies>,
    stack: Vec<Ctx>,
    /// Start of the word being built at the top level, `None` between words.
    word: Option<usize>,
    cuts: Vec<usize>,
}

impl<'a> Scanner<'a> {
    fn new(text: &'a str, quotes: &'a QuoteScan) -> Self {
        // #11262: a here-document body is data; its bytes are never cut.
        let bodies = text.contains("<<").then(|| HeredocBodies::scan(text));
        Self {
            text,
            bytes: text.as_bytes(),
            quotes,
            bodies,
            stack: Vec::new(),
            word: None,
            cuts: Vec::new(),
        }
    }

    fn at(&self, i: usize) -> u8 {
        self.bytes.get(i).copied().unwrap_or(0)
    }

    fn starts(&self, i: usize, s: &str) -> bool {
        self.bytes
            .get(i..)
            .is_some_and(|rest| rest.starts_with(s.as_bytes()))
    }

    fn run(mut self) -> Result<Vec<usize>, SplitError> {
        let mut i = 0;
        while i < self.bytes.len() {
            if self
                .bodies
                .as_ref()
                .is_some_and(|b| b.suppresses_separator(i))
            {
                self.word = None;
                i += 1;
                continue;
            }
            i = match self.stack.last().copied() {
                None => self.top(i)?,
                Some(ctx) => self.nested(ctx, i)?,
            };
        }
        Ok(self.cuts)
    }

    /// Mark a word as started at `i` if none is open.
    fn begin(&mut self, i: usize) {
        self.word.get_or_insert(i);
    }

    /// One step at the top level, outside every quote and substitution.
    fn top(&mut self, i: usize) -> Result<usize, SplitError> {
        let c = self.at(i);
        if matches!(c, b' ' | b'\t' | b'\n') {
            self.word = None;
            return Ok(i + 1);
        }
        if c == b'#' && self.word.is_none() {
            // A comment runs to the end of the line.
            return Ok(self.text[i..]
                .find('\n')
                .map_or(self.bytes.len(), |n| i + n));
        }
        if let Some((len, op)) = self.operator(i) {
            return self.cut(i, len, op);
        }
        let at_word_start = self.word.is_none();
        self.begin(i);
        Ok(match c {
            b'\\' => self.skip_escape(i),
            b'\'' => self.skip_single(i + 1),
            b'$' if self.starts(i, "$'") => self.skip_ansi_c(i + 2),
            b'$' if self.starts(i, "$\"") => self.open(Ctx::Double, i + 2),
            b'"' => self.open(Ctx::Double, i + 1),
            b'`' => self.open(Ctx::Backtick, i + 1),
            b'$' if self.starts(i, "$(") => self.open(Ctx::Paren, i + 2),
            b'$' if self.starts(i, "${") => self.open(Ctx::Brace, i + 2),
            b'<' | b'>' => self.open(Ctx::Paren, i + 2), // `<(` / `>(`
            b'(' if at_word_start && self.starts(i, "((") => {
                self.stack.push(Ctx::Paren);
                self.open(Ctx::Paren, i + 2)
            }
            // A subshell `(` stays glued as `shlex` leaves it; a `(` glued to
            // a word (`a=(`, `f()`, an extglob) is not a bash cut either.
            b'(' if at_word_start => i + 1,
            b'(' => self.open(Ctx::Paren, i + 1),
            _ => i + 1,
        })
    }

    /// One step inside `ctx`.
    fn nested(&mut self, ctx: Ctx, i: usize) -> Result<usize, SplitError> {
        let c = self.at(i);
        if c == b'\\' {
            return Ok(self.skip_escape(i));
        }
        let closes = match ctx {
            Ctx::Double => c == b'"',
            Ctx::Paren => c == b')',
            Ctx::Brace => c == b'}',
            Ctx::Backtick => c == b'`',
        };
        if closes {
            self.stack.pop();
            return Ok(i + 1);
        }
        if ctx == Ctx::Backtick {
            return Ok(i + 1);
        }
        if ctx == Ctx::Paren && self.opens_case(i) {
            return Err(SplitError::Ambiguous);
        }
        Ok(match c {
            b'\'' if ctx != Ctx::Double => self.skip_single(i + 1),
            b'"' => self.open(Ctx::Double, i + 1),
            b'`' => self.open(Ctx::Backtick, i + 1),
            b'$' if self.starts(i, "$(") => self.open(Ctx::Paren, i + 2),
            b'$' if self.starts(i, "${") => self.open(Ctx::Brace, i + 2),
            b'(' if ctx == Ctx::Paren => self.open(Ctx::Paren, i + 1),
            _ => i + 1,
        })
    }

    /// Whether a `case` keyword starts at `i` inside a substitution.
    fn opens_case(&self, i: usize) -> bool {
        let before = i.checked_sub(1).map(|p| self.at(p));
        self.starts(i, "case")
            && matches!(self.at(i + 4), b' ' | b'\t' | b'\n')
            && before.is_none_or(|b| matches!(b, b' ' | b'\t' | b'\n' | b'(' | b';'))
    }

    /// The index after the character a `\` at `i` escapes.
    fn skip_escape(&self, i: usize) -> usize {
        let escaped = self.text.get(i + 1..).and_then(|t| t.chars().next());
        i + 1 + escaped.map_or(0, char::len_utf8)
    }

    fn open(&mut self, ctx: Ctx, next: usize) -> usize {
        self.stack.push(ctx);
        next
    }

    /// The index after the `'` closing a single-quoted string that opened
    /// before `from`.
    fn skip_single(&self, from: usize) -> usize {
        self.text[from.min(self.bytes.len())..]
            .find('\'')
            .map_or(self.bytes.len(), |n| from + n + 1)
    }

    /// The index after the `'` closing an ANSI-C `$'…'` string, where `\`
    /// escapes the next byte.
    fn skip_ansi_c(&self, mut i: usize) -> usize {
        while i < self.bytes.len() {
            match self.at(i) {
                b'\\' => i += 2,
                b'\'' => return i + 1,
                _ => i += 1,
            }
        }
        self.bytes.len()
    }

    /// The operator starting at `i`, longest spelling first; `None` for `<(`
    /// and `>(`, which open a process substitution inside the word.
    fn operator(&self, i: usize) -> Option<(usize, Op)> {
        const SPELLINGS: &[(&str, Op)] = &[
            // #11262: every clobber spelling `is_clobber_bar` knows keeps its
            // `|`, so it is never cut as a pipe.
            ("&>>|", Op::Redirect),
            (">>|", Op::Redirect),
            (">&|", Op::Redirect),
            ("&>|", Op::Redirect),
            ("<<<", Op::Redirect),
            ("<<-", Op::Redirect),
            ("&>>", Op::Redirect),
            (";;&", Op::Control),
            ("<<", Op::Redirect),
            ("<>", Op::Redirect),
            ("<&", Op::Redirect),
            (">>", Op::Redirect),
            (">&", Op::Redirect),
            (">|", Op::Redirect),
            ("&>", Op::Redirect),
            (";;", Op::Control),
            (";&", Op::Control),
            ("&&", Op::Control),
            ("||", Op::Control),
            ("|&", Op::Control),
            ("<", Op::Redirect),
            (">", Op::Redirect),
            (";", Op::Control),
            ("&", Op::Control),
            ("|", Op::Control),
        ];
        if self.starts(i, "<(") || self.starts(i, ">(") {
            return None;
        }
        SPELLINGS
            .iter()
            .find(|(s, _)| self.starts(i, s))
            .map(|(s, op)| (s.len(), *op))
    }

    /// Record the cuts an operator of `len` bytes at `i` makes.
    fn cut(&mut self, i: usize, len: usize, op: Op) -> Result<usize, SplitError> {
        // #11262: every cut lands where `shlex` also reads live syntax, or a
        // space would be inserted into what `shlex` keeps as quoted text.
        if !(i..i + len).all(|at| self.quotes.is_unquoted(at)) {
            return Err(SplitError::Ambiguous);
        }
        let prefix = self.word.map_or("", |start| &self.text[start..i]);
        let redirect_prefix =
            op == Op::Redirect && matches!(self.at(i), b'<' | b'>') && is_redirect_prefix(prefix);
        if !prefix.is_empty() && !redirect_prefix {
            self.cuts.push(i);
            self.word = None;
        }
        let end = i + len;
        match op {
            Op::Redirect => self.begin(i),
            Op::Control => {
                if end < self.bytes.len() {
                    self.cuts.push(end);
                }
                self.word = None;
            }
        }
        Ok(end)
    }
}

/// Whether `prefix` belongs to the redirection after it: an fd number (`2>`)
/// or a `{name}` variable (`{fd}>`), the two shapes bash reads as part of it.
fn is_redirect_prefix(prefix: &str) -> bool {
    let digits = !prefix.is_empty() && prefix.bytes().all(|b| b.is_ascii_digit());
    let named = prefix
        .strip_prefix('{')
        .and_then(|p| p.strip_suffix('}'))
        .is_some_and(|name| {
            name.bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        });
    digits || named
}

#[cfg(test)]
#[path = "operator_split_tests.rs"]
mod tests;
