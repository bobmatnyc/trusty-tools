//! The `aliases.py` port: the alias table and its NAMES line (#12028 P1e).
//!
//! Why: the Architect resolves the owner's nicknames and project names
//! through one deterministic table instead of guessing; the prompt hook and
//! the `aliases` command read it, and `add` writes it.
//! What: the read side of the script. [`resolve_line`] is `resolve_line()`:
//! the prompt's word tokens (`[a-z0-9]+(?:[-_][a-z0-9]+)*`, caseless), the
//! longest alias first at each position, at most eight matches, `quiet`
//! aliases skipped unless asked, formatted as one `NAMES:` line or nothing.
//! [`describe`] is the short target text; [`entries`] walks the table the
//! way the script's loops do, Python's iteration rules included (a dict
//! yields its keys, a string its characters, a number raises `TypeError`).
//! `check` validates the schema; `cli` is the command line and the writer.
//! Test: the `aliases` recordings in `tests/replay_writers.rs`.

mod check;
mod cli;

use std::collections::{HashMap, HashSet};

pub use cli::{Options, run};

use crate::pyjson_load::PyValue;
use crate::pyval::{get, py_str, truthy, type_name};

/// The entry kinds and their table keys, in the script's order.
pub(crate) const KINDS: [(&str, &str); 3] = [
    ("person", "people"),
    ("project", "projects"),
    ("host", "hosts"),
];

const MAX_MATCHES: usize = 8;

/// Python's `TypeError` text for iterating a value that is not iterable.
fn not_iterable(v: &PyValue) -> String {
    format!("TypeError: '{}' object is not iterable", type_name(v))
}

/// `[x for x in (v or []) if isinstance(x, str)]`, Python's iteration rules.
pub(crate) fn str_items(v: Option<&PyValue>) -> Result<Vec<String>, String> {
    match v {
        None => Ok(Vec::new()),
        Some(v) if !truthy(v) => Ok(Vec::new()),
        Some(PyValue::List(items)) => Ok(items
            .iter()
            .filter_map(|x| match x {
                PyValue::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect()),
        Some(PyValue::Dict(pairs)) => Ok(pairs.iter().map(|(k, _)| k.clone()).collect()),
        Some(PyValue::Str(s)) => Ok(s.chars().map(String::from).collect()),
        Some(other) => Err(not_iterable(other)),
    }
}

/// `entries(table)`: `(kind, entry)` for every dict entry with a `str` name.
pub(crate) fn entries(table: &PyValue) -> Result<Vec<(&'static str, &PyValue)>, String> {
    let mut out = Vec::new();
    for (kind, key) in KINDS {
        match get(table, key) {
            Some(PyValue::List(items)) => out.extend(
                items
                    .iter()
                    .filter(|e| matches!(get(e, "name"), Some(PyValue::Str(_))))
                    .map(|e| (kind, e)),
            ),
            Some(v) if truthy(v) && !matches!(v, PyValue::Dict(_) | PyValue::Str(_)) => {
                return Err(not_iterable(v));
            }
            _ => {}
        }
    }
    Ok(out)
}

/// The entry's `name` (present: [`entries`] yields only named entries).
pub(crate) fn name(e: &PyValue) -> &str {
    match get(e, "name") {
        Some(PyValue::Str(s)) => s,
        _ => "",
    }
}

/// `_TOK` matches `c`: `[a-z0-9]` under `re.IGNORECASE`.
fn tok_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '\u{130}' | '\u{131}' | '\u{17f}' | '\u{212a}')
}

/// `_TOK.finditer(text)`: the byte spans of the word tokens.
fn tokens(text: &str) -> Vec<(usize, usize)> {
    let c: Vec<(usize, char)> = text.char_indices().collect();
    let at = |i: usize| c.get(i).map_or(text.len(), |p| p.0);
    let tok = |i: usize| c.get(i).is_some_and(|p| tok_char(p.1));
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if !tok(i) {
            i += 1;
            continue;
        }
        let start = i;
        while tok(i) {
            i += 1;
        }
        while matches!(c.get(i), Some((_, '-' | '_'))) && tok(i + 1) {
            i += 1;
            while tok(i) {
                i += 1;
            }
        }
        out.push((at(start), at(i)));
    }
    out
}

/// `norm(alias)`: the lowercase word tokens.
pub(crate) fn norm(alias: &str) -> Vec<String> {
    tokens(alias)
        .into_iter()
        .map(|(a, b)| alias[a..b].to_lowercase())
        .collect()
}

type Index<'a> = HashMap<Vec<String>, Vec<(&'static str, &'a PyValue, bool)>>;

