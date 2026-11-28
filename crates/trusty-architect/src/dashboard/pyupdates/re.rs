//! `dashboard_updates.py`'s three patterns, matched by hand with Python `re` semantics (#12029 P2h).
//!
//! Why: the crate takes no `regex` dependency, and the patterns use a
//! lookbehind, lookaheads, `\b`, lazy `.*?` and Unicode `\d` / `\s`, so a
//! tag must be found exactly where Python's backtracking engine finds it.
//! What: a small backtracking matcher over a node list ([`Node`]) that tries
//! alternatives in `re`'s order (greedy longest first, lazy shortest first,
//! an optional group present first), and [`TAG`], [`BARE`], [`HANDBACK`]
//! built from it. [`search`] is `pattern.search`, [`full_match`] is
//! `pattern.match`; both return the capture groups.
//! Test: `tag_bare_and_handback_follow_python_re`,
//! `unicode_digits_and_spaces_follow_python_re`.

use crate::pystr::{is_digit, is_space, is_word};

/// A character class the patterns use.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Cls {
    /// `\s` on `str`.
    Space,
    /// `\d` on `str` (every Unicode `Nd` digit).
    Digit,
    /// `[0-9a-km-np-z]`, the IL id digits.
    IdDigit,
    /// `[0-9a-zA-Z]`.
    Alnum,
    /// `[0-9a-zA-Z-]`.
    AlnumDash,
    /// `.`: anything but `\n`.
    Any,
}

impl Cls {
    fn has(self, c: char) -> bool {
        match self {
            Self::Space => is_space(c),
            Self::Digit => is_digit(c),
            Self::IdDigit => c.is_ascii_digit() || matches!(c, 'a'..='k' | 'm'..='n' | 'p'..='z'),
            Self::Alnum => c.is_ascii_alphanumeric(),
            Self::AlnumDash => c.is_ascii_alphanumeric() || c == '-',
            Self::Any => c != '\n',
        }
    }
}

