//! The regular-expression subset the dashboard schemas' `pattern`s use.
//!
//! Why: the crate takes no `regex` dependency (#9171 A1, Architect ruling Q2
//! for #10063), and the schema files use a small, fixed subset. A pattern
//! outside it must fail when the schema loads, so a schema edit cannot
//! silently weaken a check.
//! What: [`Pattern::compile`] parses literals, `.`, `[...]` / `[^...]` with
//! ranges, `\` before punctuation, `^`, `$`, groups `(...)` / `(?:...)` with
//! `|`, and the quantifiers `?`, `*`, `+`, `{n}`, `{n,}`, `{n,m}`; anything
//! else (`\b`, look-around, backreferences, lazy quantifiers) is refused.
//! #12029 P2g: the frozen Architect schemas use `\s`, `\S` and `\n`, so
//! `\d`, `\D`, `\s`, `\S`, `\w`, `\W` (also inside a class) and the
//! `\n`, `\t`, `\r`, `\f`, `\v` escapes are taken too, with Python's
//! Unicode classes.
//! The parse is compiled to a Thompson NFA; a repeat count above
//! [`MAX_COUNT`] or a program past [`MAX_PROGRAM`] steps is refused.
//! [`Pattern::search`] answers Python's `re.search` over code points with a
//! Pike VM (two thread lists, no recursion), so stack use does not depend on
//! the input and time is O(input × program) (#10063 df-G01): `.` skips `\n`,
//! `$` also matches before one final `\n`.
//! [`Pattern::compile_python`] (#12028) also takes `\b` and `\B`, for
//! patterns a manifest supplies at run time.
//! Test: `pattern_matches_like_python_re_search`, `pattern_refuses_unsupported_syntax`,
//! `shipped_patterns_match_a_megabyte_on_a_small_stack`,
//! `python_shorthands_match_like_re_search`.

use crate::pystr::{is_digit, is_space, is_word};

/// The largest `{n}` / `{n,m}` count a pattern may use.
pub const MAX_COUNT: usize = 1000;
/// The most compile steps (and so instructions) one pattern may expand to.
pub const MAX_PROGRAM: usize = 10_000;

/// A compiled pattern.
#[derive(Debug, Clone)]
pub struct Pattern {
    prog: Vec<Inst>,
}

/// One NFA instruction; a consuming one advances to the next pc.
#[derive(Debug, Clone)]
enum Inst {
    Char(char),
    Any,
    Class {
        negate: bool,
        ranges: Vec<(char, char)>,
        shorts: Vec<(Short, bool)>,
    },
    Start,
    End,
    // See #12028: `\d`/`\s`/`\w` (negated: `\D`/`\S`/`\W`) and `\b`/`\B`.
    Short(Short, bool),
    Boundary(bool),
    Split(usize, usize),
    Jmp(usize),
    Match,
}

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class {
        negate: bool,
        ranges: Vec<(char, char)>,
        shorts: Vec<(Short, bool)>,
    },
    Start,
    End,
    Short(Short, bool),
    Boundary(bool),
    Group(Vec<Vec<Node>>),
    Repeat {
        node: Box<Node>,
        min: usize,
        max: Option<usize>,
    },
}

/// A Python shorthand class.
#[derive(Debug, Clone, Copy)]
enum Short {
    Digit,
    Space,
    Word,
}

impl Short {
    fn test(self, c: char) -> bool {
        match self {
            Self::Digit => is_digit(c),
            Self::Space => is_space(c),
            Self::Word => is_word(c),
        }
    }
}

/// `\d \D \s \S \w \W`: the class and whether it is negated.
fn short_of(c: char) -> Option<(Short, bool)> {
    let class = match c.to_ascii_lowercase() {
        'd' => Short::Digit,
        's' => Short::Space,
        'w' => Short::Word,
        _ => return None,
    };
    Some((class, c.is_ascii_uppercase()))
}

struct Parser<'a> {
    chars: Vec<char>,
    pos: usize,
    source: &'a str,
    python: bool,
}

