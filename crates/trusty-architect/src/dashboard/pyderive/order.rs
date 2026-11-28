//! `dashboard_derive.py`'s orders and links: `goal_sessions`, `ordered`, `acted_on`, `by_acted_on` (#12029 P2b).
//!
//! Why: the page and the server list goals, sessions and tasks in one order.
//! What: [`goal_sessions`] (links both ways; removed, done and parked goals
//! skipped; a `sessions` string iterates its characters, as Python does),
//! [`ordered`] (integer `rank` first, then id; a `bool` rank is no rank),
//! [`acted_on`] (the newest of the touch fields, else `created`) and
//! [`by_acted_on`] (most recent first, no time last, ties in input order).
//! Test: `dashboard_derive_recordings_replay_clean_against_the_port` (`times`).

use std::cmp::Ordering;

use super::{Time, dict, int_of, list, newest, py_eq};
use crate::poller::pyfmt::py_float_repr;
use crate::pyjson_load::PyValue;
use crate::pyval::{cmp_int, get, int_text, is_str};
use crate::tasks::store_read::PyDoc;

/// The order `sorted()` puts two linked names in: numbers by value, strings by code point.
fn name_cmp(a: &PyValue, b: &PyValue) -> Ordering {
    let num = |v: &PyValue| match v {
        PyValue::Bool(x) => Some(f64::from(u8::from(*x))),
        PyValue::Int(i) => Some(int_text(i).parse::<f64>().unwrap_or(0.0)),
        PyValue::Float(f) => Some(*f),
        _ => None,
    };
    match (a, b, num(a), num(b)) {
        (PyValue::Str(x), PyValue::Str(y), _, _) => x.cmp(y),
        (_, _, Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        _ => Ordering::Equal,
    }
}

/// A dict key as `json.dumps` writes it.
fn key_text(v: &PyValue) -> String {
    match v {
        PyValue::Str(s) => s.clone(),
        PyValue::Null => "null".into(),
        PyValue::Bool(b) => if *b { "true" } else { "false" }.into(),
        PyValue::Int(i) => int_text(i),
        PyValue::Float(f) => py_float_repr(*f),
        other => crate::pyval::repr(other),
    }
}

fn py_iter(v: Option<&PyValue>) -> Vec<PyValue> {
    match v {
        Some(PyValue::List(l)) => l.clone(),
        Some(PyValue::Str(s)) => s.chars().map(|c| PyValue::Str(c.into())).collect(),
        Some(PyValue::Dict(p)) => p.iter().map(|(k, _)| PyValue::Str(k.clone())).collect(),
        _ => Vec::new(),
    }
}

/// Why: the page groups sessions under goals.
/// What: `goal_sessions(goals, sessions)` as the two Python dicts, in insertion order.
/// Test: `dashboard_derive_recordings_replay_clean_against_the_port`.
pub(crate) fn goal_sessions(goals: &[PyDoc], sessions: &[PyDoc]) -> (PyValue, PyValue) {
    let names: Vec<PyValue> = sessions
        .iter()
        .map(|s| get(&s.data, "name").cloned().unwrap_or(PyValue::Null))
        .collect();
    let mut by_goal: Vec<(String, PyValue)> = Vec::new();
    let mut by_session: Vec<(PyValue, Vec<String>)> = Vec::new();
    for g in goals {
        let life = get(&g.data, "lifecycle");
        if ["removed", "done", "parked"]
            .iter()
            .any(|l| is_str(life, l))
        {
            continue;
        }
        let linked: Vec<PyValue> = py_iter(get(&g.data, "sessions"))
            .into_iter()
            .filter(|n| names.iter().any(|m| py_eq(Some(m), Some(n))))
            .collect();
        for n in &linked {
            match by_session.iter_mut().find(|(k, _)| py_eq(Some(k), Some(n))) {
                Some((_, v)) => v.push(g.id.clone()),
                None => by_session.push((n.clone(), vec![g.id.clone()])),
            }
        }
        let mut sorted = linked;
        sorted.sort_by(name_cmp);
        match by_goal.iter_mut().find(|(k, _)| *k == g.id) {
            Some(slot) => slot.1 = PyValue::List(sorted),
            None => by_goal.push((g.id.clone(), PyValue::List(sorted))),
        }
    }
    let by_session = by_session
        .into_iter()
        .map(|(k, mut v)| {
            v.sort();
            (
                key_text(&k),
                PyValue::List(v.into_iter().map(PyValue::Str).collect()),
            )
        })
        .collect();
    (PyValue::Dict(by_goal), PyValue::Dict(by_session))
}

/// `ordered(docs)`: integer rank (missing last), then id; stable.
pub(crate) fn ordered(docs: &[PyDoc]) -> Vec<&PyDoc> {
    let mut out: Vec<&PyDoc> = docs.iter().collect();
    out.sort_by(|a, b| {
        match (int_of(get(&a.data, "rank")), int_of(get(&b.data, "rank"))) {
            (Some(x), Some(y)) => cmp_int(x, y),
            (x, y) => x.is_none().cmp(&y.is_none()),
        }
        .then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// Why: the task list puts the most recently acted-on task first.
/// What: `acted_on(data)`: the newest of `last_pulse_at`, `last_signal_at`,
/// `status_at`, `relayed_at`, `updated`, `progress.estimated_at` and each
/// `steer_requests[].relayed_at`; else `created`.
/// Test: `dashboard_derive_recordings_replay_clean_against_the_port`.
pub(crate) fn acted_on(d: &PyValue) -> Option<Time> {
    let mut c: Vec<Option<&PyValue>> = [
        "last_pulse_at",
        "last_signal_at",
        "status_at",
        "relayed_at",
        "updated",
    ]
    .iter()
    .map(|k| get(d, k))
    .collect();
    if let Some(prog) = dict(d, "progress") {
        c.push(get(prog, "estimated_at"));
    }
    c.extend(
        list(d, "steer_requests")
            .iter()
            .filter(|s| matches!(s, PyValue::Dict(_)))
            .map(|s| get(s, "relayed_at")),
    );
    newest(c, d)
}

/// `by_acted_on(docs)`: most recently acted on first; no usable time last; stable.
pub(crate) fn by_acted_on(docs: &[PyDoc]) -> Vec<&PyDoc> {
    let mut stamped: Vec<(Option<i64>, &PyDoc)> = docs
        .iter()
        .map(|doc| (acted_on(&doc.data).map(|t| t.utc), doc))
        .collect();
    stamped.sort_by_key(|s| std::cmp::Reverse(s.0));
    stamped.into_iter().map(|(_, d)| d).collect()
}
