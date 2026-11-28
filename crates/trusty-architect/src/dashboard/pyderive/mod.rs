//! The port of `dashboard_derive.py`: every value derived from the dashboard store (#12029 P2b).
//!
//! Why: the frozen Python owns the rules for "open", "parked", the next id,
//! stale tasks and the owner events that wake the Architect; the port must
//! give the same answer for every document shape, so it works on
//! [`PyValue`]s, which keep Python's types and key order. The live
//! [`super::ids`] and [`super::derive::is_parked`] run these rules (#12029
//! P2d), as do [`super::tally`](mod@super::tally) and the poller's owner
//! events through [`super::pyanswers`] (#12029 P2e); the rest of
//! [`super::derive`] still serves the live verbs with its own rules (see #12029).
//! What: the predicates here ([`building_brief`], [`is_parked`],
//! [`is_selftest`], [`is_relayed`]) and the shared time and equality helpers;
//! `counts` (`decision_counts`, `tally`, `run_progress`, `running_count`),
//! `ids` (base-34 through [`crate::ids36`]), `order` (`goal_sessions`,
//! `ordered`, `acted_on`, `by_acted_on`), `stale` (`last_*`,
//! `stamp_progress`, `stale_tasks`, `stale_parked`, `stale_ranked`) and
//! `pending` (`pending`, `request_queue`). No function reads the clock or the
//! filesystem except `retire_candidates` without a `now`, which reads the
//! [`crate::Clock`] it is given. `snapshot(store, colls)` is the caller's
//! `list_docs` loop and is not ported.
//! Test: `dashboard_derive_recordings_replay_clean_against_the_port`.

mod cases;
pub(crate) mod counts; // #12029 P2e: the live `dashboard::tally` runs these rules
pub(crate) mod ids; // #12029 P2d: the live `dashboard::ids` runs these rules
mod order;
pub(crate) mod pending; // #12029 P2e: the poller and `pending-answers` run these rules
pub(crate) mod stale;

use std::collections::BTreeMap;

use crate::classify::alerts::civil_from_days;
use crate::dashboard::pending::from_iso_parts;
use crate::pyjson_load::{PyInt, PyValue};
use crate::pystr::{lstrip, strip};
use crate::pyval::{get, has, is_str, py_str, truthy};
use crate::tasks::store_read::PyDoc;

pub use cases::replay_cases;

/// `{coll: [doc]}`, as `snapshot(store, colls)` returns it.
pub(crate) type PySnap = BTreeMap<String, Vec<PyDoc>>;

/// `snap.get(name, [])`.
pub(crate) fn coll<'a>(snap: &'a PySnap, name: &str) -> &'a [PyDoc] {
    snap.get(name).map_or(&[], Vec::as_slice)
}

/// An aware time: UTC and wall-clock microseconds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Time {
    pub(crate) utc: i64,
    pub(crate) wall: i64,
}

impl Time {
    /// `datetime.isoformat()`: microseconds when non-zero, then the offset.
    pub(crate) fn iso(self) -> String {
        let mut out = clock_text(self.wall, true);
        let off = self.wall - self.utc;
        let (sign, a) = if off < 0 { ('-', -off) } else { ('+', off) };
        let secs = a / 1_000_000;
        out.push_str(&format!("{sign}{:02}:{:02}", secs / 3600, secs / 60 % 60));
        if a % 60_000_000 != 0 {
            out.push_str(&format!(":{:02}", secs % 60));
        }
        if a % 1_000_000 != 0 {
            out.push_str(&format!(".{:06}", a % 1_000_000));
        }
        out
    }
}

/// `YYYY-MM-DDTHH:MM:SS` of wall-clock microseconds, with `.ffffff` when
/// `micros` and the fraction is non-zero.
pub(crate) fn clock_text(wall: i64, micros: bool) -> String {
    let secs = wall.div_euclid(1_000_000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    let mut out = format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    );
    let frac = wall.rem_euclid(1_000_000);
    if micros && frac != 0 {
        out.push_str(&format!(".{frac:06}"));
    }
    out
}

/// `_ts(value)`: `fromisoformat(str(value).replace("Z", "+00:00"))`, naive read as UTC.
pub(crate) fn ts(v: Option<&PyValue>) -> Option<Time> {
    let text = v
        .map_or_else(|| "None".to_owned(), py_str)
        .replace('Z', "+00:00");
    from_iso_parts(&text).map(|(wall, offset)| Time {
        utc: wall - offset,
        wall,
    })
}

/// `max(times)` (the first of equal instants), else `_ts(created)`.
pub(crate) fn newest(cands: Vec<Option<&PyValue>>, d: &PyValue) -> Option<Time> {
    let mut best: Option<Time> = None;
    for t in cands.into_iter().filter_map(ts) {
        if best.is_none_or(|b| t.utc > b.utc) {
            best = Some(t);
        }
    }
    best.or_else(|| ts(get(d, "created")))
}