/// One pattern element.
#[derive(Debug, Clone)]
pub(crate) enum Node {
    /// A literal string.
    Lit(&'static str),
    /// `cls{min,max}`, greedy (`max` `None` for unbounded).
    Rep(Cls, usize, Option<usize>),
    /// `cls*?`, lazy.
    Lazy(Cls),
    /// `(?:...)?`, greedy.
    Opt(Vec<Node>),
    /// A capture group with its index (0-based).
    Group(usize, Vec<Node>),
    /// `\b`.
    WordBoundary,
    /// `(?!cls)`.
    NotAhead(Cls),
    /// `(?<!cls)`.
    NotBehind(Cls),
    /// `$` without `re.M`: the end, or before a final `\n`.
    End,
}

/// The groups of one match: `(start, end)` in characters.
pub(crate) type Caps = Vec<Option<(usize, usize)>>;

type K<'k> = &'k mut dyn FnMut(usize, &mut Caps) -> bool;

fn m(nodes: &[Node], t: &[char], pos: usize, caps: &mut Caps, k: K<'_>) -> bool {
    let Some((first, rest)) = nodes.split_first() else {
        return k(pos, caps);
    };
    match first {
        Node::Lit(s) => {
            let mut p = pos;
            for c in s.chars() {
                if t.get(p) != Some(&c) {
                    return false;
                }
                p += 1;
            }
            m(rest, t, p, caps, k)
        }
        Node::Rep(cls, min, max) => {
            let mut n = 0;
            while max.is_none_or(|x| n < x) && t.get(pos + n).is_some_and(|c| cls.has(*c)) {
                n += 1;
            }
            (*min..=n).rev().any(|j| m(rest, t, pos + j, caps, k))
        }
        Node::Lazy(cls) => {
            let mut j = 0;
            loop {
                if m(rest, t, pos + j, caps, k) {
                    return true;
                }
                match t.get(pos + j) {
                    Some(c) if cls.has(*c) => j += 1,
                    _ => return false,
                }
            }
        }
        Node::Opt(inner) => {
            m(inner, t, pos, caps, &mut |p, c| m(rest, t, p, c, k)) || m(rest, t, pos, caps, k)
        }
        Node::Group(g, inner) => m(inner, t, pos, caps, &mut |p, c| {
            let old = c[*g];
            c[*g] = Some((pos, p));
            if m(rest, t, p, c, k) {
                return true;
            }
            c[*g] = old;
            false
        }),
        Node::WordBoundary => {
            let before = pos > 0 && is_word(t[pos - 1]);
            let after = t.get(pos).is_some_and(|c| is_word(*c));
            before != after && m(rest, t, pos, caps, k)
        }
        Node::NotAhead(cls) => !t.get(pos).is_some_and(|c| cls.has(*c)) && m(rest, t, pos, caps, k),
        Node::NotBehind(cls) => !(pos > 0 && cls.has(t[pos - 1])) && m(rest, t, pos, caps, k),
        Node::End => {
            let at_end = pos == t.len() || (pos + 1 == t.len() && t[pos] == '\n');
            at_end && m(rest, t, pos, caps, k)
        }
    }
}

/// A compiled pattern: its nodes and group count.
pub(crate) struct Pattern {
    nodes: Vec<Node>,
    groups: usize,
}

fn at(p: &Pattern, t: &[char], start: usize) -> Option<Vec<String>> {
    let mut caps: Caps = vec![None; p.groups];
    let mut done = false;
    m(&p.nodes, t, start, &mut caps, &mut |_, _| {
        done = true;
        true
    });
    done.then(|| {
        caps.iter()
            .map(|g| g.map_or_else(String::new, |(a, b)| t[a..b].iter().collect()))
            .collect()
    })
}

/// `pattern.search(text)`: the groups of the leftmost match.
pub(crate) fn search(p: &Pattern, text: &str) -> Option<Vec<String>> {
    let t: Vec<char> = text.chars().collect();
    (0..=t.len()).find_map(|i| at(p, &t, i))
}

/// `pattern.match(text)`: the groups of a match at the start.
pub(crate) fn full_match(p: &Pattern, text: &str) -> Option<Vec<String>> {
    let t: Vec<char> = text.chars().collect();
    at(p, &t, 0)
}

/// `(?:ar-)?T[0-9a-km-np-z]+`, as group `g`.
fn task_id(g: usize) -> Node {
    Node::Group(
        g,
        vec![
            Node::Opt(vec![Node::Lit("ar-")]),
            Node::Lit("T"),
            Node::Rep(Cls::IdDigit, 1, None),
        ],
    )
}

/// `(?:@\d{4})?`.
fn day() -> Node {
    Node::Opt(vec![Node::Lit("@"), Node::Rep(Cls::Digit, 4, Some(4))])
}

const SP: Node = Node::Rep(Cls::Space, 0, None);

/// `TAG`: `\bTask:\s*((?:ar-)?T[0-9a-km-np-z]+)(?:@\d{4})?(?![0-9a-zA-Z])`.
pub(crate) fn tag() -> Pattern {
    Pattern {
        nodes: vec![
            Node::WordBoundary,
            Node::Lit("Task:"),
            SP,
            task_id(0),
            day(),
            Node::NotAhead(Cls::Alnum),
        ],
        groups: 1,
    }
}

/// `BARE`: `(?<![0-9a-zA-Z-])ar-(T[0-9a-km-np-z]+)(?![0-9a-zA-Z])`.
pub(crate) fn bare() -> Pattern {
    Pattern {
        nodes: vec![
            Node::NotBehind(Cls::AlnumDash),
            Node::Lit("ar-"),
            Node::Group(0, vec![Node::Lit("T"), Node::Rep(Cls::IdDigit, 1, None)]),
            Node::NotAhead(Cls::Alnum),
        ],
        groups: 1,
    }
}

/// `HANDBACK`: `^Task:\s*((?:ar-)?T…+)(?:@\d{4})?\s*\|\s*done:\s*(.*?)\s*\|\s*est:.*?\|\s*remaining:.*?\|\s*pct:\s*(\d{1,3})\s*$`.
pub(crate) fn handback() -> Pattern {
    Pattern {
        nodes: vec![
            Node::Lit("Task:"),
            SP,
            task_id(0),
            day(),
            SP,
            Node::Lit("|"),
            SP,
            Node::Lit("done:"),
            SP,
            Node::Group(1, vec![Node::Lazy(Cls::Any)]),
            SP,
            Node::Lit("|"),
            SP,
            Node::Lit("est:"),
            Node::Lazy(Cls::Any),
            Node::Lit("|"),
            SP,
            Node::Lit("remaining:"),
            Node::Lazy(Cls::Any),
            Node::Lit("|"),
            SP,
            Node::Lit("pct:"),
            SP,
            Node::Group(2, vec![Node::Rep(Cls::Digit, 1, Some(3))]),
            SP,
            Node::End,
        ],
        groups: 3,
    }
}
