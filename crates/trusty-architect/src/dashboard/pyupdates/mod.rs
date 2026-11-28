//! The port of `dashboard_updates.py`: task update snippets ingested into dashboard items (#12029 P2h).
//!
//! Why: the Architect's task items carry an `updates` list built from its
//! relays, PM handbacks, its own task notes and the owner's answers. The
//! ingest is deterministic (no model call, no cursor) and, during the
//! overlap, writes the same store as the frozen script, so the port must
//! pick the same entries, skip the same duplicates and write the same bytes
//! under the same `flock`.
//! What: [`ingest`] reads the three inbox files ([`sources::candidates`]) and
//! the items' own state ([`sources::item_entries`]), keeps entries whose task
//! has a `kind: task` item ([`pick_task_items`]) and whose `at` is not older
//! than the window floor (a string compare), drops any `(at, text)` already
//! on the item or already queued, and writes each item once through
//! `PyStore::update` with `if_version`. A `list_docs` failure, a conflict
//! and a failed write are reported in the result, never raised.
//! [`add_note`] appends one `task-notes.jsonl` line. [`stale_notes`] and
//! [`annotate_stale`] are the read-time `note_stale` marks. [`cli::run`] is
//! the script's `main`.
//! Intended divergences, each with its test: the owner-answer text reads
//! [`sources::ANSWERED`] (`item_entries_use_the_neutral_answer_text`); an
//! exception the script does not catch prints its traceback's last line,
//! exit 1. `check_item` (the page button's threaded pass) is not ported: no
//! replay case calls it, and the server that calls it is not ported yet.
//! Test: `dashboard_updates_recordings_replay_clean_against_the_port`, the
//! error-arm tests in `tests.rs`.

pub mod cases;
pub mod cli;
pub(crate) mod re;
pub mod sources;
pub(crate) mod store;
#[cfg(test)]
mod tests;

use std::path::Path;

use crate::dashboard::pydb::set;
use crate::dashboard::store::py::{PyDocOut, PyStoreError};
use crate::pyjson_load::{PyInt, PyValue};
use crate::pystr::strip;
use crate::pyval::{dumps, get, py_str, truthy};
use sources::Patterns;
pub(crate) use store::ItemStore;

pub use sources::{MAX_CHARS, one_paragraph};

/// `SINCE_HOURS`.
pub const SINCE_HOURS: f64 = 72.0;

/// One candidate update: `(task id, at, by, text)`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    /// The IL task id as the source names it (`str()` of it).
    pub task: String,
    /// The source line's timestamp, any JSON value.
    pub at: PyValue,
    /// Who wrote it.
    pub by: String,
    /// One paragraph, at most [`MAX_CHARS`] code points.
    pub text: String,
}

/// An exception the script does not catch.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Fail {
    /// A store exception (an `OSError`, a `SchemaError`, the writer-class refusal).
    #[error("{}", raised(.0))]
    Store(PyStoreError),
    /// Any other: its traceback's last line, e.g. `TypeError: unhashable type: 'list'`.
    #[error("{0}")]
    Raise(String),
}

impl From<PyStoreError> for Fail {
    fn from(e: PyStoreError) -> Self {
        Self::Store(e)
    }
}

/// The last traceback line of an uncaught store exception.
pub(crate) fn raised(e: &PyStoreError) -> String {
    match e {
        PyStoreError::Os { class, msg, .. } => format!("{class}: {msg}"),
        e if e.http().is_some() => format!("dashboard_store.{}: {e}", e.class()),
        PyStoreError::Schema(m) => format!("dashboard_schema.SchemaError: {m}"),
        e => format!("{}: {e}", e.class()),
    }
}

/// `ingest`'s answer: `{added: {item id: n}, conflicts: [id], errors: [text]}`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ingested {
    /// Entries added per item, in first-write order.
    pub added: Vec<(String, usize)>,
    /// Items whose write met a version conflict or a deleted item.
    pub conflicts: Vec<String>,
    /// The store errors, as text.
    pub errors: Vec<String>,
}

impl Ingested {
    /// The answer as the script's dict.
    pub(crate) fn value(&self) -> PyValue {
        let n = |k: &usize| PyValue::Int(PyInt::Small(i64::try_from(*k).unwrap_or(i64::MAX)));
        let strs =
            |v: &[String]| PyValue::List(v.iter().map(|s| PyValue::Str(s.clone())).collect());
        PyValue::Dict(vec![
            (
                "added".into(),
                PyValue::Dict(self.added.iter().map(|(k, v)| (k.clone(), n(v))).collect()),
            ),
            ("conflicts".into(), strs(&self.conflicts)),
            ("errors".into(), strs(&self.errors)),
        ])
    }

    /// `json.dumps(ingest(...))`.
    pub fn json(&self) -> String {
        dumps(&self.value(), None, true)
    }
}

/// `_norm(task_id)`: trimmed, lower case, without an `ar-` prefix.
pub(crate) fn norm(task: &str) -> String {
    let t = strip(task).to_lowercase();
    t.strip_prefix("ar-").map_or(t.clone(), str::to_owned)
}

