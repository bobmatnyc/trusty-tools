//! `dashboard-db.py`'s write normalisation: item defaults, options, subs, links (#12029 P2g).
//!
//! Why: a new item gets its kind, status and dates from the command, a string
//! option becomes the object the page renders, and every local document link
//! is stored once, in the localhost reader form; the port must write the
//! same bytes as the frozen script for every shape it accepts.
//! What: [`default_item_fields`], [`normalize_options`], [`normalize_subs`],
//! [`normalize_specs`] (with `move_title_urls` and `reader_path`) and
//! [`with_question_title`]. The script's regexes are matched by hand on
//! Python's character classes ([`is_space`]).
//! Test: `dashboard_db_recordings_replay_clean_against_the_port` (`create-*`,
//! `set-specs`, `update-title-url`), `question_title_matches_the_script`.

use super::{Ctx, Fail, R, dict, has_key, merge, refuse, s, set};
use crate::pyjson_load::PyValue;
use crate::pystr::{is_space, py_repr, strip};
use crate::pyval::{get, py_str, repr, truthy, type_name};

/// `KIND_BY_PREFIX`.
const KIND_BY_PREFIX: [(&str, &str); 5] = [
    ("D", "decision"),
    ("B", "blocker"),
    ("T", "task"),
    ("A", "action"),
    ("Q", "question"),
];
/// `DETAIL_MIN`: a D item's plain-language context, in characters.
const DETAIL_MIN: usize = 120;
/// `LOCAL_HOST`.
pub(crate) const LOCAL_HOST: &str = "http://127.0.0.1:7890/";

fn pairs(v: &PyValue) -> Vec<(String, PyValue)> {
    match v {
        PyValue::Dict(p) => p.clone(),
        _ => Vec::new(),
    }
}

/// `data.setdefault(k, v)`.
fn setdefault(p: &mut Vec<(String, PyValue)>, k: &str, v: PyValue) {
    if !p.iter().any(|(x, _)| x == k) {
        p.push((k.to_owned(), v));
    }
}

/// Why: `create` on `items` fills what the page needs and refuses an item
/// with too little to act on.
/// What: `default_item_fields(prefix, data)`: `context` stored as `detail`,
/// `kind` from the prefix, `status` open, `created` now; a T task needs
/// `instruction` and `done_when`, a D item a `detail` of [`DETAIL_MIN`]
/// characters. A non-dict passes unchanged.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`create-items`).
pub(crate) fn default_item_fields(ctx: &Ctx, prefix: &str, data: PyValue) -> R<PyValue> {
    let PyValue::Dict(_) = data else {
        return Ok(data);
    };
    let mut p = pairs(&data);
    if let Some(i) = p.iter().position(|(k, _)| k == "context") {
        let ctx_v = p.remove(i).1;
        setdefault(&mut p, "detail", ctx_v);
    }
    let kind = KIND_BY_PREFIX.iter().find(|(k, _)| *k == prefix);
    if let Some((_, k)) = kind
        && !p.iter().any(|(x, v)| x == "kind" && truthy(v))
    {
        p = crate::dashboard::store::py::with_key(&p, "kind", s(k));
    }
    setdefault(&mut p, "status", s("open"));
    setdefault(&mut p, "created", s(&ctx.now_iso()));
    let data = PyValue::Dict(p);
    let blank = |k: &str| strip(&super::text_or(get(&data, k))).is_empty();
    if prefix == "T" && crate::pyval::is_str(get(&data, "kind"), "task") {
        let miss: Vec<String> = ["instruction", "done_when"]
            .into_iter()
            .filter(|k| blank(k))
            .map(|k| format!("`{k}`"))
            .collect();
        if !miss.is_empty() {
            return refuse(format!(
                "a T item needs {} at creation (instruction: the owner's words or a one-paragraph summary; done_when: the acceptance criteria)",
                miss.join(" and ")
            ));
        }
    }
    if prefix == "D" && strip(&super::text_or(get(&data, "detail"))).chars().count() < DETAIL_MIN {
        return refuse(format!(
            "a D item needs `detail` (alias `context`) of at least {DETAIL_MIN} characters: a few plain-language sentences on the decision context"
        ));
    }
    Ok(data)
}

