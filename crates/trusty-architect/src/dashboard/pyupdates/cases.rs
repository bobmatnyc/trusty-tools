//! The replay driver's mirror for `dashboard_updates` (#12029 P2h).
//!
//! Why: the recordings run `tests/fixtures/replay/dashboard_updates/driver.py`
//! against the frozen module, both its command line (`cli ARGS...`) and its
//! functions case by case (`cases CASES.json`); the port replays both.
//! What: [`replay`]: `cli` runs [`super::cli::run`] with the working
//! directory as the checkout; `cases` runs each `{"op": ...}` and prints
//! `case <i> <op>` and the answer as `json.dumps(sort_keys=True)` lines, or
//! the exception in the driver's form.
//! Test: `dashboard_updates_recordings_replay_clean_against_the_port`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::sources::{self, Patterns};
use super::{Entry, Fail, ItemStore, SINCE_HOURS, add_note, annotate_stale, ingest};
use super::{pick_task_items, stale_notes};
use crate::clock::Clock;
use crate::dashboard::store::py::{Options, PyDocOut, PyStore, error_line, file_op, sorted};
use crate::pyjson_file::read_text;
use crate::pyjson_load::{PyInt, PyValue, loads};
use crate::pystr::py_repr;
use crate::pyval::{dumps, get, py_str};
use crate::spec_drift::Outcome;

fn s<'a>(c: &'a PyValue, k: &str) -> &'a str {
    match get(c, k) {
        Some(PyValue::Str(v)) => v,
        _ => "",
    }
}

fn json(v: &PyValue) -> String {
    dumps(&sorted(v), None, false)
}

fn entry(e: &Entry) -> PyValue {
    PyValue::List(vec![
        PyValue::Str(e.task.clone()),
        e.at.clone(),
        PyValue::Str(e.by.clone()),
        PyValue::Str(e.text.clone()),
    ])
}

fn opt_entry(e: Option<Entry>) -> String {
    json(&e.as_ref().map_or(PyValue::Null, entry))
}

fn lines_or_empty(v: Vec<String>) -> Vec<String> {
    if v.is_empty() {
        vec!["empty".into()]
    } else {
        v
    }
}

fn driver_doc(d: PyDocOut) -> PyValue {
    PyValue::Dict(vec![
        ("id".into(), PyValue::Str(d.id)),
        ("version".into(), PyValue::Int(d.version)),
        ("data".into(), d.data),
    ])
}

fn fail_line(f: &Fail) -> String {
    match f {
        Fail::Store(e) => error_line(e),
        Fail::Raise(l) => l.clone(),
    }
}

struct Driver {
    cwd: PathBuf,
    stores: BTreeMap<String, PyStore>,
    clock: Arc<dyn Clock + Send + Sync>,
    /// What the stores wrote to stderr.
    err: String,
}

impl Driver {
    fn path(&self, c: &PyValue, k: &str, default: &str) -> PathBuf {
        match get(c, k) {
            Some(PyValue::Str(p)) => self.cwd.join(p),
            _ => self.cwd.join(default),
        }
    }

    fn open(&self, root: &str, actor: Option<&str>, fmt: Option<String>) -> Result<PyStore, Fail> {
        let opts = Options {
            fmt,
            actor: actor.map(str::to_owned),
            ..Options::default()
        };
        let env = |k: &str| std::env::var(k).ok();
        let schemas = self.cwd.join("schemas/dashboard");
        Ok(PyStore::new(
            root,
            opts,
            &env,
            &self.cwd,
            schemas,
            self.clock.clone(),
        )?)
    }

    fn store(&self, c: &PyValue) -> Result<&PyStore, Fail> {
        let name = get(c, "store").map_or_else(|| "s".to_owned(), py_str);
        self.stores
            .get(&name)
            .ok_or_else(|| Fail::Raise(format!("KeyError: {}", py_repr(&name))))
    }

