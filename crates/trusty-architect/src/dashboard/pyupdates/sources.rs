//! `dashboard_updates.py`'s four sources: the inbox files and the dashboard items (#12029 P2h).
//!
//! Why: an update entry's `at` and text are pure functions of its source
//! line, which is what makes `ingest` idempotent with no cursor; the port
//! must derive the same `(at, by, text)` from the same line.
//! What: [`one_paragraph`], [`parse_handback`], the three line parsers
//! ([`relay_entry`], [`message_entry`], [`note_entry`]), [`candidates`] (the
//! inbox files in their fixed order) and [`item_entries`] (owner answers,
//! relays and done outcomes of non-task items). An unreadable or non-JSON
//! line is skipped; a file that is not UTF-8 raises, as Python's text read does.
//! Test: `one_paragraph_cuts_by_code_point`, `item_entries_use_the_neutral_answer_text`,
//! `an_unparseable_inbox_line_is_skipped`.

use std::path::Path;

use super::re::{Pattern, bare, full_match, handback, search, tag};
use super::{Entry, Fail};
use crate::dashboard::store::py::PyDocOut;
use crate::pyjson_load::{PyValue, loads};
use crate::pystr::{collapse_space, is_space, rstrip, splitlines, strip};
use crate::pyval::{get, py_str, truthy};

/// `MAX_CHARS`.
pub const MAX_CHARS: usize = 400;

/// The inbox files and their parsers, in `candidates`' order.
pub(crate) const SOURCES: [&str; 3] = ["relays.jsonl", "mcp-messages.jsonl", "task-notes.jsonl"];

/// The three patterns, built once per pass.
pub(crate) struct Patterns {
    pub(crate) tag: Pattern,
    pub(crate) bare: Pattern,
    pub(crate) handback: Pattern,
}

impl Patterns {
    pub(crate) fn new() -> Self {
        Self {
            tag: tag(),
            bare: bare(),
            handback: handback(),
        }
    }
}

/// Why: every entry text is cut to one short paragraph.
/// What: `one_paragraph(text, limit)`: the text before the first
/// `\n\s*\n` of `text.strip()`, whitespace runs collapsed to one space, cut
/// to `limit` code points with `…` (the cut is `s[:limit-1].rstrip()`).
/// Test: `one_paragraph_cuts_by_code_point`.
pub fn one_paragraph(text: &str, limit: usize) -> String {
    let t: Vec<char> = strip(text).chars().collect();
    let mut end = t.len();
    'scan: for i in (0..t.len()).filter(|&i| t[i] == '\n') {
        for c in &t[i + 1..] {
            if !is_space(*c) {
                break;
            }
            if *c == '\n' {
                end = i;
                break 'scan;
            }
        }
    }
    let first: String = t[..end].iter().collect();
    let s = collapse_space(&first);
    if s.chars().count() <= limit {
        return s;
    }
    let head: String = s.chars().take(limit.saturating_sub(1)).collect();
    format!("{}…", rstrip(&head))
}

/// `parse_handback(text)`: `(task id, done text, pct)` from the last non-empty line.
pub(crate) fn parse_handback(p: &Patterns, text: &str) -> Option<(String, String, u32)> {
    let last = splitlines(text)
        .into_iter()
        .rfind(|x| !strip(x).is_empty())?;
    let g = full_match(&p.handback, strip(last))?;
    let pct = g[2].chars().try_fold(0u32, |acc, c| {
        Some(acc * 10 + crate::pystr::digit_value(c)?)
    })?;
    Some((g[0].clone(), g[1].clone(), pct))
}

/// `str(v or default)`.
fn or(v: Option<&PyValue>, default: &str) -> String {
    v.filter(|v| truthy(v))
        .map_or_else(|| default.to_owned(), py_str)
}

fn entry(task: String, at: &PyValue, by: String, text: &str) -> Entry {
    Entry {
        task,
        at: at.clone(),
        by,
        text: one_paragraph(text, MAX_CHARS),
    }
}

/// `relay_entry(row)`: a relay tagged `Task: <id>` or a bare `ar-` id.
pub(crate) fn relay_entry(p: &Patterns, row: &PyValue) -> Option<Entry> {
    let PyValue::Str(text) = get(row, "text")? else {
        return None;
    };
    let ts = get(row, "ts").filter(|v| truthy(v))?;
    let g = search(&p.tag, text).or_else(|| search(&p.bare, text))?;
    let by = format!("architect relay to {}", or(get(row, "session"), "session"));
    Some(entry(g[0].clone(), ts, by, text))
}

