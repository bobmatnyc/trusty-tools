//! The replay driver's mirror for the questions ports (#12029 P2f).
//!
//! Why: the recordings run `tests/fixtures/replay/questions_rc/driver.py`
//! beside the frozen scripts; the port replays the same three modes.
//! What: [`replay`] takes `rc ARGS...` (the questions pass), `start ARGS...`
//! (the launcher, `$0` = `<cwd>/scripts/start-architect-questions.sh`) or
//! `cases CASES.json` (the pure functions, one `{"op": ...}` per case,
//! printed as `case <i> <op>` and indented answers). The sandbox root, the
//! working directory, stands for the checkout; the action scripts run by bare
//! name; tmux is `tmux` on `PATH` (the recording's fake), always on the
//! private socket [`REPLAY_SOCKET`].
//! Test: `questions_rc_recordings_replay_clean_against_the_port`,
//! `start_recordings_replay_clean_against_the_port`,
//! `questions_cases_replay_clean_against_the_port`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::rc::{self, Action, decide, open_ids, rc_from_capture, settle, url_from_capture};
use super::{Socket, TmuxCli, start};
use std::time::Duration;

use crate::clock::{Clock, FakeClock};
use crate::dashboard::store::py::{error_line, file_op};
use crate::pyjson_file::read_text;
use crate::pyjson_load::{PyInt, PyValue, loads};
use crate::pystr::splitlines;
use crate::pyval::{dumps, get, py_str};
use crate::spec_drift::Outcome;

/// The private tmux socket every replay call names (Architect ruling C2).
pub const REPLAY_SOCKET: &str = "trusty-replay-p2f";

fn out(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

fn s<'a>(c: &'a PyValue, k: &str) -> &'a str {
    match get(c, k) {
        Some(PyValue::Str(v)) => v,
        _ => "",
    }
}

fn flag(c: &PyValue, k: &str) -> bool {
    matches!(get(c, k), Some(PyValue::Bool(true)))
}

fn strs(c: &PyValue, k: &str) -> Vec<String> {
    match get(c, k) {
        Some(PyValue::List(l)) => l.iter().map(py_str).collect(),
        _ => Vec::new(),
    }
}

fn json(v: PyValue) -> String {
    dumps(&v, None, false)
}

fn list(items: impl IntoIterator<Item = PyValue>) -> PyValue {
    PyValue::List(items.into_iter().collect())
}

fn text_list(items: &[String]) -> PyValue {
    list(items.iter().map(|i| PyValue::Str(i.clone())))
}

/// `{id: status}` with non-string statuses as `None`.
fn statuses(c: &PyValue) -> BTreeMap<String, Option<String>> {
    match get(c, "statuses") {
        Some(PyValue::Dict(pairs)) => pairs
            .iter()
            .map(|(k, v)| {
                let st = match v {
                    PyValue::Str(s) => Some(s.clone()),
                    _ => None,
                };
                (k.clone(), st)
            })
            .collect(),
        _ => BTreeMap::new(),
    }
}

fn action_of(name: &str) -> Action {
    match name {
        "start" => Action::Start,
        "rc_on" => Action::RcOn,
        "rc_off" => Action::RcOff,
        _ => Action::None,
    }
}