    fn answer(&mut self, c: &PyValue) -> Result<Vec<String>, Fail> {
        let p = Patterns::new();
        let text = s(c, "text");
        let row = get(c, "row").cloned().unwrap_or(PyValue::Null);
        let one = |v: PyValue| Ok(vec![json(&v)]);
        let opt_s =
            |v: Option<Vec<String>>| one(v.map_or(PyValue::Null, |g| PyValue::Str(g[0].clone())));
        match s(c, "op") {
            "new" => {
                let fmt = get(c, "fmt")
                    .filter(|f| !matches!(f, PyValue::Null))
                    .map(py_str);
                let store = self.open(s(c, "root"), None, fmt)?;
                self.stores
                    .insert(get(c, "name").map_or_else(|| "s".into(), py_str), store);
                Ok(vec!["ok".into()])
            }
            "set" => {
                let st = self.store(c)?;
                let (coll, id) = (
                    PyValue::Str(s(c, "coll").into()),
                    PyValue::Str(s(c, "id").into()),
                );
                let r = st.set(&coll, &id, get(c, "data"), None, None);
                self.err.push_str(&st.take_stderr());
                r?;
                Ok(vec!["ok".into()])
            }
            "get" => {
                let st = self.store(c)?;
                let d = st.get_item(s(c, "id"))?;
                Ok(vec![format!(
                    "{} {}",
                    crate::pyval::int_text(&d.version),
                    json(&d.data)
                )])
            }
            "one_paragraph" => {
                let limit = match get(c, "limit") {
                    Some(PyValue::Int(PyInt::Small(n))) => usize::try_from(*n).unwrap_or(0),
                    _ => super::MAX_CHARS,
                };
                one(PyValue::Str(sources::one_paragraph(text, limit)))
            }
            "tag" => opt_s(super::re::search(&p.tag, text)),
            "bare" => opt_s(super::re::search(&p.bare, text)),
            "handback" => one(sources::parse_handback(&p, text).map_or(
                PyValue::Null,
                |(t, d, n)| {
                    PyValue::List(vec![
                        PyValue::Str(t),
                        PyValue::Str(d),
                        PyValue::Int(PyInt::Small(i64::from(n))),
                    ])
                },
            )),
            "relay" => Ok(vec![opt_entry(sources::relay_entry(&p, &row))]),
            "message" => Ok(vec![opt_entry(sources::message_entry(&p, &row))]),
            "note" => Ok(vec![opt_entry(sources::note_entry(&row))]),
            "candidates" => {
                let inbox = self.path(c, "inbox", "inbox");
                let all = sources::candidates(&p, &inbox)?;
                Ok(lines_or_empty(
                    all.iter().map(|e| json(&entry(e))).collect(),
                ))
            }
            "add_note" => {
                let by = get(c, "by").map_or_else(|| "architect".into(), py_str);
                let inbox = self.path(c, "inbox", "inbox");
                let row = add_note(s(c, "task"), text, &by, &inbox, self.clock.now_unix())?;
                Ok(vec![json(
                    &loads(&row).map_err(|e| Fail::Raise(e.py_str()))?,
                )])
            }
            "ingest" => {
                let root = get(c, "root").map_or_else(|| "local/owner-queue".into(), py_str);
                let st = self.open(&root, Some("poller"), None)?;
                let hours = match get(c, "since_hours") {
                    Some(PyValue::Int(PyInt::Small(n))) => *n as f64,
                    Some(PyValue::Float(f)) => *f,
                    _ => SINCE_HOURS,
                };
                let inbox = self.path(c, "inbox", "inbox");
                let done = ingest(&st, &inbox, self.clock.now_unix(), hours);
                self.err.push_str(&st.take_stderr());
                Ok(vec![json(&done?.value())])
            }
            op @ ("items" | "pick" | "stale" | "annotate") => {
                let docs = self.store(c)?.list_items()?;
                Ok(match op {
                    "items" => lines_or_empty(
                        sources::item_entries(&docs)
                            .iter()
                            .map(|e| json(&entry(e)))
                            .collect(),
                    ),
                    "pick" => vec![json(&PyValue::Dict(
                        pick_task_items(&docs)
                            .into_iter()
                            .map(|(t, i)| (t, PyValue::Str(docs[i].id.clone())))
                            .collect(),
                    ))],
                    "stale" => vec![json(&PyValue::Dict(
                        stale_notes(&docs)
                            .into_iter()
                            .map(|(k, v)| {
                                (k, PyValue::List(v.into_iter().map(PyValue::Str).collect()))
                            })
                            .collect(),
                    ))],
                    _ => annotate_stale(&docs)
                        .into_iter()
                        .map(|d| json(&driver_doc(d)))
                        .collect(),
                })
            }
            other => Err(Fail::Raise(format!("KeyError: {}", py_repr(other)))),
        }
    }
}

const USAGE: &str = "usage: driver.py cli ARGS... | cases CASES.json\n";

fn done(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// Why: the one entry `examples/replay_port.rs dashboard_updates` dispatches to.
/// What: see the module doc.
/// Test: `dashboard_updates_recordings_replay_clean_against_the_port`.
pub fn replay(args: &[String], clock: Arc<dyn Clock + Send + Sync>) -> Outcome {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match args.split_first() {
        Some((m, rest)) if m == "cli" => {
            let env = |k: &str| std::env::var(k).ok();
            let o = super::cli::Options {
                checkout: cwd.clone(),
                cwd,
                env: &env,
                clock,
            };
            super::cli::run(rest, &o)
        }
        Some((m, [path])) if m == "cases" => {
            let cases = match read_text(Path::new(path)).map(|t| loads(&t)) {
                Ok(Ok(PyValue::List(cases))) => cases,
                _ => return done(String::new(), format!("cannot read {path}\n"), 1),
            };
            let mut d = Driver {
                cwd,
                stores: BTreeMap::new(),
                clock,
                err: String::new(),
            };
            let mut stdout = String::new();
            for (i, case) in cases.iter().enumerate() {
                stdout.push_str(&format!("case {i} {}\n", s(case, "op")));
                let lines = match file_op(case) {
                    Some(r) => r.map_err(Fail::Store),
                    None => d.answer(case),
                };
                for line in lines.unwrap_or_else(|e| vec![fail_line(&e)]) {
                    stdout.push_str(&format!("  {line}\n"));
                }
            }
            done(stdout, d.err, 0)
        }
        _ => done(String::new(), USAGE.into(), 2),
    }
}
