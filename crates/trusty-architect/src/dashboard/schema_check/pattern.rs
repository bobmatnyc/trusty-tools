//! The regular-expression subset the dashboard schemas' `pattern`s use.
//!
//! Why: the crate takes no `regex` dependency (#9171 A1, Architect ruling Q2
//! for #10063), and the schema files use a small, fixed subset. A pattern
//! outside it must fail when the schema loads, so a schema edit cannot
//! silently weaken a check.
//! What: [`Pattern::compile`] parses literals, `.`, `[...]` / `[^...]` with
//! ranges, `\` before punctuation, `^`, `$`, groups `(...)` / `(?:...)` with
//! `|`, and the quantifiers `?`, `*`, `+`, `{n}`, `{n,}`, `{n,m}`; anything
//! else (`\d`, look-around, backreferences, lazy quantifiers) is refused.
//! [`Pattern::search`] is Python's `re.search` over code points with a
//! backtracking matcher: `.` skips `\n`, `$` also matches before one final
//! `\n`.
//! Test: `pattern_matches_like_python_re_search`, `pattern_refuses_unsupported_syntax`.

/// A compiled pattern.
#[derive(Debug, Clone)]
pub struct Pattern {
    alts: Vec<Vec<Node>>,
}

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class {
        negate: bool,
        ranges: Vec<(char, char)>,
    },
    Start,
    End,
    Group(Vec<Vec<Node>>),
    Repeat {
        node: Box<Node>,
        min: usize,
        max: Option<usize>,
    },
}

struct Parser<'a> {
    chars: Vec<char>,
    pos: usize,
    source: &'a str,
}

impl Pattern {
    /// Parses `source`.
    ///
    /// # Errors
    /// A message naming the unsupported or malformed part.
    pub fn compile(source: &str) -> Result<Self, String> {
        let mut p = Parser {
            chars: source.chars().collect(),
            pos: 0,
            source,
        };
        let alts = p.alternation()?;
        if p.pos != p.chars.len() {
            return Err(p.fail("unbalanced ')'"));
        }
        Ok(Self { alts })
    }

    /// Whether the pattern matches anywhere in `text` (`re.search`).
    pub fn search(&self, text: &str) -> bool {
        let s: Vec<char> = text.chars().collect();
        (0..=s.len()).any(|start| alts(&self.alts, start, &s, &mut |_| true))
    }
}

