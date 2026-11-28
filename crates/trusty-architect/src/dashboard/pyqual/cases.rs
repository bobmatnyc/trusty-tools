//! The replay driver's mirror for `dashboard_qual` (#12029 P2d).
//!
//! Why: the recordings run `tests/fixtures/replay/dashboard_qual/driver.py`
//! against the frozen module; the port replays the same case lists.
//! What: [`replay_cases`] runs each `{"op": ...}` of `CASES.json` and prints
//! `case <i> <op>` and the answer as one `json.dumps(sort_keys=True)` line,
//! or the exception in the driver's form. The default root is
//! `local/owner-queue` under the working directory, the replay sandbox.
//! Test: `dashboard_qual_recordings_replay_clean_against_the_port`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{alias_rules, alias_target, qualify, resolve, split, store_sid, with_qualifier};
use crate::clock::Clock;
use crate::dashboard::store::py::{
    Options, PyDocOut, PyStore, PyStoreError, error_line, file_op, sorted,
};
use crate::pyjson_file::read_text;
use crate::pyjson_load::{PyValue, loads};
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

fn text(v: String) -> PyValue {
    PyValue::Str(v)
}

fn opt_text(v: Option<String>) -> PyValue {
    v.map_or(PyValue::Null, PyValue::Str)
}

fn doc(d: PyDocOut) -> PyValue {
    PyValue::Dict(vec![
        ("id".into(), PyValue::Str(d.id)),
        ("version".into(), PyValue::Int(d.version)),
        ("data".into(), d.data),
    ])
}

struct Driver {
    cwd: PathBuf,
    default_root: PathBuf,
    stores: BTreeMap<String, PyStore>,
    clock: Arc<dyn Clock + Send + Sync>,
}

impl Driver {
    fn answer(&mut self, c: &PyValue) -> Result<String, PyStoreError> {
        let op = s(c, "op");
        match op {
            "new" => {
                let opts = Options {
                    fmt: get(c, "fmt")
                        .filter(|f| !matches!(f, PyValue::Null))
                        .map(py_str),
                    ..Options::default()
                };
                let env = |k: &str| std::env::var(k).ok();
                let store = PyStore::new(
                    s(c, "root"),
                    opts,
                    &env,
                    &self.cwd,
                    self.cwd.join("schemas"),
                    self.clock.clone(),
                )?;
                let name = get(c, "name").map_or_else(|| "s".to_owned(), py_str);
                self.stores.insert(name, store);
                return Ok("ok".into());
            }
            "split" => {
                let (sid, local) = split(s(c, "ref"));
                return Ok(json(&PyValue::List(vec![opt_text(sid), text(local)])));
            }
            "qualify" => {
                let sid = get(c, "sid")
                    .filter(|v| !matches!(v, PyValue::Null))
                    .map(py_str);
                return Ok(json(&text(qualify(sid.as_deref(), s(c, "id")))));
            }
            _ => {}
        }
        let name = get(c, "store").map_or_else(|| "s".to_owned(), py_str);
        let st = self
            .stores
            .get(&name)
            .ok_or_else(|| PyStoreError::Store(format!("KeyError: {}", py_repr(&name))))?;
        let root = self.default_root.as_path();
        let coll = s(c, "coll");
        Ok(match op {
            "store_sid" => json(&opt_text(store_sid(st, root)?)),
            "alias_rules" => {
                let rules = alias_rules(st)?
                    .into_iter()
                    .map(|(f, t)| PyValue::List(vec![text(f), text(t)]));
                json(&PyValue::List(rules.collect()))
            }
            "alias_target" => json(&text(alias_target(st, coll, s(c, "local"))?)),
            "resolve" => json(&text(resolve(st, coll, s(c, "ref"), root)?)),
            "with_qualifier" => {
                let cv = PyValue::Str(coll.into());
                let docs = match get(c, "id") {
                    Some(id) => doc(st.get(&cv, id)?),
                    None => PyValue::List(
                        st.list_docs(&cv, None, None, false, None)?
                            .into_iter()
                            .map(doc)
                            .collect(),
                    ),
                };
                json(&with_qualifier(st, coll, docs, root)?)
            }
            other => return Err(PyStoreError::Store(format!("KeyError: {}", py_repr(other)))),
        })
    }
}

/// Why: the one entry `examples/replay_port.rs dashboard_qual` dispatches to.
/// What: see the module doc.
/// Test: `dashboard_qual_recordings_replay_clean_against_the_port`.
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
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut d = Driver {
        default_root: cwd.join("local").join("owner-queue"),
        cwd,
        stores: BTreeMap::new(),
        clock,
    };
    let mut stdout = String::new();
    for (i, case) in cases.iter().enumerate() {
        stdout.push_str(&format!("case {i} {}\n", s(case, "op")));
        let line = match file_op(case) {
            Some(r) => r.map(|l| l.join("\n  ")),
            None => d.answer(case),
        };
        let line = line.unwrap_or_else(|e| error_line(&e));
        stdout.push_str(&format!("  {line}\n"));
    }
    done(stdout, String::new(), 0)
}
