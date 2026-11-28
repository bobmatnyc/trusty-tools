//! The replay driver's mirror for `dashboard_attach` (#12029 P2h).
//!
//! Why: the module has no command line (the dashboard server calls it), so
//! the recordings run `tests/fixtures/replay/dashboard_attach/driver.py`
//! case by case against the frozen module; the port replays the same lists.
//! What: [`replay_cases`] runs each `{"op": ...}` of `CASES.json` and prints
//! `case <i> <op>` and the answer as `json.dumps(sort_keys=True)` lines, or
//! the exception in the driver's form. An upload body is read from the file
//! the case names (`body`), its length the file's size unless given.
//! Test: `dashboard_attach_recordings_replay_clean_against_the_port`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{AttachError, Upload, dirs, handle_upload, names, record, save, serve_file};
use crate::clock::Clock;
use crate::dashboard::pyupdates::ItemStore;
use crate::dashboard::store::py::{Options, PyStore, PyStoreError, error_line, file_op, sorted};
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

fn opt<'a>(c: &'a PyValue, k: &str) -> Option<&'a str> {
    match get(c, k) {
        Some(PyValue::Str(v)) => Some(v),
        _ => None,
    }
}

fn json(v: &PyValue) -> String {
    dumps(&sorted(v), None, false)
}

fn text(v: &str) -> PyValue {
    PyValue::Str(v.to_owned())
}

fn int(n: i64) -> PyValue {
    PyValue::Int(PyInt::Small(n))
}

fn line(e: &AttachError) -> String {
    match e {
        AttachError::Store(e) => error_line(e),
        AttachError::TooLarge(m) => format!("TooLarge 413 too_large: {m}"),
        AttachError::Raise(l) => l.clone(),
    }
}

fn os(path: &str) -> impl Fn(std::io::Error) -> AttachError + '_ {
    move |e| AttachError::Store(PyStoreError::os(path, &e))
}

struct Driver {
    cwd: PathBuf,
    env: BTreeMap<String, Option<String>>,
    stores: BTreeMap<String, PyStore>,
    clock: Arc<dyn Clock + Send + Sync>,
}

impl Driver {
    fn var(&self, k: &str) -> Option<String> {
        match self.env.get(k) {
            Some(v) => v.clone(),
            None => std::env::var(k).ok(),
        }
    }

    fn store(&self, c: &PyValue) -> Result<&PyStore, AttachError> {
        let name = get(c, "store").map_or_else(|| "s".to_owned(), py_str);
        self.stores
            .get(&name)
            .ok_or_else(|| AttachError::Raise(format!("KeyError: {}", py_repr(&name))))
    }

    /// The body file and its length (`length`, else the file's size).
    fn body(&self, c: &PyValue) -> Result<(Vec<u8>, i64), AttachError> {
        let bytes = match opt(c, "body") {
            Some(p) => std::fs::read(p).map_err(os(p))?,
            None => Vec::new(),
        };
        let length = match get(c, "length") {
            Some(PyValue::Int(PyInt::Small(n))) => *n,
            _ => i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        };
        Ok((bytes, length))
    }

