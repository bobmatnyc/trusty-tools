//! `dashboard_derive.py`'s activity and staleness rules (#12029 P2b; moved from `tasks::stale`, #12028 P1d).
//!
//! Why: the Architect parks idle tasks; the list it reads must name the
//! same tasks, in the same order, with the same text as the script.
//! What: [`last_activity`] (any touch), [`last_progress`] (real movement:
//! `progress_at`, `log[].at`, `reported.at`, else legacy
//! `progress.estimated_at`, else `created`), [`stamp_progress`],
//! [`stale_tasks`] (open, unparked, non-factory tasks with no progress in
//! `hours`), [`stale_parked`] (open tasks parked over `days` with no
//! `until`) and [`ranked`] (`stale_ranked`: both lists with goal rank, ages,
//! touch hours and flags, sorted by rank, progress time, id). `now` is Unix
//! seconds (UTC). `tasks::stale` prints [`Row`]s.
//! Known gap: a `timedelta` Python refuses (NaN, infinite, past 999,999,999
//! days) and a list or dict `goal` raise in Python; the port returns the
//! traceback's last line.
//! Test: `stale_tasks_recordings_replay_clean_against_the_port`,
//! `dashboard_derive_recordings_replay_clean_against_the_port` (`times`).

use std::collections::HashMap;

use super::{Time, clock_text, dict, newest, open_task, py_eq, text, ts};
use crate::classify::alerts::days_from_civil;
use crate::dashboard::derive::now_micros;
use crate::pyjson_load::{PyInt, PyValue};
use crate::pystr::strip;
use crate::pyval::{get, has, is_str, py_str, truthy, type_name};

const PARK_REVIEW_DAYS: f64 = 7.0;
const US_PER_HOUR: f64 = 3_600_000_000.0;

fn ats<'a>(d: &'a PyValue, k: &str, out: &mut Vec<Option<&'a PyValue>>) {
    if let Some(PyValue::List(l)) = get(d, k) {
        out.extend(
            l.iter()
                .filter(|u| matches!(u, PyValue::Dict(_)))
                .map(|u| get(u, "at")),
        );
    }
}

/// `last_progress(data)`.
pub(crate) fn last_progress(d: &PyValue) -> Option<Time> {
    let mut c = vec![get(d, "progress_at")];
    ats(d, "log", &mut c);
    if let Some(rep) = dict(d, "reported") {
        c.push(get(rep, "at"));
    }
    if let Some(prog) = dict(d, "progress").filter(|_| !has(d, "progress_at")) {
        c.push(get(prog, "estimated_at"));
    }
    newest(c, d)
}

/// `last_activity(data)`.
pub(crate) fn last_activity(d: &PyValue) -> Option<Time> {
    let mut c = vec![get(d, "updated")];
    ats(d, "updates", &mut c);
    if let Some(prog) = dict(d, "progress") {
        c.push(get(prog, "estimated_at"));
    }
    newest(c, d)
}

/// `_moved(old, new)`: progress (`pct`, `eta*`), `status`, `steps` or `outcome` changed.
fn moved(old: &PyValue, new: &PyValue) -> bool {
    let empty = PyValue::Dict(Vec::new());
    let op = dict(old, "progress").unwrap_or(&empty);
    let np = dict(new, "progress").unwrap_or(&empty);
    ["pct", "eta", "eta_low", "eta_high"]
        .iter()
        .any(|k| !py_eq(get(op, k), get(np, k)))
        || ["status", "steps", "outcome"]
            .iter()
            .any(|k| !py_eq(get(old, k), get(new, k)))
}

/// Why: a task's `progress_at` marks real movement, not a touch.
/// What: `stamp_progress(old, new, now)`: `new` with `progress_at` set to `now`
/// (`%Y-%m-%dT%H:%M:%SZ`) when it is a task created or moved by the write;
/// otherwise `new` unchanged.
/// Test: `dashboard_derive_recordings_replay_clean_against_the_port`.
pub(crate) fn stamp_progress(old: Option<&PyValue>, new: &PyValue, now: f64) -> PyValue {
    if !is_str(get(new, "kind"), "task") || old.is_some_and(|o| !moved(o, new)) {
        return new.clone();
    }
    let stamp = PyValue::Str(format!("{}Z", clock_text(now_micros(now), false)));
    let mut pairs = match new {
        PyValue::Dict(p) => p.clone(),
        _ => Vec::new(),
    };
    match pairs.iter_mut().find(|(k, _)| k == "progress_at") {
        Some(slot) => slot.1 = stamp,
        None => pairs.push(("progress_at".into(), stamp)),
    }
    PyValue::Dict(pairs)
}

