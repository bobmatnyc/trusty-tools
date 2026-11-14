//! Bracket expressions (`[…]`) in a word the secret-read guard screens.
//!
//! Why: #10761 (#10548) — `cat .[!x]nv` and `cat .[^x]nv` ALLOWED on 1.14.0
//! while a shell expands both onto `.env`. [`is_path_byte`] cut the word at the
//! `!` and `^` negation sigils, so the class never reached the classifier
//! whole, and the old `?` stand-in read `[!]x]` as a one-member class.
//! What: [`Scanner`] reads each expression the way POSIX `fnmatch`, bash and
//! zsh do. [`path_words`] is the byte cut with each expression kept whole,
//! [`bracket_readings`] turns a word into every literal name it can reach, and
//! [`widen_brackets`] is the `?` stand-in both fall back to.
//! Test: the `pm_guard_secret_brackets_tests` module.

use super::pm_guard_secret_bracket_parse::{Bracket, Item, Scanner};
use super::pm_guard_secret_words::is_path_byte;
use std::collections::{HashMap, HashSet};

/// The most names [`bracket_readings`] enumerates before it widens instead.
pub(crate) const MAX_READINGS: usize = 256;

/// The most closing `]` one `[` may have before its word is unreadable.
const MAX_CLOSES: usize = 16;

/// The stand-in for every character no secret-file rule spells.
///
/// No denylist entry, placeholder suffix or exemption names `%`, so it answers
/// exactly as a non-ASCII or punctuation character a class reaches would.
const NEUTRAL: char = '%';

/// The character standing for `c` in a reading: itself when a secret-file
/// rule could spell it, else [`NEUTRAL`]. `slash` keeps a `/` (see
/// [`glob_readings`]).
pub(crate) fn representative(c: char, slash: bool) -> char {
    if c.is_ascii_alphanumeric()
        || matches!(c, '.' | '_' | '-' | '+' | '@' | '~')
        || (slash && c == '/')
    {
        c
    } else {
        NEUTRAL
    }
}