    fn answer(&mut self, c: &PyValue) -> Result<Vec<String>, AttachError> {
        let ok = Ok(vec!["ok".to_owned()]);
        match s(c, "op") {
            "env" => {
                if let Some(PyValue::Dict(set)) = get(c, "set") {
                    for (k, v) in set {
                        self.env.insert(k.clone(), Some(py_str(v)));
                    }
                }
                if let Some(PyValue::List(unset)) = get(c, "unset") {
                    for k in unset {
                        self.env.insert(py_str(k), None);
                    }
                }
                return ok;
            }
            "new" => {
                let opts = Options {
                    fmt: opt(c, "fmt").map(str::to_owned),
                    ..Options::default()
                };
                let schemas = self.cwd.join("schemas/dashboard");
                let st = {
                    let env = |k: &str| self.var(k);
                    PyStore::new(
                        s(c, "root"),
                        opts,
                        &env,
                        &self.cwd,
                        schemas,
                        self.clock.clone(),
                    )?
                };
                self.stores
                    .insert(get(c, "name").map_or_else(|| "s".into(), py_str), st);
                return ok;
            }
            _ => {}
        }
        let env = |k: &str| self.var(k);
        match s(c, "op") {
            "symlink" => {
                std::os::unix::fs::symlink(s(c, "target"), s(c, "path"))
                    .map_err(os(s(c, "path")))?;
                ok
            }
            "ls" => match std::fs::read_dir(s(c, "path")) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec!["absent".into()]),
                Err(e) => Err(os(s(c, "path"))(e)),
                Ok(rd) => {
                    let mut names: Vec<PyValue> = rd
                        .filter_map(Result::ok)
                        .map(|e| PyValue::Str(e.file_name().to_string_lossy().into_owned()))
                        .collect();
                    names.sort_by_key(py_str);
                    Ok(vec![json(&PyValue::List(names))])
                }
            },
            "set" => {
                let st = self.store(c)?;
                let (coll, id) = (text(s(c, "coll")), text(s(c, "id")));
                st.set(&coll, &id, get(c, "data"), None, None)?;
                ok
            }
            "get" => {
                let d = self.store(c)?.get_item(s(c, "id"))?;
                Ok(vec![format!(
                    "{} {}",
                    crate::pyval::int_text(&d.version),
                    json(&d.data)
                )])
            }
            "clean_name" => Ok(vec![json(&text(&names::clean_name(opt(c, "raw"))?))]),
            "content_type" => Ok(vec![json(&text(names::content_type(s(c, "name"))))]),
            "max_bytes" => Ok(vec![names::max_bytes(&env).to_string()]),
            "item_id" => Ok(vec![json(&PyValue::Bool(names::item_id_ok(s(c, "id"))))]),
            "session_re" => Ok(vec![json(&PyValue::Bool(dirs::session_ok(s(c, "s"))))]),
            "read_schedule" => {
                let rows = dirs::read_schedule(Path::new(s(c, "path")));
                let v = rows.into_iter().map(|e| {
                    PyValue::Dict(vec![
                        ("name".into(), text(&e.name)),
                        ("project".into(), text(&e.project)),
                        ("code".into(), text(&e.code)),
                    ])
                });
                Ok(vec![json(&PyValue::List(v.collect()))])
            }
            "attach_dir" => {
                let session = match get(c, "session") {
                    Some(PyValue::Str(v)) => Some(v.as_str()),
                    _ => None,
                };
                let (d, il) = dirs::attach_dir(
                    session,
                    Path::new(s(c, "schedule")),
                    s(c, "architect_il"),
                    opt(c, "store_root"),
                    &self.cwd,
                );
                Ok(vec![json(&PyValue::List(vec![text(&d), text(il)]))])
            }
            "safe_dir" => Ok(vec![json(&text(&dirs::safe_dir(
                s(c, "base"),
                s(c, "item_id"),
            )?))]),
            "save" => {
                let (bytes, length) = self.body(c)?;
                let limit = match get(c, "limit") {
                    Some(PyValue::Int(PyInt::Small(n))) => u64::try_from(*n).unwrap_or(0),
                    _ => names::max_bytes(&env),
                };
                let mut r: &[u8] = &bytes;
                let v = save::save(
                    &mut r,
                    length,
                    s(c, "name"),
                    s(c, "directory"),
                    s(c, "item_id"),
                    limit,
                )?;
                let size = int(i64::try_from(v.size).unwrap_or(i64::MAX));
                Ok(vec![json(&PyValue::List(vec![
                    text(&v.stored),
                    text(&v.path),
                    size,
                    text(&v.sha256),
                ]))])
            }
            "record" => {
                let st = self.store(c)?;
                let entry = get(c, "entry").cloned().unwrap_or(PyValue::Null);
                let v = record(st, s(c, "item_id"), &entry, opt(c, "actor"))?;
                let r = PyValue::Dict(vec![
                    ("collection".into(), text("items")),
                    ("id".into(), text(s(c, "item_id"))),
                    ("version".into(), PyValue::Int(v)),
                ]);
                Ok(vec![json(&r)])
            }
            "upload" => {
                let (bytes, length) = self.body(c)?;
                let mut r: &[u8] = &bytes;
                let st = self.store(c)?;
                let u = Upload {
                    item_id: s(c, "item_id"),
                    raw_name: opt(c, "name"),
                    body: &mut r,
                    length,
                    schedule: Path::new(s(c, "schedule")),
                    architect_il: s(c, "architect_il"),
                    store_root: opt(c, "store_root"),
                    actor: opt(c, "actor"),
                    env: &env,
                    cwd: &self.cwd,
                };
                Ok(vec![json(&handle_upload(st, u)?)])
            }
            "serve" => {
                let st = self.store(c)?;
                let (data, ctype, inline) =
                    serve_file(st, s(c, "item_id"), s(c, "stored_name"), &env)?;
                let py = if inline { "True" } else { "False" };
                Ok(vec![format!(
                    "{} {} {ctype} {py}",
                    data.len(),
                    crate::sha256::sha256_hex(&data)
                )])
            }
            other => Err(AttachError::Raise(format!("KeyError: {}", py_repr(other)))),
        }
    }
}

/// Why: the one entry `examples/replay_port.rs dashboard_attach` dispatches to.
/// What: see the module doc.
/// Test: `dashboard_attach_recordings_replay_clean_against_the_port`.
pub fn replay_cases(args: &[String], clock: Arc<dyn Clock + Send + Sync>) -> Outcome {
    let done = |stdout: String, stderr: String, code: i32| Outcome {
        stdout,
        stderr,
        code,
    };
    let [path] = args else {
        return done(String::new(), "usage: driver.py CASES.json\n".into(), 2);
    };
    let cases = match read_text(Path::new(path)).map(|t| loads(&t)) {
        Ok(Ok(PyValue::List(cases))) => cases,
        _ => return done(String::new(), format!("cannot read {path}\n"), 1),
    };
    let mut d = Driver {
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        env: BTreeMap::new(),
        stores: BTreeMap::new(),
        clock,
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    for (i, case) in cases.iter().enumerate() {
        stdout.push_str(&format!("case {i} {}\n", s(case, "op")));
        let lines = match file_op(case) {
            Some(r) => r.map_err(AttachError::Store),
            None => d.answer(case),
        };
        for l in lines.unwrap_or_else(|e| vec![line(&e)]) {
            stdout.push_str(&format!("  {l}\n"));
        }
        for st in d.stores.values() {
            stderr.push_str(&st.take_stderr());
        }
    }
    done(stdout, stderr, 0)
}