/// `build_index(table)`: alias tokens to `(kind, entry, quiet)`, and the
/// longest alias in tokens.
fn build_index(table: &PyValue) -> Result<(Index<'_>, usize), String> {
    let mut idx: Index<'_> = HashMap::new();
    let mut longest = 1;
    for (kind, e) in entries(table)? {
        let quiet: HashSet<Vec<String>> = str_items(get(e, "quiet"))?
            .iter()
            .map(|q| norm(q))
            .collect();
        let mut seen: HashSet<Vec<String>> = HashSet::new();
        let mut names = vec![name(e).to_owned()];
        names.extend(str_items(get(e, "aliases"))?);
        for a in names {
            let k = norm(&a);
            if k.is_empty() || seen.contains(&k) {
                continue;
            }
            let q = quiet.contains(&k);
            longest = longest.max(k.len());
            idx.entry(k.clone()).or_default().push((kind, e, q));
            seen.insert(k);
        }
    }
    Ok((idx, longest))
}

/// One match: the surface text and its candidates.
type Match<'a> = (String, Vec<(&'static str, &'a PyValue)>);

/// `resolve(text, table, include_quiet)`.
fn resolve<'a>(
    text: &str,
    table: &'a PyValue,
    include_quiet: bool,
) -> Result<Vec<Match<'a>>, String> {
    let (idx, longest) = build_index(table)?;
    let toks = tokens(text);
    let low: Vec<String> = toks
        .iter()
        .map(|&(a, b)| text[a..b].to_lowercase())
        .collect();
    let (mut out, mut seen, mut i): (Vec<Match<'a>>, Vec<&[String]>, usize) =
        (Vec::new(), Vec::new(), 0);
    while i < toks.len() && out.len() < MAX_MATCHES {
        let mut hit = None;
        for n in (1..=longest.min(toks.len() - i)).rev() {
            let key = &low[i..i + n];
            let cands: Vec<(&'static str, &PyValue)> = idx
                .get(key)
                .map(|v| {
                    v.iter()
                        .filter(|c| include_quiet || !c.2)
                        .map(|c| (c.0, c.1))
                        .collect()
                })
                .unwrap_or_default();
            if !cands.is_empty() {
                hit = Some((n, cands));
                break;
            }
        }
        let Some((n, cands)) = hit else {
            i += 1;
            continue;
        };
        let key = &low[i..i + n];
        if !seen.contains(&key) {
            seen.push(key);
            out.push((text[toks[i].0..toks[i + n - 1].1].to_owned(), cands));
        }
        i += n;
    }
    Ok(out)
}

/// `_repo_name(e)`: the last path part of `github_repo` or `repo_path`.
fn repo_name(e: &PyValue) -> Option<String> {
    ["github_repo", "repo_path"].iter().find_map(|k| {
        let v = get(e, k).filter(|v| truthy(v))?;
        let s = py_str(v);
        let s = s.trim_end_matches('/');
        Some(s.rsplit('/').next().unwrap_or(s).to_owned())
    })
}

/// `describe(kind, e)`: person `name (github)`, project `sessions (repo)`,
/// host `name`; a truthy `display` wins.
pub(crate) fn describe(kind: &str, e: &PyValue) -> Result<String, String> {
    if let Some(d) = get(e, "display").filter(|d| truthy(d)) {
        return Ok(py_str(d));
    }
    Ok(match kind {
        "person" => match str_items(get(e, "github"))?.first() {
            Some(g) => format!("{} ({g})", name(e)),
            None => name(e).to_owned(),
        },
        "project" => {
            let sess: Vec<String> = str_items(get(e, "sessions"))?.into_iter().take(2).collect();
            let target = if sess.is_empty() {
                name(e).to_owned()
            } else {
                sess.join("/")
            };
            match repo_name(e) {
                Some(r) if !r.is_empty() && r != target => format!("{target} ({r})"),
                _ => target,
            }
        }
        _ => name(e).to_owned(),
    })
}

fn format_line(matches: &[Match<'_>]) -> Result<String, String> {
    let mut parts = Vec::new();
    for (surface, cands) in matches {
        if let [(kind, e)] = cands.as_slice() {
            let target = describe(kind, e)?;
            if target.to_lowercase() != surface.to_lowercase() {
                parts.push(format!("{surface} = {target}"));
            }
        } else {
            let list: Vec<String> = cands
                .iter()
                .map(|(k, e)| format!("{} ({k})", name(e)))
                .collect();
            parts.push(format!("{surface} = {}", list.join(" | ")));
        }
    }
    Ok(if parts.is_empty() {
        String::new()
    } else {
        format!("NAMES: {}", parts.join("; "))
    })
}

/// `resolve_line(text)` on a loaded table: the NAMES line, or `""` for no
/// match or any error (the script never raises here).
///
/// Why: the prompt hook prints this line before the Architect reads a prompt.
/// What: blank text is `""`; otherwise tokens, longest-alias matching and
/// the line format above.
/// Test: the `aliases/resolve-*` recordings in `tests/replay_writers.rs`.
pub(crate) fn resolve_line(text: &str, table: &PyValue, include_quiet: bool) -> String {
    if crate::pystr::strip(text).is_empty() {
        return String::new();
    }
    resolve(text, table, include_quiet)
        .and_then(|m| format_line(&m))
        .unwrap_or_default()
}