/// `d.get(k)` when it is a dict.
pub(crate) fn dict<'a>(d: &'a PyValue, k: &str) -> Option<&'a PyValue> {
    get(d, k).filter(|v| matches!(v, PyValue::Dict(_)))
}

/// `d.get(k)` when it is a list: its elements.
pub(crate) fn list<'a>(d: &'a PyValue, k: &str) -> &'a [PyValue] {
    match get(d, k) {
        Some(PyValue::List(l)) => l,
        _ => &[],
    }
}

/// `str(d.get(k, ""))`.
pub(crate) fn text(d: &PyValue, k: &str) -> String {
    get(d, k).map_or(String::new(), py_str)
}

/// `str(d.get(k) or "")`.
pub(crate) fn text_or(d: &PyValue, k: &str) -> String {
    get(d, k)
        .filter(|v| truthy(v))
        .map_or(String::new(), py_str)
}

/// A Python `int` (not a `bool`).
pub(crate) fn int_of(v: Option<&PyValue>) -> Option<&PyInt> {
    match v {
        Some(PyValue::Int(i)) => Some(i),
        _ => None,
    }
}

/// `a == b` with Python's rules: `None` for a missing value, `True == 1 ==
/// 1.0`, dicts equal whatever their key order.
pub(crate) fn py_eq(a: Option<&PyValue>, b: Option<&PyValue>) -> bool {
    let null = PyValue::Null;
    let (a, b) = (a.unwrap_or(&null), b.unwrap_or(&null));
    let num = |v: &PyValue| match v {
        PyValue::Bool(x) => Some(f64::from(u8::from(*x))),
        PyValue::Int(PyInt::Small(n)) => Some(*n as f64),
        PyValue::Float(f) => Some(*f),
        _ => None,
    };
    match (a, b) {
        (PyValue::Int(PyInt::Small(x)), PyValue::Int(PyInt::Small(y))) => x == y,
        (PyValue::Int(PyInt::Big(x)), PyValue::Int(PyInt::Big(y))) => x == y,
        (PyValue::List(x), PyValue::List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(Some(p), Some(q)))
        }
        (PyValue::Dict(x), PyValue::Dict(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| has_key(y, k) && py_eq(Some(v), get(b, k)))
        }
        _ => match (num(a), num(b)) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        },
    }
}

fn has_key(pairs: &[(String, PyValue)], k: &str) -> bool {
    pairs.iter().any(|(x, _)| x == k)
}

/// `building_brief(data)`: a `brief_request` with `at`, and no `brief` yet or `refresh` true.
pub(crate) fn building_brief(d: &PyValue) -> bool {
    dict(d, "brief_request").is_some_and(|req| {
        has(req, "at") && (!has(d, "brief") || get(req, "refresh") == Some(&PyValue::Bool(true)))
    })
}

/// `is_parked(data)`: an open item parked by a flag, a note, a wake date or a
/// `clarify` / `stale` / `indefinite` choice.
pub(crate) fn is_parked(d: &PyValue) -> bool {
    let Some(pk) = get(d, "parked").filter(|p| truthy(p)) else {
        return false;
    };
    if !is_str(get(d, "status"), "open") {
        return false;
    }
    !matches!(pk, PyValue::Dict(_))
        || has(pk, "until")
        || ["clarify", "stale", "indefinite"]
            .iter()
            .any(|c| is_str(get(pk, "choice"), c))
}

/// `is_selftest(doc)`: `selftest` true, or a title starting "selftest canary".
pub(crate) fn is_selftest(doc: &PyDoc) -> bool {
    get(&doc.data, "selftest") == Some(&PyValue::Bool(true))
        || text_or(&doc.data, "title")
            .to_lowercase()
            .starts_with("selftest canary")
}

/// `_relayed(data)`: `relayed` true or a non-blank string, or a non-blank `relayed_at`.
pub(crate) fn relayed(d: &PyValue) -> bool {
    let blank_str =
        |v: Option<&PyValue>| matches!(v, Some(PyValue::Str(s)) if !strip(s).is_empty());
    get(d, "relayed") == Some(&PyValue::Bool(true))
        || blank_str(get(d, "relayed"))
        || blank_str(get(d, "relayed_at"))
}

/// `_done(note, prefixes)`: a string note that starts with a prefix once left-stripped.
pub(crate) fn done(note: Option<&PyValue>, prefixes: &[&str]) -> bool {
    matches!(note, Some(PyValue::Str(n)) if prefixes.iter().any(|p| lstrip(n).starts_with(p)))
}

/// `is_relayed(data)`: status `relayed`, [`relayed`], or an answer note starting "Relayed".
pub(crate) fn is_relayed(d: &PyValue) -> bool {
    is_str(get(d, "status"), "relayed") || relayed(d) || done(get(d, "answer_note"), &["Relayed"])
}

/// `_open_task(d)`.
pub(crate) fn open_task(d: &PyValue) -> bool {
    is_str(get(d, "kind"), "task") && is_str(get(d, "status"), "open")
}
