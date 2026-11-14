//! The bracket-expression (`[…]`) parser behind the secret-read guard.
//!
//! Why: #10761 review — the first parser rescanned the rest of the text at
//! every `[` and every `[:`, so 8 KB of `[:` took 31 s and a long command could
//! overrun the hook budget before any deny returned.
//! What: [`Scanner`] precomputes, right to left, where a class body scan that
//! reaches each position would close. A parse then answers an unclosed `[` in
//! O(1) and walks only the class it returns, so a whole text costs O(n).
//! Test: `a_long_run_of_unclosed_brackets_scans_in_bounded_time`,
//! `parses_every_bracket_form_a_shell_accepts`.

/// One parsed bracket expression.
pub(crate) enum Bracket {
    /// A readable class whose closing `]` sits at `end`.
    Class {
        end: usize,
        negated: bool,
        items: Vec<Item>,
    },
    /// A terminated class this parser cannot read exactly — an unknown named
    /// class, a multi-character collating element, a reversed range, or shell
    /// quoting inside. It is widened to `?`, never read as matching nothing.
    Opaque { end: usize },
    /// No closing `]`: the shell reads the `[` literally.
    Unterminated,
}

/// One member of a class.
pub(crate) enum Item {
    Char(char),
    Range(char, char),
    Named(fn(&char) -> bool),
}

impl Item {
    /// Whether `c` is a member. `wide` also admits the other case of a letter
    /// inside a range, which a locale collation (`aAbB…`) can put there.
    pub(crate) fn matches(&self, c: char, wide: bool) -> bool {
        match self {
            Item::Char(m) => c == *m,
            Item::Range(lo, hi) => {
                let within = |x: char| (*lo..=*hi).contains(&x);
                within(c)
                    || (wide && within(c.to_ascii_uppercase()))
                    || (wide && within(c.to_ascii_lowercase()))
            }
            Item::Named(class) => class(&c),
        }
    }
}

fn is_blank(c: &char) -> bool {
    matches!(c, ' ' | '\t')
}

fn is_print(c: &char) -> bool {
    c.is_ascii_graphic() || *c == ' '
}

fn is_space(c: &char) -> bool {
    c.is_ascii_whitespace() || *c == '\x0b'
}

fn is_word(c: &char) -> bool {
    c.is_ascii_alphanumeric() || *c == '_'
}

/// The `[:name:]` class bash and zsh accept; `None` for any other name.
fn named_class(name: &str) -> Option<fn(&char) -> bool> {
    let class: fn(&char) -> bool = match name {
        "alnum" => char::is_ascii_alphanumeric,
        "alpha" => char::is_ascii_alphabetic,
        "ascii" => char::is_ascii,
        "blank" => is_blank,
        "cntrl" => char::is_ascii_control,
        "digit" => char::is_ascii_digit,
        "graph" => char::is_ascii_graphic,
        "lower" => char::is_ascii_lowercase,
        "print" => is_print,
        "punct" => char::is_ascii_punctuation,
        "space" => is_space,
        "upper" => char::is_ascii_uppercase,
        "word" => is_word,
        "xdigit" => char::is_ascii_hexdigit,
        _ => return None,
    };
    Some(class)
}

/// The member a `[:…:]`, `[.….]` or `[=…=]` element names, or `None` when this
/// parser cannot read it.
fn element_item(delim: char, name: &str) -> Option<Item> {
    if delim == ':' {
        return named_class(name).map(Item::Named);
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(Item::Char(c)),
        _ => None,
    }
}

/// The element delimiters `[:`, `[.` and `[=` open.
const DELIMS: [char; 3] = [':', '.', '='];

/// A text prepared for O(1) bracket lookups.
///
/// Why: #10761 review (F3) — see the module doc.
/// What: `term[p]` is the index of the `]` a class body scan arriving at
/// position `p` (not the body's first character) stops at, or `None` when it
/// reaches the end or, with `shell_text`, the end of the shell word first.
/// `pair[d][p]` is the first `k >= p` with `d` then `]` at `k`; `next_rb`,
/// `next_sq` and `next_dq` are the next `]`, `'` and `"` at or after `p`.
/// Test: `a_long_run_of_unclosed_brackets_scans_in_bounded_time`.
pub(crate) struct Scanner<'a> {
    chars: &'a [char],
    term: Vec<Option<usize>>,
    pair: [Vec<Option<usize>>; 3],
}