impl Pattern {
    /// Parses `source`.
    ///
    /// # Errors
    /// A message naming the unsupported or malformed part.
    pub fn compile(source: &str) -> Result<Self, String> {
        Self::compile_with(source, false)
    }

    /// Parses `source` with the Python shorthands as well (see the module doc).
    ///
    /// # Errors
    /// A message naming the unsupported or malformed part.
    pub fn compile_python(source: &str) -> Result<Self, String> {
        Self::compile_with(source, true)
    }

    fn compile_with(source: &str, python: bool) -> Result<Self, String> {
        let mut p = Parser {
            chars: source.chars().collect(),
            pos: 0,
            source,
            python,
        };
        let alts = p.alternation()?;
        if p.pos != p.chars.len() {
            return Err(p.fail("unbalanced ')'"));
        }
        let mut emit = Emitter {
            prog: Vec::new(),
            budget: MAX_PROGRAM,
        };
        emit.alts(&alts)
            .and_then(|()| emit.push(Inst::Match))
            .map_err(|()| format!("pattern {source:?}: expands past {MAX_PROGRAM} steps"))?;
        Ok(Self { prog: emit.prog })
    }

    /// Whether the pattern matches anywhere in `text` (`re.search`).
    pub fn search(&self, text: &str) -> bool {
        let s: Vec<char> = text.chars().collect();
        let mut cur = Threads::new(self.prog.len());
        let mut next = Threads::new(self.prog.len());
        let mut stack = Vec::new();
        for i in 0..=s.len() {
            // A thread starting at `i` joins those carried from `i - 1`.
            if self.closure(0, i, &s, &mut cur, &mut stack) {
                return true;
            }
            let Some(&ch) = s.get(i) else { break };
            for idx in 0..cur.dense.len() {
                let pc = cur.dense[idx];
                if self.accepts(pc, ch) && self.closure(pc + 1, i + 1, &s, &mut next, &mut stack) {
                    return true;
                }
            }
            cur.clear();
            std::mem::swap(&mut cur, &mut next);
        }
        false
    }

