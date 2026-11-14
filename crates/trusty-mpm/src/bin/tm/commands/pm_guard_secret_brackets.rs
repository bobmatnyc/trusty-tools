//! Bracket expressions (`[…]`) in a word the secret-read guard screens.
//!
//! Why: #10761 (#10548) — `cat .[!x]nv` and `cat .[^x]nv` ALLOWED on 1.14.0
//! while a shell expands both onto `.env`. [`is_path_byte`] cut the word at the
//! `!` and `^` negation sigils, so the class never reached the classifier
//! whole, and the old `?` stand-in read `[!]x]` as a one-member class.
//! What: [`parse_bracket`] reads one expression the way POSIX `fnmatch`, bash
//! and zsh do. [`path_words`] is the byte cut with each expression kept whole,
//! [`bracket_readings`] turns a word into every literal name it can reach, and
//! [`widen_brackets`] is the `?` stand-in both fall back to.
//! Test: the `pm_guard_secret_brackets_tests` module.

use super::pm_guard_secret_words::is_path_byte;

/// The most names [`bracket_readings`] enumerates before it widens instead.
const MAX_READINGS: usize = 256;

/// The stand-in for every character no secret-file rule spells.
///
/// No denylist entry, placeholder suffix or exemption names `%`, so it answers
/// exactly as a non-ASCII or punctuation character a class reaches would.
const NEUTRAL: char = '%';

