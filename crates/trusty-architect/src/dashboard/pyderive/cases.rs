//! The replay driver's mirror for `dashboard_derive` (#12029 P2b).
//!
//! Why: the recordings run `tests/fixtures/replay/dashboard_derive/driver.py`
//! against the frozen module; the port replays the same case lists, and its
//! values stay inside the crate.
//! What: [`replay_cases`] reads `CASES.json` and, per `{"op": ...}`, prints
//! `case <i> <op>` and the answer as one `json.dumps` line (a time as
//! `isoformat()`, a tuple as a list, a set sorted). `now` is an ISO string;
//! `retire` without one reads the [`Clock`].
//! Test: `dashboard_derive_recordings_replay_clean_against_the_port`.

use std::path::Path;

use super::counts::{decision_counts_py, run_progress, running_count, tally};
use super::ids::{KEEP_FREE, Retire, free_count, kept_ids, next_id, retire_candidates, used_ids};
use super::order::{acted_on, by_acted_on, goal_sessions, ordered};
use super::pending::{pending, request_queue};
use super::stale::{
    last_activity, last_progress, ranked, stale_parked, stale_tasks, stamp_progress,
};
use super::{PySnap, Time, building_brief, is_parked, is_relayed, is_selftest};
use crate::Clock;
use crate::dashboard::pending::from_iso_parts;
use crate::pyjson_file::read_text;
use crate::pyjson_load::{PyInt, PyValue, loads};
use crate::pyval::{dumps, get, py_str};
use crate::spec_drift::Outcome;
use crate::tasks::store_read::PyDoc;

fn doc(v: &PyValue) -> PyDoc {
    PyDoc {
        id: get(v, "id").map_or(String::new(), py_str),
        data: get(v, "data").cloned().unwrap_or(PyValue::Null),
        version: 1,
    }
}

fn docs(c: &PyValue, k: &str) -> Vec<PyDoc> {
    match get(c, k) {
        Some(PyValue::List(l)) => l.iter().map(doc).collect(),
        _ => Vec::new(),
    }
}

fn snap(c: &PyValue) -> PySnap {
    match get(c, "snap") {
        Some(PyValue::Dict(p)) => p
            .iter()
            .map(|(k, _)| (k.clone(), docs(get(c, "snap").unwrap_or(&PyValue::Null), k)))
            .collect(),
        _ => PySnap::new(),
    }
}

fn num(v: Option<&PyValue>) -> Option<f64> {
    match v {
        Some(PyValue::Int(PyInt::Small(n))) => Some(*n as f64),
        Some(PyValue::Float(f)) => Some(*f),
        _ => None,
    }
}

fn int(n: i64) -> PyValue {
    PyValue::Int(PyInt::Small(n))
}

fn time(t: Option<Time>) -> PyValue {
    t.map_or(PyValue::Null, |t| PyValue::Str(t.iso()))
}

fn strs(v: Vec<String>) -> PyValue {
    PyValue::List(v.into_iter().map(PyValue::Str).collect())
}

fn ids(v: Vec<&PyDoc>) -> PyValue {
    strs(v.into_iter().map(|d| d.id.clone()).collect())
}

fn each(c: &PyValue, f: impl Fn(&PyValue) -> PyValue) -> PyValue {
    match get(c, "data") {
        Some(PyValue::List(l)) => PyValue::List(l.iter().map(f).collect()),
        _ => PyValue::List(Vec::new()),
    }
}