    /// Adds `pc` and every pc reachable from it without consuming at
    /// position `i` to `list`; true when `Match` is reached.
    fn closure(
        &self,
        pc: usize,
        i: usize,
        s: &[char],
        list: &mut Threads,
        stack: &mut Vec<usize>,
    ) -> bool {
        stack.clear();
        stack.push(pc);
        while let Some(pc) = stack.pop() {
            if !list.insert(pc) {
                continue;
            }
            match self.prog[pc] {
                Inst::Match => return true,
                Inst::Jmp(x) => stack.push(x),
                Inst::Split(x, y) => stack.extend([y, x]),
                Inst::Start if i == 0 => stack.push(pc + 1),
                Inst::End if i == s.len() || (i + 1 == s.len() && s[i] == '\n') => {
                    stack.push(pc + 1);
                }
                Inst::Boundary(want) => {
                    let before = i > 0 && is_word(s[i - 1]);
                    let after = s.get(i).is_some_and(|c| is_word(*c));
                    if (before != after) == want {
                        stack.push(pc + 1);
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Whether the instruction at `pc` consumes `ch`.
    fn accepts(&self, pc: usize, ch: char) -> bool {
        match &self.prog[pc] {
            Inst::Char(c) => *c == ch,
            Inst::Any => ch != '\n',
            Inst::Short(class, negate) => class.test(ch) != *negate,
            Inst::Class {
                negate,
                ranges,
                shorts,
            } => {
                let member = ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&ch))
                    || shorts.iter().any(|(c, n)| c.test(ch) != *n);
                member != *negate
            }
            _ => false,
        }
    }
}

/// A set of pcs in insertion order, cleared in O(len).
struct Threads {
    dense: Vec<usize>,
    seen: Vec<bool>,
}

impl Threads {
    fn new(len: usize) -> Self {
        Self {
            dense: Vec::with_capacity(len),
            seen: vec![false; len],
        }
    }

    fn insert(&mut self, pc: usize) -> bool {
        let fresh = !self.seen[pc];
        if fresh {
            self.seen[pc] = true;
            self.dense.push(pc);
        }
        fresh
    }

    fn clear(&mut self) {
        for pc in self.dense.drain(..) {
            self.seen[pc] = false;
        }
    }
}

/// Lowers the parse to [`Inst`]s; every node and instruction spends one
/// unit of `budget`, so no pattern compiles in more than [`MAX_PROGRAM`] steps.
struct Emitter {
    prog: Vec<Inst>,
    budget: usize,
}

impl Emitter {
    fn spend(&mut self) -> Result<(), ()> {
        self.budget = self.budget.checked_sub(1).ok_or(())?;
        Ok(())
    }

    fn push(&mut self, inst: Inst) -> Result<usize, ()> {
        self.spend()?;
        self.prog.push(inst);
        Ok(self.prog.len() - 1)
    }

    /// `a|b`: `Split(a, next)`, `a`, `Jmp(end)`, `next: b`.
    fn alts(&mut self, alts: &[Vec<Node>]) -> Result<(), ()> {
        let mut jumps = Vec::new();
        for (n, seq) in alts.iter().enumerate() {
            let split = (n + 1 < alts.len())
                .then(|| self.push(Inst::Split(0, 0)))
                .transpose()?;
            for node in seq {
                self.node(node)?;
            }
            if let Some(split) = split {
                jumps.push(self.push(Inst::Jmp(0))?);
                self.prog[split] = Inst::Split(split + 1, self.prog.len());
            }
        }
        let end = self.prog.len();
        for j in jumps {
            self.prog[j] = Inst::Jmp(end);
        }
        Ok(())
    }

    fn node(&mut self, node: &Node) -> Result<(), ()> {
        self.spend()?;
        match node {
            Node::Char(c) => self.push(Inst::Char(*c)).map(drop),
            Node::Any => self.push(Inst::Any).map(drop),
            Node::Class {
                negate,
                ranges,
                shorts,
            } => self
                .push(Inst::Class {
                    negate: *negate,
                    ranges: ranges.clone(),
                    shorts: shorts.clone(),
                })
                .map(drop),
            Node::Start => self.push(Inst::Start).map(drop),
            Node::End => self.push(Inst::End).map(drop),
            Node::Short(c, n) => self.push(Inst::Short(*c, *n)).map(drop),
            Node::Boundary(b) => self.push(Inst::Boundary(*b)).map(drop),
            Node::Group(inner) => self.alts(inner),
            Node::Repeat { node, min, max } => {
                for _ in 0..*min {
                    self.node(node)?;
                }
                let mut splits = Vec::new();
                match max {
                    // `L: Split(body, end)`, body, `Jmp(L)`.
                    None => {
                        let split = self.push(Inst::Split(0, 0))?;
                        self.node(node)?;
                        self.push(Inst::Jmp(split))?;
                        splits.push(split);
                    }
                    // `max - min` nested optional copies, each skipping to the end.
                    Some(max) => {
                        for _ in *min..*max {
                            splits.push(self.push(Inst::Split(0, 0))?);
                            self.node(node)?;
                        }
                    }
                }
                let end = self.prog.len();
                for s in splits {
                    self.prog[s] = Inst::Split(s + 1, end);
                }
                Ok(())
            }
        }
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
            '\\' => match self.peek() {
                Some(c @ ('b' | 'B')) if self.python => {
                    self.pos += 1;
                    Node::Boundary(c == 'b')
                }
                Some(c) if short_of(c).is_some() => {
                    self.pos += 1;
                    let (class, negate) = short_of(c).unwrap_or((Short::Word, false));
                    Node::Short(class, negate)
                }
                _ => Node::Char(self.escaped()?),
            },
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

    /// The character after `\`: escaped punctuation, or a control escape
    /// (`\n`, `\t`, `\r`, `\f`, `\v`; #12029 P2g).
    fn escaped(&mut self) -> Result<char, String> {
        let c = self.peek().ok_or_else(|| self.fail("trailing backslash"))?;
        let control = match c {
            'n' => Some('\n'),
            't' => Some('\t'),
            'r' => Some('\r'),
            'f' => Some('\u{c}'),
            'v' => Some('\u{b}'),
            _ => None,
        };
        if let Some(ch) = control {
            self.pos += 1;
            return Ok(ch);
        }
        if !c.is_ascii_punctuation() {
            // #10697: name the escape found; "like \d" sent a `\s` report after `\d`.
            return Err(self.fail(&format!("escape class \\{c} is not supported")));
        }
        self.pos += 1;
        Ok(c)
    }

    fn class(&mut self) -> Result<Node, String> {
        let negate = self.peek() == Some('^');
        if negate {
            self.pos += 1;
        }
        let (mut ranges, mut shorts) = (Vec::new(), Vec::new());
        let mut first = true;
        loop {
            let c = self.peek().ok_or_else(|| self.fail("missing ']'"))?;
            self.pos += 1;
            if c == ']' && !first {
                break;
            }
            first = false;
            // #12029 P2g: `[^/\s]` in the frozen items schema.
            if c == '\\'
                && let Some(short) = self.peek().and_then(short_of)
            {
                self.pos += 1;
                if self.peek() == Some('-')
                    && self.chars.get(self.pos + 1).is_some_and(|n| *n != ']')
                {
                    return Err(self.fail("bad range"));
                }
                shorts.push(short);
                continue;
            }
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
        Ok(Node::Class {
            negate,
            ranges,
            shorts,
        })
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
        if min.max(max.unwrap_or(0)) > MAX_COUNT {
            return Err(self.fail(&format!("repeat count above {MAX_COUNT}")));
        }
        Ok(Node::Repeat {
            node: Box::new(node),
            min,
            max,
        })
    }
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
            "\\b",
            "\\q",
            "(?=x)",
            "a*?",
            "a{2",
            "(a",
            "a)",
            "[a",
            "*a",
            "[z-a]",
            "\\1",
            "a{1001}",
            "a{2,1001}",
            "(a{1000}){1000}",
            "((){1000}){1000}",
        ] {
            assert!(Pattern::compile(bad).is_err(), "{bad}");
        }
        assert!(Pattern::compile("a{1000}").is_ok());
    }

    #[test]
    fn python_shorthands_match_like_re_search() -> Result<(), String> {
        let word = Pattern::compile_python("\\bAcme\\b")?;
        assert!(word.search("an Acme host") && word.search("Acme"));
        assert!(!word.search("Acmes") && !word.search("_Acme"));
        assert!(Pattern::compile_python("build-\\d+")?.search("build-\u{663}"));
        assert!(Pattern::compile_python("a\\s+b")?.search("a \u{a0}b"));
        assert!(Pattern::compile_python("^\\w\\W\\S\\D$")?.search("x-yz"));
        assert!(
            Pattern::compile_python("a\\Bb")?.search("ab")
                && !Pattern::compile_python("a\\B")?.search("a")
        );
        assert!(Pattern::compile_python("ws-[a-z-]+-0[0-9]")?.search("ws-main-branch-07"));
        assert!(Pattern::compile("\\b").is_err());
        // #12029 P2g: a shorthand inside a class, and the control escapes.
        let class = Pattern::compile_python("^[\\d_]+$")?;
        assert!(class.search("1_\u{663}") && !class.search("1a"));
        assert!(
            Pattern::compile("^[^\\n]{1,3}$")?.search("abc")
                && !Pattern::compile("^[^\\n]{1,3}$")?.search("a\nb")
        );
        assert!(
            Pattern::compile("^\\d+$")?.search("\u{663}7") && Pattern::compile("[\\s-x]").is_err()
        );
        Ok(())
    }
}