/// Why: the page reads `{label, desc, recommended}` objects.
/// What: `normalize_options(data)`: string options become objects (the first
/// is recommended unless one is labelled "(Recommended)"); objects pass; any
/// other type, or a non-list, is refused.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`create-items`).
pub(crate) fn normalize_options(data: PyValue) -> R<PyValue> {
    let opts = match get(&data, "options") {
        None | Some(PyValue::Null) => return Ok(data),
        Some(PyValue::List(l)) => l.clone(),
        Some(_) => {
            return refuse(
                "options must be a list of strings or {label, desc, recommended} objects",
            );
        }
    };
    let marked = opts
        .iter()
        .any(|o| matches!(o, PyValue::Str(t) if t.to_lowercase().contains("(recommended)")));
    let mut out = Vec::with_capacity(opts.len());
    for (i, o) in opts.into_iter().enumerate() {
        match o {
            PyValue::Str(t) => {
                let rec = if marked {
                    t.to_lowercase().contains("(recommended)")
                } else {
                    i == 0
                };
                out.push(dict(vec![
                    ("label", PyValue::Str(t)),
                    ("desc", s("")),
                    ("recommended", PyValue::Bool(rec)),
                ]));
            }
            PyValue::Dict(_) => out.push(o),
            other => {
                return refuse(format!(
                    "options[{i}]: must be a string or an object, got {}",
                    type_name(&other)
                ));
            }
        }
    }
    Ok(set(&data, "options", PyValue::List(out)))
}

/// `x in values` with Python's `==`.
fn among(x: Option<&PyValue>, values: &[Option<&str>]) -> bool {
    let x = x.filter(|v| !matches!(v, PyValue::Null));
    values.iter().any(|v| match (x, v) {
        (None, None) => true,
        (Some(PyValue::Str(a)), Some(b)) => a == b,
        _ => false,
    })
}

/// Why: a multi-decision's questions need unique keys and the page's answer values.
/// What: `normalize_subs(data)`: `subs` must be a non-empty list of objects
/// with a key and a question, unique keys, `rec` yes/no, `answer` yes/no/other.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`create-bad`).
pub(crate) fn normalize_subs(data: PyValue) -> R<PyValue> {
    let subs = match get(&data, "subs") {
        None | Some(PyValue::Null) => return Ok(data),
        Some(PyValue::List(l)) if !l.is_empty() => l.clone(),
        Some(_) => {
            return refuse(
                "subs must be a non-empty list of {key, question, detail?, rec?} objects",
            );
        }
    };
    let mut seen: Vec<PyValue> = Vec::new();
    for (i, sub) in subs.iter().enumerate() {
        let field = |k: &str| strip(&get(sub, k).map_or(String::new(), py_str)).is_empty();
        if !matches!(sub, PyValue::Dict(_)) || field("key") || field("question") {
            return refuse(format!("subs[{i}]: needs a key and a question"));
        }
        let key = get(sub, "key").cloned().unwrap_or(PyValue::Null);
        if seen
            .iter()
            .any(|k| crate::dashboard::pyderive::py_eq(Some(k), Some(&key)))
        {
            return refuse(format!("subs[{i}]: duplicate key {}", repr(&key)));
        }
        seen.push(key);
        let rec_ok = among(get(sub, "rec"), &[None, Some("yes"), Some("no")]);
        let ans_ok = among(
            get(sub, "answer"),
            &[None, Some("yes"), Some("no"), Some("other")],
        );
        if !rec_ok || !ans_ok {
            return refuse(format!(
                "subs[{i}]: rec is yes or no; answer is yes, no or other"
            ));
        }
    }
    Ok(data)
}

/// `_TITLE_URL` (`https?://[^\s)<>"'\]]+`) and `_LOCAL_URL`'s tail class.
fn url_char(c: char, bracket: bool) -> bool {
    !(is_space(c) || matches!(c, ')' | '<' | '>' | '"' | '\'') || (bracket && c == ']'))
}