/// `str(v or "")`.
fn text_of(v: Option<&PyValue>) -> String {
    v.filter(|v| truthy(v)).map_or_else(String::new, py_str)
}

/// Why: several `kind: task` items may name one IL task; exactly one receives its entries.
/// What: `pick_task_items(docs)`: per normalised task id, the item whose
/// `(status == "done", str(created or "~"))` is least, the first on a tie;
/// the answer is `(task id, index into docs)` in first-seen order.
/// Test: `pick_task_items_prefers_open_then_oldest`.
pub(crate) fn pick_task_items(docs: &[PyDocOut]) -> Vec<(String, usize)> {
    let mut best: Vec<(String, (bool, String), usize)> = Vec::new();
    for (i, d) in docs.iter().enumerate() {
        let x = &d.data;
        let is_task = matches!(get(x, "kind"), Some(PyValue::Str(k)) if k == "task");
        let Some(task) = get(x, "task").filter(|v| truthy(v) && is_task) else {
            continue;
        };
        let done = matches!(get(x, "status"), Some(PyValue::Str(s)) if s == "done");
        let created = get(x, "created")
            .filter(|v| truthy(v))
            .map_or_else(|| "~".to_owned(), py_str);
        let key = (done, created);
        let tid = norm(&py_str(task));
        match best.iter_mut().find(|b| b.0 == tid) {
            Some(b) if key < b.1 => (b.1, b.2) = (key, i),
            Some(_) => {}
            None => best.push((tid, key, i)),
        }
    }
    best.into_iter().map(|(t, _, i)| (t, i)).collect()
}

fn lookup(tasks: &[(String, usize)], tid: &str) -> Option<usize> {
    tasks.iter().find(|(t, _)| t == tid).map(|(_, i)| *i)
}

/// `stale_notes(docs)`: `{task item id: [ids]}` of non-task items answered
/// after the task's status note (`status_at`, else `updated`).
pub(crate) fn stale_notes(docs: &[PyDocOut]) -> Vec<(String, Vec<String>)> {
    let tasks = pick_task_items(docs);
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for d in docs {
        let x = &d.data;
        let is_task = matches!(get(x, "kind"), Some(PyValue::Str(k)) if k == "task");
        let task = get(x, "task").filter(|v| truthy(v) && !is_task);
        let Some(t) = task.and_then(|t| lookup(&tasks, &norm(&py_str(t)))) else {
            continue;
        };
        let Some(answered) = get(x, "answered_at").filter(|v| truthy(v)) else {
            continue;
        };
        let td = &docs[t].data;
        let note_at = text_of(
            get(td, "status_at")
                .filter(|v| truthy(v))
                .or(get(td, "updated")),
        );
        if py_str(answered) > note_at {
            let id = docs[t].id.clone();
            match out.iter_mut().find(|(k, _)| *k == id) {
                Some((_, v)) => v.push(d.id.clone()),
                None => out.push((id, vec![d.id.clone()])),
            }
        }
    }
    out
}

/// `annotate_stale(docs)`: copies where a stale task carries `note_stale`
/// and the sorted `note_stale_by`.
pub(crate) fn annotate_stale(docs: &[PyDocOut]) -> Vec<PyDocOut> {
    let stale = stale_notes(docs);
    docs.iter()
        .map(|d| match stale.iter().find(|(k, _)| *k == d.id) {
            Some((_, ids)) => {
                let mut ids = ids.clone();
                ids.sort();
                let by = PyValue::List(ids.into_iter().map(PyValue::Str).collect());
                let data = set(
                    &set(&d.data, "note_stale", PyValue::Bool(true)),
                    "note_stale_by",
                    by,
                );
                PyDocOut { data, ..d.clone() }
            }
            None => d.clone(),
        })
        .collect()
}

/// Python's `hash()` of an `(at, text)` tuple refuses a list or a dict.
fn hashable(v: Option<&PyValue>) -> Result<(), Fail> {
    let refuse = |t: &str| {
        Err(Fail::Raise(format!(
            "TypeError: cannot use 'tuple' as a set element (unhashable type: '{t}')"
        )))
    };
    match v {
        Some(PyValue::List(_)) => refuse("list"),
        Some(PyValue::Dict(_)) => refuse("dict"),
        _ => Ok(()),
    }
}

/// `list(v or [])` for the `updates` field.
fn as_list(v: Option<&PyValue>) -> Result<Vec<PyValue>, Fail> {
    match v.filter(|v| truthy(v)) {
        None => Ok(Vec::new()),
        Some(PyValue::List(l)) => Ok(l.clone()),
        Some(PyValue::Str(s)) => Ok(s.chars().map(|c| PyValue::Str(c.into())).collect()),
        Some(PyValue::Dict(p)) => Ok(p.iter().map(|(k, _)| PyValue::Str(k.clone())).collect()),
        Some(other) => Err(Fail::Raise(format!(
            "TypeError: '{}' object is not iterable",
            crate::pyval::type_name(other)
        ))),
    }
}

fn same(a: &PyValue, at: &PyValue) -> bool {
    crate::dashboard::pyderive::py_eq(Some(a), Some(at))
}