impl Parser<'_> {
    fn fail(&self, why: &str) -> String {
        format!("pattern {:?}: {why} at {}", self.source, self.pos)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn alternation(&mut self) -> Result<Vec<Vec<Node>>, String> {
        let mut out = vec![self.sequence()?];
        while self.peek() == Some('|') {
            self.pos += 1;
            out.push(self.sequence()?);
        }
        Ok(out)
    }

    fn sequence(&mut self) -> Result<Vec<Node>, String> {
        let mut seq = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            seq.push(self.quantified(atom)?);
        }
        Ok(seq)
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.peek().ok_or_else(|| self.fail("unexpected end"))?;
        self.pos += 1;
        Ok(match c {
            '^' => Node::Start,
            '$' => Node::End,
            '.' => Node::Any,
            '[' => self.class()?,
            '\\' => Node::Char(self.escaped()?),
            '(' => {
                if self.peek() == Some('?') {
                    if self.chars.get(self.pos + 1) != Some(&':') {
                        return Err(self.fail("only (?:...) groups are supported"));
                    }
                    self.pos += 2;
                }
                let inner = self.alternation()?;
                if self.peek() != Some(')') {
                    return Err(self.fail("missing ')'"));
                }
                self.pos += 1;
                Node::Group(inner)
            }
            '*' | '+' | '?' | '{' => return Err(self.fail("nothing to repeat")),
            c => Node::Char(c),
        })
    }

    /// The character after `\`: only escaped punctuation is supported.
    fn escaped(&mut self) -> Result<char, String> {
        let c = self.peek().ok_or_else(|| self.fail("trailing backslash"))?;
        if !c.is_ascii_punctuation() {
            return Err(self.fail("escape classes like \\d are not supported"));
        }
        self.pos += 1;
        Ok(c)
    }

    fn class(&mut self) -> Result<Node, String> {
        let negate = self.peek() == Some('^');
        if negate {
            self.pos += 1;
        }
        let mut ranges = Vec::new();
        let mut first = true;
        loop {
            let c = self.peek().ok_or_else(|| self.fail("missing ']'"))?;
            self.pos += 1;
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo = if c == '\\' { self.escaped()? } else { c };
            let is_range =
                self.peek() == Some('-') && self.chars.get(self.pos + 1).is_some_and(|n| *n != ']');
            if is_range {
                self.pos += 1;
                let hi = self.peek().ok_or_else(|| self.fail("missing ']'"))?;
                self.pos += 1;
                let hi = if hi == '\\' { self.escaped()? } else { hi };
                if hi < lo {
                    return Err(self.fail("bad range"));
                }
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
        Ok(Node::Class { negate, ranges })
    }

    fn quantified(&mut self, node: Node) -> Result<Node, String> {
        let (min, max) = match self.peek() {
            Some('?') => (0, Some(1)),
            Some('*') => (0, None),
            Some('+') => (1, None),
            Some('{') => return self.braces(node),
            _ => return Ok(node),
        };
        self.pos += 1;
        self.repeat(node, min, max)
    }

    fn braces(&mut self, node: Node) -> Result<Node, String> {
        let close = self.chars[self.pos..]
            .iter()
            .position(|c| *c == '}')
            .ok_or_else(|| self.fail("missing '}'"))?;
        let body: String = self.chars[self.pos + 1..self.pos + close].iter().collect();
        let num = |s: &str| s.parse::<usize>().map_err(|_| self.fail("bad {n,m}"));
        let (min, max) = match body.split_once(',') {
            None => (num(&body)?, Some(num(&body)?)),
            Some((lo, "")) => (num(lo)?, None),
            Some((lo, hi)) => (num(lo)?, Some(num(hi)?)),
        };
        self.pos += close + 1;
        self.repeat(node, min, max)
    }

    fn repeat(&mut self, node: Node, min: usize, max: Option<usize>) -> Result<Node, String> {
        if matches!(self.peek(), Some('?' | '+' | '*' | '{')) {
            return Err(self.fail("lazy, possessive or stacked quantifiers are not supported"));
        }
        if max.is_some_and(|m| m < min) {
            return Err(self.fail("bad {n,m}"));
        }
        Ok(Node::Repeat {
            node: Box::new(node),
            min,
            max,
        })
    }
}

type Cont<'a> = &'a mut dyn FnMut(usize) -> bool;

fn alts(alts: &[Vec<Node>], i: usize, s: &[char], k: Cont<'_>) -> bool {
    alts.iter().any(|seq| sequence(seq, i, s, k))
}

fn sequence(seq: &[Node], i: usize, s: &[char], k: Cont<'_>) -> bool {
    match seq.split_first() {
        None => k(i),
        Some((node, rest)) => one(node, i, s, &mut |j| sequence(rest, j, s, k)),
    }
}

fn one(node: &Node, i: usize, s: &[char], k: Cont<'_>) -> bool {
    match node {
        Node::Char(c) => s.get(i) == Some(c) && k(i + 1),
        Node::Any => s.get(i).is_some_and(|c| *c != '\n') && k(i + 1),
        Node::Class { negate, ranges } => {
            let hit = |c: &char| ranges.iter().any(|(lo, hi)| (lo..=hi).contains(&c));
            s.get(i).is_some_and(|c| hit(c) != *negate) && k(i + 1)
        }
        Node::Start => i == 0 && k(i),
        Node::End => (i == s.len() || (i + 1 == s.len() && s[i] == '\n')) && k(i),
        Node::Group(inner) => alts(inner, i, s, k),
        Node::Repeat { node, min, max } => repeat(node, *min, *max, 0, i, s, k),
    }
}

/// Greedy repetition with backtracking; an empty iteration past `min` stops.
fn repeat(
    node: &Node,
    min: usize,
    max: Option<usize>,
    count: usize,
    i: usize,
    s: &[char],
    k: Cont<'_>,
) -> bool {
    if max.is_none_or(|m| count < m) {
        let more = one(node, i, s, &mut |j| {
            (j != i || count < min) && repeat(node, min, max, count + 1, j, s, k)
        });
        if more {
            return true;
        }
    }
    count >= min && k(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_matches_like_python_re_search() -> Result<(), String> {
        let ts = Pattern::compile(
            "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}([.][0-9]{3})?Z$",
        )?;
        assert!(ts.search("2026-10-12T03:46:00Z"));
        assert!(ts.search("2026-10-12T03:46:00.123Z"));
        assert!(ts.search("2026-10-12T03:46:00Z\n"));
        assert!(!ts.search("2026-10-12T03:46Z"));
        assert!(!ts.search("x2026-10-12T03:46:00Z"));
        let date = Pattern::compile(
            "^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])(T([01][0-9]|2[0-3]):[0-5][0-9](:[0-5][0-9])?Z)?$",
        )?;
        assert!(date.search("2026-10-31") && date.search("2026-10-31T23:59Z"));
        assert!(!date.search("2026-13-01") && !date.search("2026-10-31T24:00Z"));
        let repos = Pattern::compile("^[^/ ,]+/[^/ ,]+(, [^/ ,]+/[^/ ,]+)*$")?;
        assert!(repos.search("a/b") && repos.search("a/b, c/d"));
        assert!(!repos.search("a/b,c/d") && !repos.search("a b/c"));
        let brief = Pattern::compile("^[^/]+/docs/briefs/.+[.]md$")?;
        assert!(brief.search("p/docs/briefs/x.md") && !brief.search("p/docs/briefs/x.mdx"));
        assert!(Pattern::compile("^https?://")?.search("https://x"));
        assert!(Pattern::compile("b+")?.search("abbbc"));
        assert!(Pattern::compile("^(a|ab)c$")?.search("abc"));
        assert!(Pattern::compile("^(?:x*)*$")?.search("xxx"));
        assert!(!Pattern::compile("^a.c$")?.search("a\nc"));
        assert!(Pattern::compile("^G[0-9a-f]{2}$")?.search("G0a"));
        assert!(
            Pattern::compile("^a{2,}$")?.search("aaa")
                && !Pattern::compile("^a{2,3}$")?.search("aaaa")
        );
        Ok(())
    }

    #[test]
    fn pattern_refuses_unsupported_syntax() {
        for bad in [
            "\\d+", "(?=x)", "a*?", "a{2", "(a", "a)", "[a", "*a", "[z-a]", "\\1",
        ] {
            assert!(Pattern::compile(bad).is_err(), "{bad}");
        }
    }
}