/// `message_entry(row)`: a PM handback line.
pub(crate) fn message_entry(p: &Patterns, row: &PyValue) -> Option<Entry> {
    let PyValue::Str(text) = get(row, "text")? else {
        return None;
    };
    let (task, done, pct) = parse_handback(p, text)?;
    let ts = get(row, "ts").filter(|v| truthy(v))?;
    let body = format!("done: {done} (pct {pct})");
    Some(entry(task, ts, or(get(row, "sender"), "pm"), &body))
}

/// `note_entry(row)`: one task note.
pub(crate) fn note_entry(row: &PyValue) -> Option<Entry> {
    let task = get(row, "task").filter(|v| truthy(v))?;
    let ts = get(row, "ts").filter(|v| truthy(v))?;
    let PyValue::Str(text) = get(row, "text")? else {
        return None;
    };
    if strip(text).is_empty() {
        return None;
    }
    let task = strip(&py_str(task)).to_owned();
    Some(entry(task, ts, or(get(row, "by"), "architect"), text))
}

/// `_lines(path)`: each JSON object line; an unopenable file gives none, a
/// line that is not JSON or not an object is skipped.
///
/// # Errors
/// [`Fail::Raise`] for a file that is not UTF-8 (Python's decode error).
pub(crate) fn lines(path: &Path) -> Result<Vec<PyValue>, Fail> {
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(Vec::new());
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(t) => crate::writers::universal_newlines(t.to_owned()),
        Err(e) => {
            let msg = crate::pyjson_file::decode_error(&bytes, &e);
            return Err(Fail::Raise(format!("UnicodeDecodeError: {msg}")));
        }
    };
    Ok(text
        .split_inclusive('\n')
        .filter_map(|raw| loads(raw).ok())
        .filter(|row| matches!(row, PyValue::Dict(_)))
        .collect())
}

/// `candidates(inbox)`: relays, handbacks, then task notes, in file order.
///
/// # Errors
/// [`lines`]' decode error.
pub(crate) fn candidates(p: &Patterns, inbox: &Path) -> Result<Vec<Entry>, Fail> {
    let mut out = Vec::new();
    for (n, name) in SOURCES.iter().enumerate() {
        for row in lines(&inbox.join(name))? {
            let e = match n {
                0 => relay_entry(p, &row),
                1 => message_entry(p, &row),
                _ => note_entry(&row),
            };
            out.extend(e);
        }
    }
    Ok(out)
}

/// The owner-answer entry's text. #12029 P2h: neutral wording (ruled
/// divergence); the frozen script names the owner here.
pub const ANSWERED: &str = "Owner answered";

/// `item_entries(docs)`: the answer, relay and done state of every non-task
/// item that names an IL task.
pub(crate) fn item_entries(docs: &[PyDocOut]) -> Vec<Entry> {
    let mut out = Vec::new();
    for d in docs {
        let x = &d.data;
        let task = get(x, "task").filter(|v| truthy(v));
        let is_task = matches!(get(x, "kind"), Some(PyValue::Str(k)) if k == "task");
        let Some(task) = task.filter(|_| !is_task) else {
            continue;
        };
        let (i, tid) = (&d.id, py_str(task));
        if let Some(at) = get(x, "answered_at").filter(|v| truthy(v)) {
            let mut text = format!("{ANSWERED} {i}: {}", or(get(x, "answer"), ""));
            if let Some(note) = get(x, "answer_note").filter(|v| truthy(v)) {
                text.push_str(&format!(": {}", py_str(note)));
            }
            out.push(entry(tid.clone(), at, "owner".into(), &text));
        }
        if let Some(at) = get(x, "relayed_at").filter(|v| truthy(v)) {
            let text = format!("{i} relayed to {}", or(get(x, "session"), "session"));
            out.push(entry(tid.clone(), at, "architect".into(), &text));
        }
        let done = matches!(get(x, "status"), Some(PyValue::Str(s)) if s == "done");
        let outcome = get(x, "outcome").filter(|v| truthy(v));
        let done_at = get(x, "done_at").filter(|v| truthy(v));
        if let (true, Some(outcome), Some(at)) = (done, outcome, done_at) {
            let text = format!("{i} done: {}", py_str(outcome));
            out.push(entry(tid, at, "architect".into(), &text));
        }
    }
    out
}