/// Every match of `prefix` followed by one or more URL characters, left to
/// right: `(start, end)` in characters.
fn url_spans(t: &[char], prefixes: &[&str], bracket: bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        let hit = prefixes.iter().find_map(|p| {
            let p: Vec<char> = p.chars().collect();
            let end = i + p.len();
            (t.len() > end && t[i..end] == p[..] && url_char(t[end], bracket)).then_some(end)
        });
        match hit {
            Some(mut end) => {
                while end < t.len() && url_char(t[end], bracket) {
                    end += 1;
                }
                out.push((i, end));
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

/// `pat.sub(f, text)` over the spans of [`url_spans`].
fn replace_urls(text: &str, spans: &[(usize, usize)], mut f: impl FnMut(&str) -> String) -> String {
    let t: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut at = 0;
    for &(a, b) in spans {
        out.extend(&t[at..a]);
        out.push_str(&f(&t[a..b].iter().collect::<String>()));
        at = b;
    }
    out.extend(&t[at..]);
    out
}

/// `move_title_urls(data)`: each URL in `title` becomes a `specs` entry
/// labelled host/last-segment, and the title keeps the label.
fn move_title_urls(data: PyValue) -> PyValue {
    let Some(PyValue::Str(title)) = get(&data, "title") else {
        return data;
    };
    if !title.contains("http") {
        return data;
    }
    let mut specs = match get(&data, "specs") {
        Some(PyValue::List(l)) => l.clone(),
        _ => Vec::new(),
    };
    let mut have: Vec<PyValue> = specs
        .iter()
        .filter(|e| matches!(e, PyValue::Dict(_)))
        .map(|e| get(e, "url").cloned().unwrap_or(PyValue::Null))
        .collect();
    let chars: Vec<char> = title.chars().collect();
    let spans = url_spans(&chars, &["http://", "https://"], true);
    let new_title = replace_urls(title, &spans, |m| {
        let url = m.trim_end_matches(['.', ',', ';', ':']);
        let tail = &m[url.len()..];
        let rest = url.split_once("://").map_or("", |(_, r)| r);
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let path = path.split('?').next().unwrap_or("");
        let path = path.split('#').next().unwrap_or("").trim_end_matches('/');
        let seg = path.rsplit('/').next().unwrap_or("");
        let label = if seg.is_empty() {
            host.to_owned()
        } else {
            format!("{host}/{seg}")
        };
        let u = s(url);
        if !have.contains(&u) {
            have.push(u.clone());
            specs.push(dict(vec![("label", s(&label)), ("url", u)]));
        }
        format!("{label}{tail}")
    });
    if new_title == *title {
        return data;
    }
    let data = set(&data, "title", PyValue::Str(new_title));
    set(&data, "specs", PyValue::List(specs))
}

/// `reader_path(rest)`: `doc/<p>` and `files/local/briefs/<f>` in the `#doc/` reader form.
/// Test: `reader_path_rewrites_the_legacy_brief_form`.
pub(super) fn reader_path(rest: &str) -> String {
    if rest.starts_with("doc/") {
        format!("#{rest}")
    } else if let Some(f) = rest.strip_prefix("files/") {
        if f.starts_with("local/briefs/") {
            format!("#doc/architect/{f}")
        } else {
            rest.to_owned()
        }
    } else {
        rest.to_owned()
    }
}

/// `_SUFFIX.sub("", label)`: a final `(localhost)` or `(tailnet)` and the
/// whitespace around it removed.
fn strip_suffix_label(label: &str) -> String {
    let body = label.trim_end_matches(is_space);
    for suf in ["(localhost)", "(tailnet)"] {
        if let Some(head) = body.strip_suffix(suf) {
            return head.trim_end_matches(is_space).to_owned();
        }
    }
    label.to_owned()
}

/// `rsplit("/", 1)[-1]`.
fn last_segment(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

/// Why: one spec entry per local document, in the reader form the page pairs
/// with its tailnet twin; a localhost URL in `detail` moves to the specs.
/// What: `normalize_specs(data)` on a dict (title URLs first); normalizes,
/// never refuses. `tail_host` is `TAIL_HOST`.
/// Test: `dashboard_db_recordings_replay_clean_against_the_port` (`set-specs`).
pub(crate) fn normalize_specs(data: PyValue, tail_host: &str) -> PyValue {
    if !matches!(data, PyValue::Dict(_)) {
        return data;
    }
    let data = move_title_urls(data);
    let specs = get(&data, "specs").cloned();
    let detail = match get(&data, "detail") {
        Some(PyValue::Str(d)) => Some(d.clone()),
        _ => None,
    };
    let dchars: Vec<char> = detail.as_deref().unwrap_or("").chars().collect();
    let found_spans = url_spans(&dchars, &[LOCAL_HOST], false);
    let found: Vec<String> = found_spans
        .iter()
        .map(|&(a, b)| dchars[a..b].iter().collect())
        .collect();
    let spec_list = match &specs {
        Some(PyValue::List(l)) => Some(l.clone()),
        _ => None,
    };
    if spec_list.is_none() && found.is_empty() {
        return data;
    }
    let brief = match get(&data, "brief") {
        Some(PyValue::Str(b)) => Some(b.clone()),
        _ => None,
    };
    let mut entries = spec_list.unwrap_or_default();
    for u in &found {
        let label = Some(last_segment(u))
            .filter(|l| !l.is_empty())
            .unwrap_or("doc");
        entries.push(dict(vec![("label", s(label)), ("url", s(u))]));
    }
    let (mut out, mut seen) = (Vec::new(), Vec::<String>::new());
    for e in entries {
        let url = match get(&e, "url") {
            Some(PyValue::Str(u)) if matches!(e, PyValue::Dict(_)) => Some(u.clone()),
            _ => None,
        };
        let host = [LOCAL_HOST, tail_host]
            .into_iter()
            .find(|h| !h.is_empty() && url.as_deref().is_some_and(|u| u.starts_with(h)));
        let (Some(host), Some(url)) = (host, url) else {
            out.push(e);
            continue;
        };
        let path = reader_path(&url[host.len()..]);
        if seen.contains(&path)
            || brief
                .as_deref()
                .is_some_and(|b| path == format!("#doc/{b}"))
        {
            continue;
        }
        seen.push(path.clone());
        let label = strip_suffix_label(&get(&e, "label").map_or(String::new(), py_str));
        let label = if label.is_empty() {
            last_segment(&path).to_owned()
        } else {
            label
        };
        let e = merge(
            &e,
            &dict(vec![
                ("label", s(&label)),
                ("url", s(&format!("{LOCAL_HOST}{path}"))),
            ]),
        );
        out.push(e);
    }
    let mut data = set(&data, "specs", PyValue::List(out));
    if let Some(d) = detail.filter(|_| !found.is_empty()) {
        data = set(
            &data,
            "detail",
            s(&replace_urls(&d, &found_spans, |_| "see doc links".into())),
        );
    }
    data
}

/// `re.sub(r"\[([^\]]+)\]\([^)]*\)", r"\1", text)`: markdown links to their text.
fn unlink(t: &str) -> String {
    let c: Vec<char> = t.chars().collect();
    let (mut out, mut i) = (String::new(), 0);
    while i < c.len() {
        if c[i] == '[' {
            let close = (i + 1..c.len()).find(|&j| c[j] == ']');
            if let Some(j) = close.filter(|&j| j > i + 1)
                && c.get(j + 1) == Some(&'(')
                && let Some(k) = (j + 2..c.len()).find(|&k| c[k] == ')')
            {
                out.extend(&c[i + 1..j]);
                i = k + 1;
                continue;
            }
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// Why: a question written without a title gets a short plain one.
/// What: `question_title(asked)`: links to their text, whitespace collapsed,
/// the first sentence (`.+?[?.!:]` then a space or the end) without a final
/// `.` or `:`, at most ten words, then `…` when cut.
/// Test: `question_title_matches_the_script`.
pub(crate) fn question_title(asked: &str) -> String {
    let t = crate::pystr::collapse_space(&unlink(asked));
    let c: Vec<char> = t.chars().collect();
    let end =
        (1..c.len()).find(|&k| "?.!:".contains(c[k]) && (k + 1 == c.len() || is_space(c[k + 1])));
    let head: String = match end {
        Some(k) => c[..=k]
            .iter()
            .collect::<String>()
            .trim_end_matches(['.', ':'])
            .to_owned(),
        None => t,
    };
    let w: Vec<&str> = head.split(' ').collect();
    let mut out = w[..w.len().min(10)].join(" ");
    if w.len() > 10 {
        out.push('\u{2026}');
    }
    out
}

/// `with_question_title(data)`: `title` from `asked` when absent (set and create only).
pub(crate) fn with_question_title(data: PyValue) -> R<PyValue> {
    if !matches!(data, PyValue::Dict(_)) || get(&data, "title").is_some_and(truthy) {
        return Ok(data);
    }
    match get(&data, "asked") {
        Some(PyValue::Str(a)) if !a.is_empty() => {
            let t = question_title(a);
            Ok(set(&data, "title", PyValue::Str(t)))
        }
        Some(v) if truthy(v) => Err(Fail::Raise(format!(
            "TypeError: expected string or bytes-like object, got {}",
            py_repr(type_name(v))
        ))),
        _ => Ok(data),
    }
}

/// `has_key` for the callers that test `"specs" in data`.
pub(crate) fn has_specs(data: &PyValue) -> bool {
    has_key(data, "specs")
}
