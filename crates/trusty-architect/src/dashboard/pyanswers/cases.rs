//! The replay driver's mirror for `owner_answers` (#12029 P2e).
//!
//! Why: the recordings run `tests/fixtures/replay/owner_answers/driver.py`
//! against the frozen modules; the port replays the same case lists.
//! What: [`replay_cases`] runs each `{"op": ...}` of `CASES.json` and prints
//! `case <i> <op>` and the answer, one `json.dumps` line per event, or the
//! exception in the driver's form; `main` runs [`super::cli::run`] and
//! prints `exit <code>`. The default root is `local/owner-queue` under the
//! working directory, the replay sandbox; `env` edits the environment the
//! store and the command read.
//! Test: `owner_answers_recordings_replay_clean_against_the_port`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{ILIO_ACTOR, cli, pending, qualified, resolve_id};
use crate::clock::Clock;
use crate::dashboard::pending::from_iso_parts;
use crate::dashboard::pyids::env_op;
use crate::dashboard::store::py::{Options, PyStore, PyStoreError, error_line, file_op};
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
    dumps(v, None, false)
}

struct Driver<'a> {
    cwd: PathBuf,
    default_root: PathBuf,
    env: &'a dyn Fn(&str) -> Option<String>,
    clock: Arc<dyn Clock + Send + Sync>,
    stderr: String,
}

impl Driver<'_> {
    /// `ds.Store(root or ds.DEFAULT_ROOT)`.
    fn store(&self, c: &PyValue) -> Result<PyStore, PyStoreError> {
        let root = match get(c, "root") {
            Some(PyValue::Str(r)) if !r.is_empty() => r.clone(),
            _ => self.default_root.to_string_lossy().into_owned(),
        };
        PyStore::new(
            &root,
            Options::default(),
            self.env,
            &self.cwd,
            self.cwd.join("schemas"),
            self.clock.clone(),
        )
    }

    fn answer(&mut self, c: &PyValue, out: &mut String) -> Result<Vec<String>, PyStoreError> {
        let lines = |v: Vec<PyValue>| v.iter().map(json).collect();
        Ok(match s(c, "op") {
            "pending" => {
                let now = match from_iso_parts(s(c, "now")) {
                    Some((wall, off)) => (wall - off) as f64 / 1e6,
                    None => self.clock.now_unix(),
                };
                let trial = (self.env)("DASHBOARD_ACTOR").as_deref() == Some(ILIO_ACTOR);
                lines(pending(&self.store(c)?, now, trial)?)
            }
            "resolve_id" => {
                let id = resolve_id(&self.store(c)?, s(c, "ref"), &self.default_root)?;
                vec![json(&PyValue::Str(id))]
            }
            "qualified" => {
                let entries = match get(c, "entries") {
                    Some(PyValue::List(l)) => l.clone(),
                    _ => Vec::new(),
                };
                lines(qualified(&self.store(c)?, entries, &self.default_root)?)
            }
            "main" => {
                let argv: Vec<String> = match get(c, "argv") {
                    Some(PyValue::List(l)) => l.iter().map(py_str).collect(),
                    _ => Vec::new(),
                };
                let o = cli::run(
                    &argv,
                    &cli::Options {
                        prog: "pending-answers.py".into(),
                        default_root: self.default_root.clone(),
                        cwd: self.cwd.clone(),
                        env: self.env,
                        schema_dir: self.cwd.join("schemas"),
                        clock: self.clock.clone(),
                    },
                );
                out.push_str(&o.stdout);
                self.stderr.push_str(&o.stderr);
                vec![format!("exit {}", o.code)]
            }
            other => return Err(PyStoreError::Store(format!("KeyError: {}", py_repr(other)))),
        })
    }
}

/// Why: the one entry `examples/replay_port.rs owner_answers` dispatches to.
/// What: see the module doc; `clock` is the recording's frozen clock.
/// Test: `owner_answers_recordings_replay_clean_against_the_port`.
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
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    let (mut stdout, mut stderr) = (String::new(), String::new());
    for (i, case) in cases.iter().enumerate() {
        let op = s(case, "op");
        stdout.push_str(&format!("case {i} {op}\n"));
        let lines = if op == "env" {
            env_op(case, &mut env);
            vec!["ok".to_owned()]
        } else if let Some(r) = file_op(case) {
            r.unwrap_or_else(|e| vec![error_line(&e)])
        } else {
            let lookup = |k: &str| env.get(k).cloned();
            let mut d = Driver {
                default_root: cwd.join("local").join("owner-queue"),
                cwd: cwd.clone(),
                env: &lookup,
                clock: clock.clone(),
                stderr: String::new(),
            };
            let lines = d
                .answer(case, &mut stdout)
                .unwrap_or_else(|e| vec![error_line(&e)]);
            stderr.push_str(&d.stderr);
            lines
        };
        for line in lines {
            stdout.push_str(&format!("  {line}\n"));
        }
    }
    done(stdout, stderr, 0)
}