/// One `cases` op; `Err` holds the driver's exception line.
fn answer(c: &PyValue, clock: &dyn Clock) -> Result<Vec<String>, String> {
    let int = |k: &str| match get(c, k) {
        Some(PyValue::Int(PyInt::Small(n))) => usize::try_from(*n).unwrap_or(0),
        _ => 0,
    };
    let capture = |lines: &[&str]| {
        json(list([
            PyValue::Bool(rc_from_capture(lines)),
            PyValue::Str(url_from_capture(lines)),
        ]))
    };
    Ok(match s(c, "op") {
        "open_ids" => vec![json(text_list(&open_ids(&statuses(c), &strs(c, "queued"))))],
        "decide" => vec![
            decide(int("count"), flag(c, "up"), flag(c, "rc"))
                .as_str()
                .into(),
        ],
        "lines" => {
            let lines = strs(c, "lines");
            vec![capture(
                &lines.iter().map(String::as_str).collect::<Vec<_>>(),
            )]
        }
        "pane" => vec![capture(&splitlines(s(c, "text")))],
        "settle" => {
            let seq: Vec<(bool, bool)> = match get(c, "seq") {
                Some(PyValue::List(l)) => l
                    .iter()
                    .map(|p| match p {
                        PyValue::List(p) => (
                            matches!(p.first(), Some(PyValue::Bool(true))),
                            matches!(p.get(1), Some(PyValue::Bool(true))),
                        ),
                        _ => (false, false),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let reads = std::cell::Cell::new(0);
            let observe = || {
                let i = reads.get();
                reads.set(i + 1);
                Ok(seq.get(i).copied().unwrap_or((false, false)))
            };
            let fake = FakeClock::new(0.0);
            let tries = if get(c, "tries").is_some() {
                int("tries")
            } else {
                4
            };
            let (up, rc) = settle(action_of(s(c, "action")), observe, &fake, tries)
                .map_err(|e| e.to_string())?;
            let n = |v: usize| PyValue::Int(PyInt::Small(i64::try_from(v).unwrap_or(i64::MAX)));
            vec![json(list([
                PyValue::Bool(up),
                PyValue::Bool(rc),
                n(fake.sleeps().len()),
                n(reads.get()),
            ]))]
        }
        "step" => {
            let ids = open_ids(&statuses(c), &strs(c, "queued"));
            let action = decide(ids.len(), flag(c, "up"), flag(c, "rc"));
            let ran = !flag(c, "dry") && action != Action::None;
            let call = list([PyValue::Str(action.as_str().into()), text_list(&ids)]);
            let calls = list(ran.then_some(call));
            let count = PyValue::Int(PyInt::Small(i64::try_from(ids.len()).unwrap_or(i64::MAX)));
            vec![json(list([
                PyValue::Str(action.as_str().into()),
                count,
                calls,
            ]))]
        }
        "write_state" => {
            let doc = rc::write_state(
                flag(c, "rc"),
                s(c, "url"),
                int("count"),
                Path::new(s(c, "path")),
                clock,
            )
            .map_err(|_| "OSError".to_owned())?;
            vec![doc]
        }
        "queue" => match rc::read_queue(Path::new(s(c, "path"))) {
            Ok(q) => vec![json(text_list(&q))],
            Err(_) => return Err("UnicodeDecodeError".into()),
        },
        other => return Err(format!("KeyError: {other:?}")),
    })
}

/// The pure-function cases of `driver.py cases CASES.json`.
fn cases(args: &[String], clock: &dyn Clock) -> Outcome {
    let [path] = args else {
        return out(
            String::new(),
            "usage: driver.py cases CASES.json\n".into(),
            2,
        );
    };
    let cases = match read_text(Path::new(path)).map(|t| loads(&t)) {
        Ok(Ok(PyValue::List(cases))) => cases,
        _ => return out(String::new(), format!("cannot read {path}\n"), 1),
    };
    let mut stdout = String::new();
    for (i, case) in cases.iter().enumerate() {
        stdout.push_str(&format!("case {i} {}\n", s(case, "op")));
        let lines = match file_op(case) {
            Some(r) => r.unwrap_or_else(|e| vec![error_line(&e)]),
            None => answer(case, clock).unwrap_or_else(|e| vec![e]),
        };
        for line in lines {
            stdout.push_str(&format!("  {line}\n"));
        }
    }
    out(stdout, String::new(), 0)
}

/// The recording's instant, which no sleep moves: the driver freezes the
/// clock and makes `settle`'s sleeps no-ops.
struct Frozen(f64);

impl Clock for Frozen {
    fn now_unix(&self) -> f64 {
        self.0
    }

    fn sleep(&self, _: Duration) {}
}

/// Why: the one entry `examples/replay_port.rs questions_rc` dispatches to.
/// What: see the module doc; `at` is the recording's frozen instant.
/// Test: `questions_rc_recordings_replay_clean_against_the_port`.
pub fn replay(args: &[String], at: f64) -> Outcome {
    let clock: Arc<dyn Clock + Send + Sync> = Arc::new(Frozen(at));
    let usage = "usage: driver.py rc ARGS... | start ARGS... | cases CASES.json\n";
    let Some((mode, rest)) = args.split_first() else {
        return out(String::new(), usage.into(), 2);
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let env = |k: &str| std::env::var(k).ok();
    let tmux = TmuxCli::on_path_with_socket(Socket::Name(REPLAY_SOCKET.into()));
    let rc_opts = rc::Options {
        root: cwd.clone(),
        scripts: PathBuf::new(),
        store_root: cwd.join("local").join("owner-queue"),
        schema_dir: cwd.join("schemas"),
        cwd: cwd.clone(),
        env: &env,
        tmux: tmux.clone(),
        clock: clock.clone(),
    };
    match mode.as_str() {
        "rc" => rc::run(rest, &rc_opts),
        "start" => {
            let in_process = || rc::run(&[], &rc_opts);
            let prog = cwd.join("scripts").join("start-architect-questions.sh");
            start::run(
                rest,
                &start::Options {
                    prog: prog.to_string_lossy().into_owned(),
                    root: cwd.clone(),
                    env: &env,
                    tmux,
                    clock: clock.clone(),
                    rc: &in_process,
                },
            )
        }
        "cases" => cases(rest, clock.as_ref()),
        _ => out(String::new(), usage.into(), 2),
    }
}