/// Why: the one write path for update entries; idempotent with no cursor.
/// What: `ingest(store_root, inbox, now, since_hours)` on `store` (actor
/// `poller`); see the module doc. `floor` is
/// `(now - since_hours).strftime("%Y-%m-%dT%H:%M:%SZ")`.
/// Test: `ingest_is_idempotent_and_dedups_on_at_and_text`,
/// `a_list_docs_failure_is_an_errors_entry`, `a_conflict_is_reported_and_writes_nothing`.
///
/// # Errors
/// [`Fail::Raise`] where the script raises: a store exception that is not a
/// `StoreError` (an `OSError` from the listing, the writer-class refusal),
/// an inbox file that is not UTF-8, an unhashable `at`.
pub(crate) fn ingest<S: ItemStore + ?Sized>(
    store: &S,
    inbox: &Path,
    now: f64,
    since_hours: f64,
) -> Result<Ingested, Fail> {
    let floor = crate::classify::alerts::iso((now - since_hours * 3600.0).floor());
    let mut out = Ingested::default();
    let docs = match store.list_items() {
        Ok(d) => d,
        Err(e) if e.http().is_some() => {
            out.errors.push(e.to_string());
            return Ok(out);
        }
        Err(e) => return Err(e.into()),
    };
    let by_task = pick_task_items(&docs);
    let p = Patterns::new();
    let mut all = sources::candidates(&p, inbox)?;
    all.extend(sources::item_entries(&docs));
    let mut new: Vec<(String, Vec<Entry>)> = Vec::new();
    for e in all {
        let Some(di) = lookup(&by_task, &norm(&e.task)) else {
            continue;
        };
        if py_str(&e.at) < floor {
            continue;
        }
        let d = &docs[di];
        let mut have = Vec::new();
        for u in as_list(get(&d.data, "updates"))? {
            if matches!(u, PyValue::Dict(_)) {
                let (a, t) = (get(&u, "at"), get(&u, "text"));
                hashable(a)?;
                hashable(t)?;
                have.push((
                    a.cloned().unwrap_or(PyValue::Null),
                    t.cloned().unwrap_or(PyValue::Null),
                ));
            }
        }
        hashable(Some(&e.at))?;
        let text = PyValue::Str(e.text.clone());
        if have.iter().any(|(a, t)| same(a, &e.at) && same(t, &text)) {
            continue;
        }
        let slot = match new.iter().position(|(id, _)| *id == d.id) {
            Some(i) => i,
            None => {
                new.push((d.id.clone(), Vec::new()));
                new.len() - 1
            }
        };
        let mine = &mut new[slot].1;
        if mine.iter().any(|u| same(&u.at, &e.at) && u.text == e.text) {
            continue;
        }
        mine.push(e);
    }
    new.retain(|(_, adds)| !adds.is_empty());
    for (item_id, adds) in new {
        let Some(d) = docs.iter().find(|x| x.id == item_id) else {
            continue;
        };
        let mut merged = as_list(get(&d.data, "updates"))?;
        merged.extend(adds.iter().map(|u| {
            PyValue::Dict(vec![
                ("at".into(), u.at.clone()),
                ("by".into(), PyValue::Str(u.by.clone())),
                ("text".into(), PyValue::Str(u.text.clone())),
            ])
        }));
        let body = PyValue::Dict(vec![("updates".into(), PyValue::List(merged))]);
        match store.update_item(&item_id, &body, &d.version, None) {
            Ok(_) => out.added.push((item_id, adds.len())),
            Err(PyStoreError::Conflict(_) | PyStoreError::NotFound(_)) => {
                out.conflicts.push(item_id);
            }
            Err(e @ PyStoreError::Os { .. }) => out.errors.push(format!("items/{item_id}: {e}")),
            Err(e) if e.http().is_some() => out.errors.push(format!("items/{item_id}: {e}")),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(out)
}

/// `add_note(task, text, by, inbox, now)`: appends one line to
/// `<inbox>/task-notes.jsonl` (the directory made first) and returns it.
///
/// # Errors
/// [`Fail::Raise`] with the `OSError` line when the directory or the file cannot be written.
pub fn add_note(task: &str, text: &str, by: &str, inbox: &Path, now: f64) -> Result<String, Fail> {
    use std::io::Write;
    let s = |v: &str| PyValue::Str(v.to_owned());
    let row = PyValue::Dict(vec![
        ("ts".into(), s(&crate::classify::alerts::iso(now.floor()))),
        ("task".into(), s(task)),
        ("by".into(), s(by)),
        ("text".into(), s(text)),
    ]);
    let line = dumps(&row, None, true);
    let dir = inbox.to_string_lossy();
    crate::dashboard::store::py::makedirs(&dir).map_err(Fail::from)?;
    let path = inbox.join("task-notes.jsonl");
    let shown = path.to_string_lossy().into_owned();
    let os = |e: std::io::Error| Fail::from(PyStoreError::os(&shown, &e));
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .map_err(os)?;
    f.write_all(format!("{line}\n").as_bytes()).map_err(os)?;
    Ok(line)
}
