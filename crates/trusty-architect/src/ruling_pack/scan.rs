//! The pure parts of `ruling_pack.py`: key-map lookup, identifier scan, label (#12028 P1i).
//!
//! Why: these decide which project, palaces and grep terms a pack uses, and
//! the script matches them with `re` patterns; the crate takes no `regex`
//! dependency, so each pattern is matched by hand with Python's semantics.
//! What: [`resolve`] is `resolve()`, [`identifiers`] is `identifiers()`,
//! [`trim`] and [`label`] are the script's helpers of the same names.
//! Test: `resolve_matches_key_alias_palace_index_and_repo`,
//! `identifiers_follow_the_script_patterns`, `trim_and_label_follow_the_script`,
//! the `identifiers` recording.

use crate::pyjson_load::PyValue;
use crate::pystr::{is_digit, is_word, strip};
use crate::pyval::{get, py_str, truthy};

/// The script's `GENERIC_SYMBOL`: lower-cased tokens that are never symbols.
const GENERIC_SYMBOL: [&str; 13] = [
    "the", "and", "for", "with", "what", "does", "this", "that", "from", "into", "are", "was",
    "how",
];

/// The file extensions of the script's file pattern, in its alternation order.
const EXTS: [&str; 12] = [
    "py", "rs", "ts", "tsx", "js", "md", "json", "toml", "yml", "yaml", "sql", "sh",
];

/// `str(v or "")`: the text of a value, empty when it is falsy or absent.
pub(crate) fn text_or_empty(v: Option<&PyValue>) -> String {
    v.filter(|v| truthy(v)).map(py_str).unwrap_or_default()
}

/// `resolve(name, projects)`: the key and entry a name stands for.
///
/// Why: a ruling names a project by any of its handles; mistyped palace and
/// index names were the failure the script exists to stop.
/// What: `name` is stripped and lower-cased. A key or alias equal to it wins
/// (first entry in key-map order); else the first entry whose palace, index,
/// repo or repo's last `/` segment equals it. `None` when nothing matches.
/// Test: `resolve_matches_key_alias_palace_index_and_repo`.
pub(crate) fn resolve<'a>(
    name: &str,
    projects: &'a [(String, PyValue)],
) -> Option<(&'a str, &'a PyValue)> {
    let n = strip(name).to_lowercase();
    let aliases = |e: &PyValue| match get(e, "aliases") {
        Some(PyValue::List(items)) => items.iter().map(|a| py_str(a).to_lowercase()).collect(),
        _ => Vec::new(),
    };
    let by_key = projects
        .iter()
        .find(|(k, e)| k.to_lowercase() == n || aliases(e).contains(&n));
    let by_handle = || {
        projects.iter().find(|(_, e)| {
            let repo = text_or_empty(get(e, "repo")).to_lowercase();
            let short = repo.rsplit('/').next().unwrap_or_default().to_owned();
            let palace = text_or_empty(get(e, "palace")).to_lowercase();
            let index = text_or_empty(get(e, "index")).to_lowercase();
            [palace, index, repo, short].contains(&n)
        })
    };
    by_key.or_else(by_handle).map(|(k, e)| (k.as_str(), e))
}

