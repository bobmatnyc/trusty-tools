//! The port of `dashboard-db.py`: the Architect's command line over the dashboard store (#12029 P2g).
//!
//! Why: the Architect reads and writes dashboard documents with this command,
//! with or without the dashboard server running; during the overlap the
//! frozen script and the port write the same store, so the port must take the
//! same `flock`, write the same bytes and refuse the same writes, and print
//! the same JSON, lines and exit codes.
//! What: [`cli::run`] parses the frozen script's argparse command line (18
//! sub-commands, `--root` / `$DASHBOARD_ROOT`, `--actor` / `$DASHBOARD_ACTOR`)
//! and runs it on a [`PyStore`]: `normalize` (the `items` field defaults,
//! options, subs, title URLs and `specs`), `checks` (the write refusals),
//! `warn` (the stderr warnings), `write` (set, update, create, delete, log,
//! status-brief, batch, the progress stamp and the questions kick),
//! `summary` (tally, footer, summary, requests, validate, fsck, pruned) and
//! `reminders` (the footer's `Reminders (ack): ...` line).
//! Intended divergences from the frozen script, each with its test:
//! the footer reads the reminders with one bounded `datg reminders --due
//! --json` call, where the script imports `datg` and `il_remind`
//! (`reminders_line_formats_the_datg_rows_as_il_remind_does`); the questions
//! kick runs `questions::rc` in process after the store write released its
//! lock, where the script spawns `questions_rc.py` detached
//! (`kick_runs_after_the_write_and_never_changes_the_exit_code`); an
//! exception the script does not catch (a schema file that cannot load, an
//! `OSError` outside the store) prints its last traceback line and exits 1.
//! Test: `dashboard_db_recordings_replay_clean_against_the_port`, the
//! failure-path tests in `tests.rs`.

mod checks;
pub mod cli;
mod help;
mod normalize;
mod reminders;
mod replay;
mod summary;
#[cfg(test)]
mod tests;
mod warn;
mod write;

use std::cell::RefCell;
use std::path::PathBuf;

use super::pyderive::PySnap;
use super::store::py::{PyDocOut, PyStore, PyStoreError, with_key};
use crate::pyjson_load::{PyInt, PyValue};
use crate::pyval::get;
use crate::tasks::store_read::PyDoc;

pub use cli::{Options, run};
pub use replay::replay;

/// How a command failed, as the frozen script ends.
#[derive(Debug)]
pub(crate) enum Fail {
    /// An exception from the store: a `StoreError` is caught (exit 1, 2 or
    /// 3); any other class ends the script in a traceback.
    Store(PyStoreError),
    /// An exception the script does not catch: its traceback's last line.
    Raise(String),
}

impl From<PyStoreError> for Fail {
    fn from(e: PyStoreError) -> Self {
        Self::Store(e)
    }
}

/// A result of the port.
pub(crate) type R<T> = Result<T, Fail>;

/// `raise ds.StoreError(msg)`.
pub(crate) fn refuse<T>(msg: impl Into<String>) -> R<T> {
    Err(Fail::Store(PyStoreError::Store(msg.into())))
}