/// `next[p]` = the first index `>= p` whose character satisfies `hit`.
fn next_where(chars: &[char], hit: impl Fn(usize) -> bool) -> Vec<Option<usize>> {
    let mut next = vec![None; chars.len() + 1];
    for p in (0..chars.len()).rev() {
        next[p] = if hit(p) { Some(p) } else { next[p + 1] };
    }
    next
}

impl<'a> Scanner<'a> {
    /// Precompute every lookup for `chars` in one right-to-left pass each.
    pub(crate) fn new(chars: &'a [char], shell_text: bool) -> Self {
        let n = chars.len();
        let pair =
            DELIMS.map(|d| next_where(chars, |p| chars[p] == d && chars.get(p + 1) == Some(&']')));
        let next_rb = next_where(chars, |p| chars[p] == ']');
        let next_sq = next_where(chars, |p| chars[p] == '\'');
        let next_dq = next_where(chars, |p| chars[p] == '"');
        let mut term: Vec<Option<usize>> = vec![None; n + 4];
        for p in (0..n).rev() {
            let at = |q: usize| term.get(q).copied().flatten();
            let c = chars[p];
            let value = if c == ']' {
                Some(p)
            } else if shell_text && ends_shell_word(c) {
                None
            } else {
                match c {
                    '\\' => at(p + 2),
                    '\'' | '"' => {
                        let quotes = if c == '\'' { &next_sq } else { &next_dq };
                        match quotes[p + 1] {
                            Some(q) => at(q + 1),
                            // An unbalanced quote: the class closes at the next `]`.
                            None => next_rb[p + 1],
                        }
                    }
                    '[' if chars.get(p + 1).is_some_and(|d| DELIMS.contains(d)) => {
                        let d = delim_index(chars[p + 1]);
                        match pair[d].get(p + 2).copied().flatten() {
                            Some(k) => at(k + 2),
                            None => at(p + 1),
                        }
                    }
                    '$' | '`' => at(p + 1),
                    _ => at(p + member_width(chars, p)),
                }
            };
            term[p] = value;
        }
        Scanner { chars, term, pair }
    }

    /// The `]` a scan arriving at `p` stops at.
    fn at(&self, p: usize) -> Option<usize> {
        self.term.get(p).copied().flatten()
    }

    /// Whether the `[` at `open` starts with a `!`/`^` negation sigil.
    pub(crate) fn has_sigil(&self, open: usize) -> bool {
        matches!(self.chars.get(open + 1), Some('!' | '^'))
    }

    /// The body start of the `[` at `open`: past an optional `!`/`^` sigil,
    /// unless `literal_sigil` reads that sigil as a plain member.
    fn body_start(&self, open: usize, literal_sigil: bool) -> (usize, bool) {
        let negated = !literal_sigil && self.has_sigil(open);
        (open + 1 + usize::from(negated), negated)
    }

    /// The scan position after the `]` at `k` is read as a member.
    fn past_member(&self, k: usize) -> usize {
        k + member_width(self.chars, k)
    }

    /// Every `]` that can close the `[` at `open`, nearest first, at most
    /// `limit + 1` of them. Empty when the `[` is unterminated.
    ///
    /// Why: #10761 review (F1) — the lexer strips the quoting that made a `]` a
    /// member (`.[x\]e]nv` arrives as `.[x]e]nv`), so each later `]` the scan
    /// reaches past an earlier one is a close the shell may have used.
    /// #10761: `literal_sigil` reads a `!`/`^` sigil as a member, the way the
    /// shell read a quoted one (`.[\!eE]nv`) before the lexer dropped the `\`.
    /// Test: `a_quoted_or_escaped_close_bracket_inside_a_class_fails_closed`,
    /// `a_quoted_negation_sigil_is_judged_as_a_member`.
    pub(crate) fn closes(&self, open: usize, literal_sigil: bool, limit: usize) -> Vec<usize> {
        let (body, _) = self.body_start(open, literal_sigil);
        let mut next = match self.chars.get(body) {
            None => None,
            Some(']') => self.at(self.past_member(body)),
            Some(_) => self.at(body),
        };
        let mut out = Vec::new();
        while let Some(end) = next {
            out.push(end);
            if out.len() > limit {
                break;
            }
            next = self.at(self.past_member(end));
        }
        out
    }