/// Pushes `item` unless `list` holds it (`dict.fromkeys` order).
fn push_new(list: &mut Vec<String>, item: String) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// `re.findall(r"#(\d{1,6})", q)`.
fn hash_numbers(q: &[char]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < q.len() {
        if q[i] == '#' {
            let n = q[i + 1..]
                .iter()
                .take(6)
                .take_while(|c| is_digit(**c))
                .count();
            if n > 0 {
                out.push(q[i + 1..i + 1 + n].iter().collect());
                i += 1 + n;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The script's file pattern `[\w./-]+\.(?:py|...|sh)\b`, all matches.
///
/// From a start, `[\w./-]+` is greedy, so the match ends at the last `.ext`
/// in the run that a word boundary follows; with none, no later start in
/// the same run matches either.
fn file_names(q: &[char]) -> Vec<String> {
    let in_class = |c: char| is_word(c) || matches!(c, '.' | '/' | '-');
    let word_at = |i: usize| q.get(i).is_some_and(|c| is_word(*c));
    let mut out = Vec::new();
    let mut s = 0;
    while s < q.len() {
        if !in_class(q[s]) {
            s += 1;
            continue;
        }
        let run_end = (s..q.len()).find(|&i| !in_class(q[i])).unwrap_or(q.len());
        let ext_end = |p: usize| {
            EXTS.iter().find_map(|e| {
                let end = p + 1 + e.chars().count();
                let ok =
                    end <= q.len() && q[p + 1..end].iter().copied().eq(e.chars()) && !word_at(end);
                ok.then_some(end)
            })
        };
        let found = (s + 1..run_end)
            .rev()
            .filter(|&p| q[p] == '.')
            .find_map(ext_end);
        match found {
            Some(end) => {
                out.push(q[s..end].iter().collect());
                s = end;
            }
            None => s = run_end,
        }
    }
    out
}

/// `re.findall(r"[A-Za-z_][A-Za-z0-9_]{3,}", q)`.
fn tokens(q: &[char]) -> Vec<String> {
    let head = |c: char| c.is_ascii_alphabetic() || c == '_';
    let tail = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = Vec::new();
    let mut i = 0;
    while i < q.len() {
        if head(q[i]) {
            let end = (i + 1..q.len()).find(|&j| !tail(q[j])).unwrap_or(q.len());
            if end - i >= 4 {
                out.push(q[i..end].iter().collect());
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The numbers, files and symbols a question names.
pub(crate) struct Identifiers {
    /// Issue and PR numbers as text, the flags first, then `#N` in the question.
    pub(crate) nums: Vec<String>,
    /// File names: `--file` values, then path-like question tokens.
    pub(crate) files: Vec<String>,
    /// Symbols: `--symbol` values, then snake_case and camelCase tokens.
    pub(crate) symbols: Vec<String>,
}

/// `identifiers(question, issues, prs, files, symbols)`.
///
/// Why: the pack greps each named identifier exactly, so the scan must pick
/// the same tokens as the script.
/// What: `nums` and `files` are deduplicated in first-seen order; a question
/// token joins `symbols` when it is not generic, not already present, and
/// holds `_` or a lower-then-upper pair; `--symbol` values are kept as given.
/// Test: `identifiers_follow_the_script_patterns`, the `identifiers` recording.
pub(crate) fn identifiers(
    question: &str,
    issues: &[String],
    prs: &[String],
    files: &[String],
    symbols: &[String],
) -> Identifiers {
    let q: Vec<char> = question.chars().collect();
    let mut nums = Vec::new();
    for n in issues.iter().chain(prs).cloned().chain(hash_numbers(&q)) {
        push_new(&mut nums, n);
    }
    let mut fl = Vec::new();
    for f in files.iter().cloned().chain(file_names(&q)) {
        push_new(&mut fl, f);
    }
    let mut sy = symbols.to_vec();
    for tok in tokens(&q) {
        let lower = tok.to_ascii_lowercase();
        if GENERIC_SYMBOL.contains(&lower.as_str()) || sy.contains(&tok) {
            continue;
        }
        let cs: Vec<char> = tok.chars().collect();
        let camel = cs
            .windows(2)
            .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase());
        if tok.contains('_') || camel {
            sy.push(tok);
        }
    }
    Identifiers {
        nums,
        files: fl,
        symbols: sy,
    }
}

/// `trim(s, n)`: whitespace runs collapsed; past `n` characters, the first
/// `n - 1` and an ellipsis.
pub(crate) fn trim(s: &str, n: usize) -> String {
    let s = crate::pystr::collapse_space(s);
    if s.chars().count() <= n {
        return s;
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// `label(prior, fact, checked)`: HIGH for all three, MEDIUM for two, else LOW.
pub(crate) fn label(prior: bool, fact: bool, checked: bool) -> &'static str {
    match u8::from(prior) + u8::from(fact) + u8::from(checked) {
        3 => "HIGH",
        2 => "MEDIUM",
        _ => "LOW",
    }
}