fn answer(c: &PyValue, clock: &dyn Clock) -> Result<PyValue, String> {
    let now = match get(c, "now") {
        Some(PyValue::Str(s)) => from_iso_parts(s).map(|(w, o)| (w - o) as f64 / 1e6),
        _ => None,
    };
    let at = now.unwrap_or(0.0);
    let prefix = get(c, "prefix").map_or(String::new(), py_str);
    let hours = num(get(c, "hours")).unwrap_or(crate::tasks::stale::STALE_HOURS);
    let items = || docs(c, "items");
    let op = get(c, "op").map_or(String::new(), py_str);
    Ok(match op.as_str() {
        "building_brief" => each(c, |d| PyValue::Bool(building_brief(d))),
        "is_parked" => each(c, |d| PyValue::Bool(is_parked(d))),
        "is_relayed" => each(c, |d| PyValue::Bool(is_relayed(d))),
        "is_selftest" => PyValue::List(
            docs(c, "docs")
                .iter()
                .map(|d| PyValue::Bool(is_selftest(d)))
                .collect(),
        ),
        "decision_counts" => decision_counts_py(&items()),
        "tally" => tally(&items()),
        "run_progress" => each(c, |d| run_progress(d, at)),
        "running_count" => int(running_count(&items(), at)),
        "used_ids" => PyValue::List(used_ids(&snap(c), &prefix).into_iter().map(int).collect()),
        "free_count" => int(free_count(&snap(c), &prefix)),
        "next_id" => {
            let last = match get(c, "last_issued") {
                Some(PyValue::Int(PyInt::Small(n))) => Some(*n),
                _ => None,
            };
            next_id(&snap(c), &prefix, last).map_or(PyValue::Null, PyValue::Str)
        }
        "retire" => {
            let keep_free = match get(c, "keep_free") {
                Some(PyValue::Int(PyInt::Small(n))) => *n,
                _ => KEEP_FREE,
            };
            let r = Retire {
                keep_free,
                age_days: num(get(c, "age_days")),
                now,
            };
            strs(retire_candidates(&snap(c), &prefix, r, clock))
        }
        "kept_ids" => strs(kept_ids(&snap(c), &prefix)),
        "last_activity" => each(c, |d| time(last_activity(d))),
        "last_progress" => each(c, |d| time(last_progress(d))),
        "acted_on" => each(c, |d| time(acted_on(d))),
        "stamp_progress" => {
            let old = get(c, "old").filter(|o| !matches!(o, PyValue::Null));
            stamp_progress(old, get(c, "new").unwrap_or(&PyValue::Null), at)
        }
        "stale_tasks" => PyValue::List(
            stale_tasks(&items(), at, hours)?
                .into_iter()
                .map(|(t, id, (session, title))| {
                    PyValue::List(vec![
                        time(t),
                        PyValue::Str(id),
                        PyValue::Str(session),
                        PyValue::Str(title),
                    ])
                })
                .collect(),
        ),
        "stale_parked" => PyValue::List(
            stale_parked(&items(), at, num(get(c, "days")).unwrap_or(7.0))?
                .into_iter()
                .map(|(t, id, ())| PyValue::List(vec![time(t), PyValue::Str(id)]))
                .collect(),
        ),
        "stale_ranked" => PyValue::List(
            ranked(&items(), &docs(c, "goals"), at, hours)?
                .iter()
                .map(|r| r.to_py())
                .collect(),
        ),
        "goal_sessions" => {
            let (g, s) = goal_sessions(&docs(c, "goals"), &docs(c, "sessions"));
            PyValue::List(vec![g, s])
        }
        "ordered" => ids(ordered(&docs(c, "docs"))),
        "by_acted_on" => ids(by_acted_on(&docs(c, "docs"))),
        "pending" => PyValue::List(pending(&snap(c), at)),
        "request_queue" => PyValue::List(request_queue(&snap(c), at)),
        other => return Err(format!("KeyError: {}", crate::pystr::py_repr(other))),
    })
}

/// Why: the one entry `examples/replay_port.rs dashboard_derive` dispatches to.
/// What: see the module doc.
/// Test: `dashboard_derive_recordings_replay_clean_against_the_port`.
pub fn replay_cases(args: &[String], clock: &dyn Clock) -> Outcome {
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
    let mut stdout = String::new();
    for (i, case) in cases.iter().enumerate() {
        stdout.push_str(&format!(
            "case {i} {}\n",
            get(case, "op").map_or(String::new(), py_str)
        ));
        match answer(case, clock) {
            Ok(v) => stdout.push_str(&format!("  {}\n", dumps(&v, None, false))),
            Err(last_line) => return done(stdout, format!("{last_line}\n"), 1),
        }
    }
    done(stdout, String::new(), 0)
}
