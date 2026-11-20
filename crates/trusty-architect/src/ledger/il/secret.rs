//! ilio's own `SECRET` pattern, ported without a regex engine (G0b).
//!
//! Why: the injected scan (`scan_for_credentials`) and ilio's `SECRET`
//! refuse different shapes; a block the Rust path writes must be refused by
//! both, or the Rust path writes what `ilio.py apply` would have refused.
//! What: [`ilio_secret`] matches, case-insensitively, `ilio.py`'s
//! `ghp_[A-Za-z0-9]{20,}`, `sk-[A-Za-z0-9_-]{20,}`, `AKIA[0-9A-Z]{16}`,
//! `xox[abp]-[A-Za-z0-9-]{10,}`, `-----BEGIN [A-Z ]*PRIVATE KEY` and
//! `(?:password|secret|token|api[_-]?key)\s*[:=]\s*\S{8,}`.
//! Test: `il_secret_rules_match_ilio_on_the_rust_path`.

/// The length of the run of chars from `i` that satisfy `f`.
fn run(s: &[char], i: usize, f: impl Fn(char) -> bool) -> usize {
    s.get(i..)
        .map_or(0, |r| r.iter().take_while(|c| f(**c)).count())
}

fn starts(s: &[char], i: usize, lit: &str) -> Option<usize> {
    let n = lit.chars().count();
    let ok = s
        .get(i..i + n)
        .is_some_and(|w| w.iter().copied().eq(lit.chars()));
    ok.then_some(i + n)
}

fn alnum(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}

/// The `password: <8+ chars>` alternative, from just past the keyword.
fn assignment(s: &[char], i: usize) -> bool {
    let i = i + run(s, i, char::is_whitespace);
    if !matches!(s.get(i), Some(':' | '=')) {
        return false;
    }
    let i = i + 1 + run(s, i + 1, char::is_whitespace);
    run(s, i, |c| !c.is_whitespace()) >= 8
}

fn at(s: &[char], i: usize) -> bool {
    let tok =
        |lit, min, f: fn(char) -> bool| starts(s, i, lit).is_some_and(|j| run(s, j, f) >= min);
    if tok("ghp_", 20, alnum)
        || tok("sk-", 20, |c| alnum(c) || matches!(c, '_' | '-'))
        || tok("akia", 16, alnum)
    {
        return true;
    }
    let xox = starts(s, i, "xox")
        .filter(|j| matches!(s.get(*j), Some('a' | 'b' | 'p')))
        .and_then(|j| starts(s, j + 1, "-"));
    if xox.is_some_and(|j| run(s, j, |c| alnum(c) || c == '-') >= 10) {
        return true;
    }
    if let Some(j) = starts(s, i, "-----begin ") {
        let n = run(s, j, |c| c.is_ascii_lowercase() || c == ' ');
        let seg: String = s[j..j + n].iter().collect();
        if seg.contains("private key") {
            return true;
        }
    }
    let api = starts(s, i, "api").map(|j| match s.get(j) {
        Some('_' | '-') => j + 1,
        _ => j,
    });
    ["password", "secret", "token"]
        .iter()
        .filter_map(|k| starts(s, i, k))
        .chain(api.and_then(|j| starts(s, j, "key")))
        .any(|j| assignment(s, j))
}

/// `ilio.py` `SECRET.search(v)`: true when `v` holds a secret shape.
pub(crate) fn ilio_secret(v: &str) -> bool {
    let s: Vec<char> = v.chars().map(|c| c.to_ascii_lowercase()).collect();
    (0..s.len()).any(|i| at(&s, i))
}
