//! `level_lint.py`'s seven patterns, matched by hand as Python `re` does.
//!
//! Why: the crate takes no `regex` dependency (#9171 A1), and the warnings
//! must name the spans Python's `finditer` finds (#12028 AC1).
//! What: [`RULES`] lists the rules in the script's order; [`find_iter`] is
//! `rx.finditer(text)` for one rule: at each position the matcher returns the
//! end of the match Python's backtracking engine picks first (greedy, then
//! alternation order), and the scan resumes at that end. [`mask_urls`] is the
//! script's `_URL.sub` that blanks `https?://\S+` with spaces. `\w`, `\d` and
//! `\s` are Python's Unicode classes from [`crate::pystr`].
//! Test: `level_lint_recordings_replay_clean_against_the_port` (the
//! `rules-corpus` recording exercises each rule's edges).

use crate::pystr::{is_digit, is_space, is_word};

/// The rule names in the script's `_RULES` order (an earlier one wins a span).
pub(super) const RULES: [&str; 7] = ["path", "sha", "flag", "envvar", "job", "tool", "identifier"];

const EXTS: [&str; 20] = [
    "py", "rs", "md", "json", "jsonl", "toml", "yaml", "yml", "sh", "js", "ts", "tsx", "html",
    "css", "txt", "lock", "cfg", "ini", "sql", "log",
];

const TOOLS: [&str; 11] = [
    "cargo",
    "pytest",
    "tmux",
    "clippy",
    "rustc",
    "npm",
    "pnpm",
    "launchctl",
    "ripgrep",
    "git",
    "gh",
];

fn at(t: &[char], i: usize) -> Option<char> {
    t.get(i).copied()
}

fn word_at(t: &[char], i: usize) -> bool {
    at(t, i).is_some_and(is_word)
}

fn word_or_dash_at(t: &[char], i: usize) -> bool {
    at(t, i).is_some_and(|c| is_word(c) || c == '-')
}

/// The character before `i` is `\w` (`(?<![\w])` fails).
fn word_before(t: &[char], i: usize) -> bool {
    i > 0 && is_word(t[i - 1])
}

fn word_or_dash_before(t: &[char], i: usize) -> bool {
    i > 0 && (is_word(t[i - 1]) || t[i - 1] == '-')
}

fn run(t: &[char], i: usize, f: impl Fn(char) -> bool) -> usize {
    t[i.min(t.len())..].iter().take_while(|c| f(**c)).count()
}

fn literal(t: &[char], i: usize, s: &str) -> Option<usize> {
    let mut j = i;
    for c in s.chars() {
        if at(t, j) != Some(c) {
            return None;
        }
        j += 1;
    }
    Some(j)
}

/// `rx.finditer` spans for rule `rule` over `t`.
pub(super) fn find_iter(rule: &str, t: &[char]) -> Vec<(usize, usize)> {
    let matcher: fn(&[char], usize) -> Option<usize> = match rule {
        "path" => path_at,
        "sha" => sha_at,
        "flag" => flag_at,
        "envvar" => envvar_at,
        "job" => job_at,
        "tool" => tool_at,
        _ => identifier_at,
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        match matcher(t, i) {
            Some(end) => {
                out.push((i, end));
                i = end.max(i + 1);
            }
            None => i += 1,
        }
    }
    out
}

/// `_URL.sub(lambda m: " " * len(m.group(0)), text)`.
pub(super) fn mask_urls(t: &mut [char]) {
    let mut i = 0;
    while i < t.len() {
        let after = literal(t, i, "https://").or_else(|| literal(t, i, "http://"));
        let n = after.map_or(0, |j| run(t, j, |c| !is_space(c)));
        match after {
            Some(j) if n > 0 => {
                t[i..j + n].iter_mut().for_each(|c| *c = ' ');
                i = j + n;
            }
            _ => i += 1,
        }
    }
}

/// `(?::\d+(?:-\d+)?)?(?![\w])` after a path's extension ends at `e`.
fn path_tail(t: &[char], e: usize) -> Option<usize> {
    if at(t, e) == Some(':') {
        let d = run(t, e + 1, is_digit);
        for dl in (1..=d).rev() {
            let e2 = e + 1 + dl;
            if at(t, e2) == Some('-') {
                let m = run(t, e2 + 1, is_digit);
                for ml in (1..=m).rev() {
                    if !word_at(t, e2 + 1 + ml) {
                        return Some(e2 + 1 + ml);
                    }
                }
            }
            if !word_at(t, e2) {
                return Some(e2);
            }
        }
    }
    (!word_at(t, e)).then_some(e)
}