/// One run's state: the store, the caller's options and the stderr text in order.
pub(crate) struct Ctx<'a> {
    pub(crate) store: PyStore,
    pub(crate) o: &'a Options<'a>,
    /// `TAIL_HOST`: the tailnet origin, `""` for none.
    pub(crate) tail_host: String,
    err: RefCell<String>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(store: PyStore, o: &'a Options<'a>, tail_host: String, err: String) -> Self {
        Self {
            store,
            o,
            tail_host,
            err: RefCell::new(err),
        }
    }

    /// `print(line, file=sys.stderr)`: the store's own lines first.
    pub(crate) fn warn(&self, line: &str) {
        self.sync();
        let mut e = self.err.borrow_mut();
        e.push_str(line);
        e.push('\n');
    }

    /// Appends what the store wrote to stderr since the last call.
    pub(crate) fn sync(&self) {
        let more = self.store.take_stderr();
        self.err.borrow_mut().push_str(&more);
    }

    /// The stderr text so far.
    pub(crate) fn stderr(&self) -> String {
        self.sync();
        self.err.borrow().clone()
    }

    /// `datetime.now(timezone.utc)` as Unix seconds.
    pub(crate) fn now(&self) -> f64 {
        self.o.clock.now_unix()
    }

    /// `now.strftime("%Y-%m-%dT%H:%M:%SZ")`.
    pub(crate) fn now_iso(&self) -> String {
        crate::classify::alerts::iso(self.now().floor())
    }

    /// `ds.DEFAULT_ROOT`.
    pub(crate) fn default_root(&self) -> PathBuf {
        self.o.default_root()
    }

    /// `dashboard_schema.SCHEMA_DIR`.
    pub(crate) fn schema_dir(&self) -> PathBuf {
        self.o.schema_dir()
    }

    /// `store.get(coll, doc_id)`.
    pub(crate) fn get(&self, coll: &str, id: &PyValue) -> Result<PyDocOut, PyStoreError> {
        self.store.get(&s(coll), id)
    }

    /// `store.get(coll, doc_id)["data"]` merged under `data` (`{**stored, **data}`);
    /// `None` when the read raises a `StoreError` (the callers pass then).
    pub(crate) fn merged_over(
        &self,
        coll: &str,
        id: &PyValue,
        data: &PyValue,
    ) -> R<Option<PyValue>> {
        match self.get(coll, id) {
            Ok(doc) => Ok(Some(merge(&doc.data, data))),
            Err(e) if e.http().is_some() => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// `store.list_docs(coll)` as derive's documents.
    pub(crate) fn docs(&self, coll: &str) -> Result<Vec<PyDoc>, PyStoreError> {
        Ok(self
            .store
            .list_docs(&s(coll), None, None, false, None)?
            .into_iter()
            .map(py_doc)
            .collect())
    }

    /// `derive.snapshot(store, colls)`.
    pub(crate) fn snapshot(&self, colls: &[&str]) -> Result<PySnap, PyStoreError> {
        crate::dashboard::pyids::snapshot_of(&self.store, colls)
    }
}

/// A store document as derive reads it.
pub(crate) fn py_doc(d: PyDocOut) -> PyDoc {
    PyDoc {
        version: match d.version {
            PyInt::Small(v) => u64::try_from(v).unwrap_or(1),
            PyInt::Big(_) => 1,
        },
        id: d.id,
        data: d.data,
    }
}

/// A Python `str`.
pub(crate) fn s(v: &str) -> PyValue {
    PyValue::Str(v.to_owned())
}

/// A Python `int`.
pub(crate) fn int(n: i64) -> PyValue {
    PyValue::Int(PyInt::Small(n))
}

/// A dict from `(key, value)` pairs.
pub(crate) fn dict(pairs: Vec<(&str, PyValue)>) -> PyValue {
    PyValue::Dict(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

/// `{**old, **new}` (`dict(old, **new)`); a non-dict side adds nothing.
pub(crate) fn merge(old: &PyValue, new: &PyValue) -> PyValue {
    let mut out = match old {
        PyValue::Dict(p) => p.clone(),
        _ => Vec::new(),
    };
    if let PyValue::Dict(p) = new {
        for (k, v) in p {
            out = with_key(&out, k, v.clone());
        }
    }
    PyValue::Dict(out)
}

/// `dict(d, k=v)`: `d` with `k` replaced in place or appended.
pub(crate) fn set(d: &PyValue, k: &str, v: PyValue) -> PyValue {
    match d {
        PyValue::Dict(p) => PyValue::Dict(with_key(p, k, v)),
        other => other.clone(),
    }
}

/// `k in d` for a dict.
pub(crate) fn has_key(d: &PyValue, k: &str) -> bool {
    get(d, k).is_some()
}

/// `list(v)`: a list's items, a string's characters, a dict's keys.
pub(crate) fn py_list(v: &PyValue) -> Vec<PyValue> {
    match v {
        PyValue::List(l) => l.clone(),
        PyValue::Str(t) => t.chars().map(|c| PyValue::Str(c.into())).collect(),
        PyValue::Dict(p) => p.iter().map(|(k, _)| s(k)).collect(),
        _ => Vec::new(),
    }
}

/// `str(v or "")`.
pub(crate) fn text_or(v: Option<&PyValue>) -> String {
    v.filter(|v| crate::pyval::truthy(v))
        .map_or(String::new(), crate::pyval::py_str)
}

/// `re.match(r"[DA][0-9a-km-np-z]{2}$", text)` and `ID_RE` share the digit set.
pub(crate) fn id_digit(c: char) -> bool {
    crate::ids36::DIGITS.contains(c)
}