/// Every reading character a class reaches, without repeats.
///
/// What: tests each printable ASCII character; a `/` only with `slash`, since
/// no shell bracket expression matches one in a pathname but a globset class
/// does. A negated class compares against its members exactly; a positive one
/// with [`Item::matches`]'s `wide` reading, so a locale collation can only add
/// to it. A negated class always reaches some non-ASCII name character, which
/// [`NEUTRAL`] stands for.
pub(crate) fn reachable(negated: bool, items: &[Item], slash: bool) -> Vec<char> {
    let mut out = Vec::new();
    let mut push = |c: char| {
        if !out.contains(&c) {
            out.push(c);
        }
    };
    if negated {
        push(NEUTRAL);
    }
    for c in (' '..='~').filter(|c| slash || *c != '/') {
        let hit = if negated {
            !items.iter().any(|item| item.matches(c, false))
        } else {
            items.iter().any(|item| item.matches(c, true))
        };
        if hit {
            push(representative(c, slash));
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
    let scanner = Scanner::new(&chars, false);
    let mut out = String::with_capacity(name.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '[' => match scanner.parse(i) {
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

/// Every literal name a word's bracket expressions can produce, or `None` when
/// the word is unreadable and the caller must deny.
///
/// Why: #10761 — a negated class is judged by what it can REACH. `.[!x]nv`
/// reaches `.env` and denies; `.[!eE]nv` denies too (see the follow-up note
/// below). `.[!e]nv` denies: bash globbing is case-sensitive, so it matches
/// `.Env`, which this guard's case-insensitive classifier calls a secret.
/// What: a word with no bracket is its own one reading. Otherwise each class
/// contributes every [`reachable`] character, an opaque class `?`, and an
/// unterminated `[` both nothing (the earlier reading) and `?` (the shell's
/// literal `[`); a stray `]` is dropped. #10761 review: the lexer strips the
/// quoting that made a `]` a member, so every `]` [`Scanner::closes`] finds is
/// tried as the close. Over [`MAX_READINGS`] names each class is read as `?`;
/// still over, an unambiguous word widens to [`widen_brackets`] and an
/// ambiguous one is `None`.
///
/// #10761 follow-up: the lexer also strips the quoting that made a `!`/`^`
/// sigil a member (`.[\!eE]nv` arrives as `.[!eE]nv`), so a class opening
/// with one is read both ways — negated, and with the sigil as a member. The
/// judged set is never narrower than what bash or zsh could expand.
/// Test: `denies_a_negated_or_ranged_bracket_glob_onto_a_secret_name`,
/// `allows_a_bracket_glob_that_reaches_no_secret_name`,
/// `a_bracket_the_guard_cannot_read_fails_closed`,
/// `a_quoted_or_escaped_close_bracket_inside_a_class_fails_closed`,
/// `a_quoted_negation_sigil_is_judged_as_a_member`.
pub(crate) fn bracket_readings(name: &str) -> Option<Vec<String>> {
    classify(name, false).1
}

/// [`bracket_readings`] for a `Grep` tool glob, where a class may hold `/`.
///
/// Why: #10761 review (F2) — ripgrep's globset reads `/` inside a class as a
/// member, so `.e[!/]v` matched `.env` while the basename cut split it.
/// What: the whole glob is read with `/` reachable; the caller takes each
/// reading's basename afterwards. #10761 follow-up: kept beside
/// [`rg_glob_readings`](super::pm_guard_secret_rg_glob::rg_glob_readings) so
/// the `Grep` arm refuses everything it refused before.
/// Test: `a_grep_glob_class_carrying_a_slash_is_read_whole`,
/// `a_grep_glob_refused_by_shell_rules_stays_refused`.
pub(crate) fn glob_readings(glob: &str) -> Option<Vec<String>> {
    classify(glob, true).1
}

/// [`bracket_readings`] (`slash` keeps a `/` member), plus whether the exact
/// enumeration overflowed [`MAX_READINGS`] so each class was read as `?`.
fn classify(name: &str, slash: bool) -> (bool, Option<Vec<String>>) {
    if !name.contains(['[', ']']) {
        return (false, Some(vec![name.to_string()]));
    }
    let chars: Vec<char> = name.chars().collect();
    let scanner = Scanner::new(&chars, false);
    let mut classes = HashMap::new();
    let mut ambiguous = false;
    for widen in [false, true] {
        match enumerate(&chars, &scanner, &mut classes, widen, slash) {
            Enumerated::Names(names) => return (widen, Some(names)),
            Enumerated::Unreadable => return (widen, None),
            Enumerated::TooMany { ambiguous: a } => ambiguous |= a,
        }
    }
    if ambiguous {
        return (true, None);
    }
    let widened = vec![widen_brackets(name, ""), widen_brackets(name, "?")];
    (true, Some(widened))
}

/// The refusal for a word denied because its bracket expressions could not be
/// read exactly, or `None` when they were.
///
/// Why: #10761 follow-up (E) — such a word fails closed, and the refusal said
/// its name "is in this guard's secret-bearing file class", which misled for
/// `find -name '[0-9][0-9]…'` and `jq '.a[0][1]…[8]'`: past [`MAX_READINGS`]
/// combinations each class is read as `?`, and `????-??*.log` can reach a
/// `*secrets*` name the digits never could.
/// What: answers when [`bracket_readings`] of the word's basename is `None`,
/// or needed the `?` widening.
/// Test: `an_unreadable_bracket_word_is_refused_with_its_own_reason`.
pub(crate) fn unreadable_reason(target: &str, how: &str) -> Option<String> {
    let base = super::pm_guard_secret_read::command_basename(target);
    let (widened, readings) = classify(&base, false);
    let why = match readings {
        None => "has more readings than this guard can check",
        Some(_) if widened => {
            "has more combinations than this guard enumerates, so each class was read as `?`, \
             and that wider name can reach its secret-bearing file class"
        }
        Some(_) => return None,
    };
    Some(format!(
        "naming `{target}` in {how} is refused (issue #10761) — its bracket expression {why}. \
         The shell lexer drops the quoting that decides which `]` closes a class and whether \
         `!`/`^` negates, so the guard fails closed rather than allow a name it could not judge. \
         Use fewer bracket expressions in one word, or let the program read the pattern from a \
         file."
    ))
}

/// The outcome of one [`enumerate`] pass.
enum Enumerated {
    Names(Vec<String>),
    Unreadable,
    TooMany { ambiguous: bool },
}

/// Each way the `[` at an index reads: the position after it and its pieces.
type Choices = Vec<(usize, Vec<String>)>;

/// One bounded enumeration of every reading of `chars`; `widen` reads each
/// class as `?` instead of its reachable characters.
fn enumerate(
    chars: &[char],
    scanner: &Scanner,
    classes: &mut HashMap<(usize, bool), Choices>,
    widen: bool,
    slash: bool,
) -> Enumerated {
    let mut states = vec![(0usize, String::new())];
    let mut done: Vec<String> = Vec::new();
    let mut ambiguous = false;
    while let Some((i, mut prefix)) = states.pop() {
        let Some(&c) = chars.get(i) else {
            if !done.contains(&prefix) {
                done.push(prefix);
            }
            continue;
        };
        match c {
            '[' => {
                // #10761: a sigil may have been quoted, so it is also a member.
                // A `Grep` glob (`slash`) reaches ripgrep unlexed: never quoted.
                let sigil = !slash && scanner.has_sigil(i);
                let negated = scanner.closes(i, false, MAX_CLOSES);
                let literal = if sigil {
                    scanner.closes(i, true, MAX_CLOSES)
                } else {
                    Vec::new()
                };
                if negated.len() > MAX_CLOSES || literal.len() > MAX_CLOSES {
                    // #10761: more candidate closes than the guard tries.
                    return Enumerated::Unreadable;
                }
                let mut all = negated.clone();
                all.extend(literal.iter().filter(|end| !negated.contains(end)));
                ambiguous |= all.len() > 1;
                let choices = classes.entry((i, widen)).or_insert_with(|| {
                    let mut choices = choices_at(scanner, i, &negated, widen, slash, false);
                    if !literal.is_empty() {
                        choices.extend(choices_at(scanner, i, &literal, widen, slash, true));
                    }
                    choices
                });
                for (next, pieces) in choices.iter() {
                    for piece in pieces {
                        states.push((*next, format!("{prefix}{piece}")));
                    }
                }
            }
            ']' => states.push((i + 1, prefix)),
            c => {
                prefix.push(c);
                states.push((i + 1, prefix));
            }
        }
        if states.len() + done.len() > MAX_READINGS {
            return Enumerated::TooMany { ambiguous };
        }
    }
    Enumerated::Names(done)
}

/// Every reading of the `[` at `open` given its candidate `closes`, read with
/// `literal_sigil` as [`Scanner::closes`] found them.
fn choices_at(
    scanner: &Scanner,
    open: usize,
    closes: &[usize],
    widen: bool,
    slash: bool,
    literal_sigil: bool,
) -> Choices {
    if closes.is_empty() {
        return vec![(open + 1, vec![String::new(), "?".to_string()])];
    }
    closes
        .iter()
        .map(|&end| {
            let pieces = match scanner.class_to(open, end, literal_sigil) {
                Bracket::Class { negated, items, .. } if !widen => {
                    reachable(negated, &items, slash)
                        .into_iter()
                        .map(String::from)
                        .collect()
                }
                _ => vec!["?".to_string()],
            };
            (end + 1, pieces)
        })
        .collect()
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
/// space or quote is a member. Joined words can only add a deny. Each joined
/// cut uses one [`Scanner`], so the whole text costs O(n) (#10761 review).
/// Test: `denies_a_negated_or_ranged_bracket_glob_onto_a_secret_name`,
/// `keeps_a_bracket_expression_whole_through_the_cut`,
/// `a_long_run_of_unclosed_brackets_scans_in_bounded_time`.
pub(crate) fn path_words(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut words = cut(&chars, None);
    if chars.contains(&'[') {
        // #10761 review: a set, so a long text's many words dedupe in O(n).
        let mut seen: HashSet<String> = words.iter().cloned().collect();
        let shell = Scanner::new(&chars, true);
        let argv = Scanner::new(&chars, false);
        for word in cut(&chars, Some(&shell))
            .into_iter()
            .chain(cut(&chars, Some(&argv)))
        {
            if seen.insert(word.clone()) {
                words.push(word);
            }
        }
    }
    words
}

/// One pass of [`path_words`]: `join` is `None` for the plain cut, else the
/// [`Scanner`] each `[` is read with.
fn cut(chars: &[char], join: Option<&Scanner>) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '['
            && let Some(scanner) = join
            && let Bracket::Class { end, .. } | Bracket::Opaque { end } = scanner.parse(i)
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