/// `(?<![\w])(?:[\w.~-]+/)*[\w.-]+\.EXT(?::\d+(?:-\d+)?)?(?![\w])`.
fn path_at(t: &[char], i: usize) -> Option<usize> {
    if word_before(t, i) {
        return None;
    }
    let dir = |c: char| is_word(c) || matches!(c, '.' | '~' | '-');
    let file = |c: char| is_word(c) || matches!(c, '.' | '-');
    let mut starts = vec![i];
    let mut cur = i;
    loop {
        let n = run(t, cur, dir);
        if n == 0 || at(t, cur + n) != Some('/') {
            break;
        }
        cur += n + 1;
        starts.push(cur);
    }
    for &p in starts.iter().rev() {
        let f = run(t, p, file);
        for len in (1..=f).rev() {
            let dot = p + len;
            if at(t, dot) != Some('.') {
                continue;
            }
            for ext in EXTS {
                if let Some(e) = literal(t, dot + 1, ext)
                    && let Some(end) = path_tail(t, e)
                {
                    return Some(end);
                }
            }
        }
    }
    None
}

fn hex(c: char) -> bool {
    c.is_ascii_digit() || ('a'..='f').contains(&c)
}

/// `(?<![\w-])(?=[0-9a-f]*\d)(?=[0-9a-f]*[a-f])[0-9a-f]{7,40}(?![\w-])`.
fn sha_at(t: &[char], i: usize) -> Option<usize> {
    if word_or_dash_before(t, i) {
        return None;
    }
    let n = run(t, i, hex);
    let digit = (0..=n).any(|k| at(t, i + k).is_some_and(is_digit));
    let letter = (0..n).any(|k| ('a'..='f').contains(&t[i + k]));
    if !(digit && letter) {
        return None;
    }
    (7..=n.min(40))
        .rev()
        .map(|len| i + len)
        .find(|e| !word_or_dash_at(t, *e))
}

/// `(?<![\w-])--[a-z][\w-]*`.
fn flag_at(t: &[char], i: usize) -> Option<usize> {
    if word_or_dash_before(t, i) || literal(t, i, "--").is_none() {
        return None;
    }
    at(t, i + 2)
        .filter(char::is_ascii_lowercase)
        .map(|_| i + 3 + run(t, i + 3, |c| is_word(c) || c == '-'))
}

/// `(?<![\w])[A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+(?![\w])`.
fn envvar_at(t: &[char], i: usize) -> Option<usize> {
    let upper = |c: char| c.is_ascii_uppercase() || c.is_ascii_digit();
    if word_before(t, i) || !at(t, i).is_some_and(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let mut e = i + run(t, i, upper);
    let mut segments = 0;
    while at(t, e) == Some('_') && run(t, e + 1, upper) > 0 {
        e += 1 + run(t, e + 1, upper);
        segments += 1;
    }
    (segments > 0 && !word_at(t, e)).then_some(e)
}

/// One `re.IGNORECASE` comparison of a text character with an ASCII
/// lowercase pattern letter, Unicode folds included (`K` U+212A is `k`,
/// `ſ` U+017F is `s`).
fn fold_eq(c: char, p: char) -> bool {
    c.to_lowercase().eq(std::iter::once(p))
        || (p == 'k' && c == '\u{212a}')
        || (p == 's' && c == '\u{17f}')
}

fn literal_ci(t: &[char], i: usize, s: &str) -> Option<usize> {
    let mut j = i;
    for p in s.chars() {
        if !at(t, j).is_some_and(|c| fold_eq(c, p)) {
            return None;
        }
        j += 1;
    }
    Some(j)
}

/// `\b(?:job|workflow)s?\s+[`'"][^`'"\n]+[`'"]` with `re.I`.
fn job_at(t: &[char], i: usize) -> Option<usize> {
    if word_before(t, i) || !word_at(t, i) {
        return None;
    }
    let quote = |c: char| matches!(c, '`' | '\'' | '"');
    let word_end = literal_ci(t, i, "job").or_else(|| literal_ci(t, i, "workflow"))?;
    let mut tries = Vec::new();
    if let Some(e) = literal_ci(t, word_end, "s") {
        tries.push(e);
    }
    tries.push(word_end);
    for e in tries {
        let sp = run(t, e, is_space);
        if sp == 0 || !at(t, e + sp).is_some_and(quote) {
            continue;
        }
        let body = e + sp + 1;
        let n = run(t, body, |c| !quote(c) && c != '\n');
        if n > 0 && at(t, body + n).is_some_and(quote) {
            return Some(body + n + 1);
        }
    }
    None
}

/// `(?<![\w-])(?:cargo|pytest|...|gh)(?![\w-])`.
fn tool_at(t: &[char], i: usize) -> Option<usize> {
    if word_or_dash_before(t, i) {
        return None;
    }
    TOOLS
        .iter()
        .filter_map(|w| literal(t, i, w))
        .find(|e| !word_or_dash_at(t, *e))
}

/// `` `[^`\n]+` ``.
fn identifier_at(t: &[char], i: usize) -> Option<usize> {
    if at(t, i) != Some('`') {
        return None;
    }
    let n = run(t, i + 1, |c| c != '`' && c != '\n');
    (n > 0 && at(t, i + 1 + n) == Some('`')).then_some(i + n + 2)
}