/// One parsed bracket expression.
enum Bracket {
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
enum Item {
    Char(char),
    Range(char, char),
    Named(fn(&char) -> bool),
}

impl Item {
    /// Whether `c` is a member. `wide` also admits the other case of a letter
    /// inside a range, which a locale collation (`aAbB…`) can put there.
    fn matches(&self, c: char, wide: bool) -> bool {
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

/// Parse the bracket expression whose `[` sits at `open`.
///
/// Why: a `?` stand-in found by "the next `]`" misreads `[!]x]` and `[]e]`,
/// where a `]` right after the `[` or the sigil is a member, not the close.
/// What: an optional `!` or `^` sigil, then members until an unescaped `]`
/// that is not first: single characters, `a-b` ranges, `[:name:]` classes and
/// `[.c.]`/`[=c=]` elements. Quoting or `$` inside makes the class
/// [`Bracket::Opaque`]. With `shell_text`, unquoted whitespace or a shell
/// operator ends the word first, which makes the `[` unterminated.
/// Test: `parses_every_bracket_form_a_shell_accepts`.
fn parse_bracket(chars: &[char], open: usize, shell_text: bool) -> Bracket {
    let mut i = open + 1;
    let negated = matches!(chars.get(i), Some('!' | '^'));
    if negated {
        i += 1;
    }
    let body_start = i;
    let mut items = Vec::new();
    let mut opaque = false;
    while let Some(&c) = chars.get(i) {
        if c == ']' && i > body_start {
            return if opaque {
                Bracket::Opaque { end: i }
            } else {
                Bracket::Class {
                    end: i,
                    negated,
                    items,
                }
            };
        }
        if shell_text && (c.is_whitespace() || matches!(c, ';' | '|' | '&' | '<' | '>' | '(' | ')'))
        {
            return Bracket::Unterminated;
        }
        match c {
            '\\' => {
                opaque = true;
                i += 2;
            }
            '\'' | '"' => {
                opaque = true;
                if let Some(len) = chars[i + 1..].iter().position(|q| *q == c) {
                    i += len + 2;
                    continue;
                }
                // An unbalanced quote is unreadable, so the class it opens is
                // widened rather than read as a literal `[`.
                return match chars[i + 1..].iter().position(|q| *q == ']') {
                    Some(len) => Bracket::Opaque { end: i + 1 + len },
                    None => Bracket::Unterminated,
                };
            }
            '$' | '`' => {
                opaque = true;
                i += 1;
            }
            '[' if matches!(chars.get(i + 1), Some(':' | '.' | '=')) => {
                let delim = chars[i + 1];
                let close = (i + 2..chars.len().saturating_sub(1))
                    .find(|&k| chars[k] == delim && chars[k + 1] == ']');
                if let Some(k) = close {
                    let name: String = chars[i + 2..k].iter().collect();
                    match element_item(delim, &name) {
                        Some(item) => items.push(item),
                        None => opaque = true,
                    }
                    i = k + 2;
                } else {
                    items.push(Item::Char('['));
                    i += 1;
                }
            }
            _ => match (chars.get(i + 1), chars.get(i + 2)) {
                (Some('-'), Some(&hi)) if hi != ']' => {
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
            },
        }
    }
    Bracket::Unterminated
}

/// The character standing for `c` in a reading: itself when a secret-file
/// rule could spell it, else [`NEUTRAL`].
fn representative(c: char) -> char {
    if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '@' | '~') {
        c
    } else {
        NEUTRAL
    }
}

/// Every reading character a class reaches, without repeats.
///
/// What: tests each printable ASCII character except `/` (which no bracket
/// expression matches in a pathname). A negated class compares against its
/// members exactly; a positive one with [`Item::matches`]'s `wide` reading, so
/// a locale collation can only add to it. A negated class always reaches some
/// non-ASCII name character, which [`NEUTRAL`] stands for.
fn reachable(negated: bool, items: &[Item]) -> Vec<char> {
    let mut out = Vec::new();
    let mut push = |c: char| {
        if !out.contains(&c) {
            out.push(c);
        }
    };
    if negated {
        push(NEUTRAL);
    }
    for c in (' '..='~').filter(|c| *c != '/') {
        let hit = if negated {
            !items.iter().any(|item| item.matches(c, false))
        } else {
            items.iter().any(|item| item.matches(c, true))
        };
        if hit {
            push(representative(c));
        }
    }
    if !negated
        && items
            .iter()
            .any(|item| matches!(item, Item::Char(m) if !m.is_ascii()))
    {
        push(NEUTRAL);
    }
    out
}

/// `name` with every bracket expression collapsed to `?`, a stray `]` dropped
/// and an unterminated `[` replaced by `unterminated`.
///
/// Why: the bounded fallback for [`bracket_readings`] and the shape
/// [`normalize_bracket_classes`](super::pm_guard_secret_words::normalize_bracket_classes)
/// returns. `?` reaches every character a class does, so it can only add a deny.
/// Test: `normalize_bracket_classes_collapses_a_class_and_drops_a_stray`,
/// `parses_every_bracket_form_a_shell_accepts`.
pub(crate) fn widen_brackets(name: &str, unterminated: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '[' => match parse_bracket(&chars, i, false) {
                Bracket::Class { end, .. } | Bracket::Opaque { end } => {
                    out.push('?');
                    i = end + 1;
                }
                Bracket::Unterminated => {
                    out.push_str(unterminated);
                    i += 1;
                }
            },
            ']' => i += 1,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Every literal name a word's bracket expressions can produce.
///
/// Why: #10761 — a negated class is judged by what it can REACH. `.[!x]nv`
/// reaches `.env` and denies; `.[!eE]nv` reaches no secret-class name and
/// allows. `.[!e]nv` denies: bash globbing is case-sensitive, so it matches
/// `.Env`, which this guard's case-insensitive classifier calls a secret.
/// What: a word with no bracket is its own one reading. Otherwise each class
/// contributes every [`reachable`] character, an opaque class `?`, and an
/// unterminated `[` both nothing (the earlier reading) and `?` (the shell's
/// literal `[`); a stray `]` is dropped. Over [`MAX_READINGS`] combinations
/// the word widens to [`widen_brackets`] instead, so a parse that cannot be
/// enumerated still fails closed.
/// Test: `denies_a_negated_or_ranged_bracket_glob_onto_a_secret_name`,
/// `allows_a_bracket_glob_that_reaches_no_secret_name`,
/// `a_bracket_the_guard_cannot_read_fails_closed`.
pub(crate) fn bracket_readings(name: &str) -> Vec<String> {
    if !name.contains(['[', ']']) {
        return vec![name.to_string()];
    }
    let chars: Vec<char> = name.chars().collect();
    let mut parts: Vec<Vec<String>> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '[' => match parse_bracket(&chars, i, false) {
                Bracket::Class {
                    end,
                    negated,
                    items,
                } => {
                    parts.push(
                        reachable(negated, &items)
                            .into_iter()
                            .map(String::from)
                            .collect(),
                    );
                    i = end + 1;
                }
                Bracket::Opaque { end } => {
                    parts.push(vec!["?".to_string()]);
                    i = end + 1;
                }
                Bracket::Unterminated => {
                    parts.push(vec![String::new(), "?".to_string()]);
                    i += 1;
                }
            },
            ']' => i += 1,
            c => {
                parts.push(vec![c.to_string()]);
                i += 1;
            }
        }
    }
    let bounded = parts.iter().try_fold(1usize, |total, part| {
        total.checked_mul(part.len()).filter(|n| *n <= MAX_READINGS)
    });
    if bounded.is_none() {
        return vec![widen_brackets(name, ""), widen_brackets(name, "?")];
    }
    parts.iter().fold(vec![String::new()], |prefixes, part| {
        prefixes
            .iter()
            .flat_map(|prefix| part.iter().map(move |piece| format!("{prefix}{piece}")))
            .collect()
    })
}

/// The candidate path words of `text`, without repeats: the plain cut at every
/// non-[`is_path_byte`] byte, then the same cut with each bracket expression
/// kept whole.
///
/// Why: #10761 — the plain cut split `.[!x]nv` at `!` into `.[` and `x]nv`,
/// neither of which names a secret, so the deny never fired. The plain words
/// stay first and unchanged: a bracket span that swallowed a quoted `.env`
/// (`'commands=["grep -c K /srv/.env"]'`) would otherwise hide it.
/// What: [`cut`] three ways — plain; joining a bracket expression that ends
/// inside one shell word; and joining one read as a single argv token, where a
/// space or quote is a member. Joined words can only add a deny.
/// Test: `denies_a_negated_or_ranged_bracket_glob_onto_a_secret_name`,
/// `keeps_a_bracket_expression_whole_through_the_cut`.
pub(crate) fn path_words(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut words = cut(&chars, None);
    if chars.contains(&'[') {
        for word in cut(&chars, Some(true))
            .into_iter()
            .chain(cut(&chars, Some(false)))
        {
            if !words.contains(&word) {
                words.push(word);
            }
        }
    }
    words
}

/// One pass of [`path_words`]: `join` is `None` for the plain cut, else the
/// `shell_text` mode [`parse_bracket`] reads each `[` with.
fn cut(chars: &[char], join: Option<bool>) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '['
            && let Some(shell_text) = join
            && let Bracket::Class { end, .. } | Bracket::Opaque { end } =
                parse_bracket(chars, i, shell_text)
        {
            word.extend(&chars[i..=end]);
            i = end + 1;
            continue;
        }
        if is_path_byte(chars[i]) {
            word.push(chars[i]);
        } else if !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        i += 1;
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}