/// `_flags(d, now)`.
fn flags(d: &PyValue, now: i64) -> Vec<String> {
    let prog = dict(d, "progress");
    let mut out = Vec::new();
    let pct = prog
        .and_then(|p| get(p, "pct"))
        .is_some_and(|v| !matches!(v, PyValue::Null));
    if !pct && !has(d, "steps") {
        out.push("no_progress".into());
    }
    let eta = prog
        .filter(|p| has(p, "eta"))
        .and_then(|p| ts(get(p, "eta")));
    if eta.is_some_and(|e| e.utc < now) {
        out.push("past_eta".into());
    }
    let session = get(d, "session")
        .filter(|v| truthy(v))
        .map_or(String::new(), py_str);
    if strip(&session).is_empty() {
        out.push("no_session".into());
    }
    out
}

/// `timedelta(hours=h)` in microseconds, or the last traceback line.
fn delta_us(units: f64, per: f64) -> Result<i64, String> {
    if units.is_nan() {
        return Err("ValueError: cannot convert float NaN to integer".into());
    }
    if units.is_infinite() {
        return Err("OverflowError: cannot convert float infinity to integer".into());
    }
    let us = (units * per).round_ties_even();
    let days = (us / 86_400_000_000.0).floor();
    if days.abs() > 999_999_999.0 {
        return Err(format!(
            "OverflowError: days={days}; must have magnitude <= 999999999"
        ));
    }
    Ok(us as i64)
}

/// `now - timedelta(...)`, refused outside `datetime`'s range.
fn cutoff(now_us: i64, units: f64, per: f64) -> Result<i64, String> {
    let c = now_us - delta_us(units, per)?;
    let (min, max) = (days_from_civil(1, 1, 1), days_from_civil(10_000, 1, 1));
    if c < min * 86_400_000_000 || c >= max * 86_400_000_000 {
        return Err("OverflowError: date value out of range".into());
    }
    Ok(c)
}

/// `(r[0] is not None, r[0] or now, id)`.
fn by_time<T>(rows: &mut [(Option<Time>, String, T)]) {
    rows.sort_by(|a, b| {
        (a.0.is_some(), a.0.map(|t| t.utc), &a.1).cmp(&(b.0.is_some(), b.0.map(|t| t.utc), &b.1))
    });
}

/// One `stale_tasks` row: `(last_progress, id, (session, title))`.
pub(crate) type StaleRow = (Option<Time>, String, (String, String));

/// Why: the open tasks with no real progress in `hours`.
/// What: `stale_tasks(items, now, hours)`, oldest first, never-moved first of all.
/// Test: `dashboard_derive_recordings_replay_clean_against_the_port`.
///
/// # Errors
/// The traceback's last line for an `hours` Python refuses.
pub(crate) fn stale_tasks(
    items: &[super::PyDoc],
    now: f64,
    hours: f64,
) -> Result<Vec<StaleRow>, String> {
    let now_us = now_micros(now);
    let cut = cutoff(now_us, hours, US_PER_HOUR)?;
    let mut out = Vec::new();
    for doc in items {
        let d = &doc.data;
        if !open_task(d) || has(d, "parked") || dict(d, "factory").is_some() {
            continue;
        }
        let last = last_progress(d);
        if last.is_none_or(|t| t.utc < cut) {
            out.push((last, doc.id.clone(), (text(d, "session"), text(d, "title"))));
        }
    }
    by_time(&mut out);
    Ok(out)
}

/// `stale_parked(items, now, days)`: open tasks parked over `days` ago with no `until`.
///
/// # Errors
/// The traceback's last line for a `days` Python refuses.
pub(crate) fn stale_parked(
    items: &[super::PyDoc],
    now: f64,
    days: f64,
) -> Result<Vec<(Option<Time>, String, ())>, String> {
    let cut = cutoff(now_micros(now), days, 86_400_000_000.0)?;
    let mut out = Vec::new();
    for doc in items {
        let Some(pk) = dict(&doc.data, "parked").filter(|_| open_task(&doc.data)) else {
            continue;
        };
        if has(pk, "until") {
            continue;
        }
        let at = ts(get(pk, "at"));
        if at.is_none_or(|t| t.utc < cut) {
            out.push((at, doc.id.clone(), ()));
        }
    }
    by_time(&mut out);
    Ok(out)
}