    /// Parse the `[` at `open`, closing at the nearest `]`.
    ///
    /// Why: a `?` stand-in found by "the next `]`" misreads `[!]x]` and `[]e]`,
    /// where a `]` right after the `[` or the sigil is a member, not the close.
    /// What: an optional `!` or `^` sigil, then members until an unescaped `]`
    /// that is not first: single characters, `a-b` ranges, `[:name:]` classes
    /// and `[.c.]`/`[=c=]` elements. Quoting or `$` inside makes the class
    /// [`Bracket::Opaque`]. With `shell_text`, unquoted whitespace or a shell
    /// operator ends the word first, which makes the `[` unterminated.
    /// Test: `parses_every_bracket_form_a_shell_accepts`.
    pub(crate) fn parse(&self, open: usize) -> Bracket {
        match self.closes(open, false, 0).first() {
            Some(&end) => self.class_to(open, end, false),
            None => Bracket::Unterminated,
        }
    }

    /// Read the `[` at `open` as a class closed by the `]` at `end`, every
    /// earlier `]` a member. `end` must come from [`Scanner::closes`] called
    /// with the same `literal_sigil`.
    pub(crate) fn class_to(&self, open: usize, end: usize, literal_sigil: bool) -> Bracket {
        let chars = self.chars;
        let (mut i, negated) = self.body_start(open, literal_sigil);
        let mut items = Vec::new();
        let mut opaque = false;
        while i < end {
            let c = chars[i];
            match c {
                '\\' => {
                    opaque = true;
                    i += 2;
                }
                '\'' | '"' => {
                    opaque = true;
                    match chars[i + 1..end].iter().position(|q| *q == c) {
                        Some(len) => i += len + 2,
                        // An unbalanced quote: unreadable up to the close.
                        None => i = end,
                    }
                }
                '$' | '`' => {
                    opaque = true;
                    i += 1;
                }
                '[' if chars.get(i + 1).is_some_and(|d| DELIMS.contains(d)) => {
                    let d = delim_index(chars[i + 1]);
                    match self.pair[d].get(i + 2).copied().flatten() {
                        Some(k) if k < end => {
                            let name: String = chars[i + 2..k].iter().collect();
                            match element_item(chars[i + 1], &name) {
                                Some(item) => items.push(item),
                                None => opaque = true,
                            }
                            i = k + 2;
                        }
                        _ => {
                            items.push(Item::Char('['));
                            i += 1;
                        }
                    }
                }
                _ if member_width(chars, i) == 3 => {
                    let hi = chars[i + 2];
                    if c > hi || !c.is_ascii() || !hi.is_ascii() || "[\\'\"$`".contains(hi) {
                        opaque = true;
                    } else {
                        items.push(Item::Range(c, hi));
                    }
                    i += 3;
                }
                _ => {
                    items.push(Item::Char(c));
                    i += 1;
                }
            }
        }
        if opaque || i != end {
            Bracket::Opaque { end }
        } else {
            Bracket::Class {
                end,
                negated,
                items,
            }
        }
    }
}

/// Whether unquoted `c` ends a shell word.
fn ends_shell_word(c: char) -> bool {
    c.is_whitespace() || matches!(c, ';' | '|' | '&' | '<' | '>' | '(' | ')')
}

/// The index of `d` in [`DELIMS`].
fn delim_index(d: char) -> usize {
    DELIMS.iter().position(|x| *x == d).unwrap_or(0)
}

/// 3 when the plain member at `p` opens an `a-b` range, else 1.
fn member_width(chars: &[char], p: usize) -> usize {
    match (chars.get(p + 1), chars.get(p + 2)) {
        (Some('-'), Some(&hi)) if hi != ']' => 3,
        _ => 1,
    }
}