/// One `stale_ranked` row.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub(crate) id: String,
    pub(crate) session: String,
    pub(crate) goal: Option<PyValue>,
    pub(crate) rank: Option<PyInt>,
    pub(crate) last: Option<String>,
    pub(crate) age_h: Option<f64>,
    pub(crate) days: Option<f64>,
    pub(crate) touch_h: Option<f64>,
    pub(crate) flags: Vec<String>,
    pub(crate) kind: &'static str,
    pub(crate) title: String,
}

/// `round(micros / 1e6 / per, 1)`.
fn round1(micros: i64, per: f64) -> f64 {
    let x = micros as f64 / 1e6 / per;
    format!("{x:.1}").parse().unwrap_or(x)
}

/// Why: the Architect's idle-task list (`stale_ranked`).
/// What: see the module doc; `now` is Unix seconds.
/// Test: `stale_tasks_recordings_replay_clean_against_the_port`.
///
/// # Errors
/// The last traceback line where Python raises (see the module's known gap).
pub(crate) fn ranked(
    items: &[super::PyDoc],
    goals: &[super::PyDoc],
    now: f64,
    hours: f64,
) -> Result<Vec<Row>, String> {
    let now_us = now_micros(now);
    let mut by_id: HashMap<&str, Option<&PyInt>> = HashMap::new();
    for g in goals {
        let r = match get(&g.data, "rank") {
            Some(PyValue::Int(i)) => Some(i),
            _ => None,
        };
        by_id.insert(&g.id, r);
    }
    let docs: HashMap<&str, &PyValue> = items.iter().map(|d| (d.id.as_str(), &d.data)).collect();
    let row =
        |id: &str, last: Option<Time>, kind: &'static str, extra: &[&str]| -> Result<Row, String> {
            let d = docs.get(id).copied().unwrap_or(&PyValue::Null);
            let goal = get(d, "goal").filter(|v| truthy(v)).cloned();
            let rank = match &goal {
                Some(PyValue::Str(g)) => by_id.get(g.as_str()).copied().flatten().cloned(),
                Some(v @ (PyValue::List(_) | PyValue::Dict(_))) => {
                    return Err(format!("TypeError: unhashable type: '{}'", type_name(v)));
                }
                _ => None,
            };
            let act = last_activity(d);
            let mut f = flags(d, now_us);
            f.extend(extra.iter().map(|s| (*s).to_owned()));
            Ok(Row {
                id: id.to_owned(),
                session: text(d, "session"),
                goal,
                rank,
                last: last.map(|t| format!("{}Z", clock_text(t.wall, false))),
                age_h: last.map(|t| round1(now_us - t.utc, 3600.0)),
                days: last.map(|t| round1(now_us - t.utc, 86_400.0)),
                touch_h: act.map(|t| round1(now_us - t.utc, 3600.0)),
                flags: f,
                kind,
                title: text(d, "title"),
            })
        };
    let mut rows = Vec::new();
    for (last, id, _) in stale_tasks(items, now, hours)? {
        rows.push(row(&id, last, "stale", &[])?);
    }
    for (at, id, ()) in stale_parked(items, now, PARK_REVIEW_DAYS)? {
        rows.push(row(&id, at, "parked", &["park_no_event"])?);
    }
    rows.sort_by(|a, b| {
        let rank = match (&a.rank, &b.rank) {
            (Some(x), Some(y)) => crate::pyval::cmp_int(x, y),
            (x, y) => x.is_none().cmp(&y.is_none()),
        };
        rank.then_with(|| a.last.is_some().cmp(&b.last.is_some()))
            .then_with(|| a.last.cmp(&b.last))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(rows)
}

fn opt_f(x: Option<f64>) -> PyValue {
    x.map_or(PyValue::Null, PyValue::Float)
}

impl Row {
    /// The row as the script's dict, keys in its order.
    pub(crate) fn to_py(&self) -> PyValue {
        let s = |x: &str| PyValue::Str(x.to_owned());
        PyValue::Dict(vec![
            ("id".into(), s(&self.id)),
            ("session".into(), s(&self.session)),
            ("goal".into(), self.goal.clone().unwrap_or(PyValue::Null)),
            (
                "rank".into(),
                self.rank.clone().map_or(PyValue::Null, PyValue::Int),
            ),
            ("last".into(), self.last.as_deref().map_or(PyValue::Null, s)),
            ("age_h".into(), opt_f(self.age_h)),
            ("days".into(), opt_f(self.days)),
            ("touch_h".into(), opt_f(self.touch_h)),
            (
                "flags".into(),
                PyValue::List(self.flags.iter().map(|f| s(f)).collect()),
            ),
            ("kind".into(), s(self.kind)),
            ("title".into(), s(&self.title)),
        ])
    }
}
