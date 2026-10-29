//! `last_activity` and `stale_tasks` from `dashboard_derive.py`.
//!
//! Why: an open task nobody touched for a day needs a nudge; the rule that
//! decides "touched" must match the prototype's (#10403).
//! What: [`last_activity`] is the newest parseable of `updated`, each
//! `updates[].at` and `progress.estimated_at`, else `created`, as UTC
//! microseconds. [`stale_tasks`] lists open, unparked tasks idle at least
//! `hours`, undated first, then oldest first, then by id. A task whose
//! timestamps do not parse looks stale (Python's behaviour) and each
//! unparseable field is listed in [`StaleTasks::skipped`].
//! Test: `pending_parity_goldens`, `stale_unparseable_updated_is_skipped`.

use serde_json::{Map, Value};

use super::super::derive::now_micros;
use super::isotime::ts;
use super::{SkippedField, obj, s, t};
use crate::dashboard::store::Doc;

/// One stale task, Python's `(last_activity, id, session, title)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleTask {
    /// Last activity, UTC microseconds since the epoch; `None` when undated.
    pub last: Option<i64>,
    /// Item id.
    pub id: String,
    /// `str(session)`.
    pub session: String,
    /// `str(title)`.
    pub title: String,
}

/// [`stale_tasks`]'s result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StaleTasks {
    tasks: Vec<StaleTask>,
    skipped: Vec<SkippedField>,
}

impl StaleTasks {
    /// The stale tasks, in Python's order.
    pub fn tasks(&self) -> &[StaleTask] {
        &self.tasks
    }

    /// Present timestamps that did not parse; not part of the parity output.
    pub fn skipped(&self) -> &[SkippedField] {
        &self.skipped
    }
}

/// Why: the stale rule and the page's "last touched" read one value.
/// What: the newest parseable activity timestamp, else `created`, as UTC
/// microseconds; `None` when nothing parses.
/// Test: `pending_parity_goldens`.
pub fn last_activity(data: &Map<String, Value>) -> Option<i64> {
    activity(data, &mut |_| {})
}

/// [`last_activity`], reporting each present field that did not parse.
fn activity(data: &Map<String, Value>, bad: &mut dyn FnMut(String)) -> Option<i64> {
    let mut cands: Vec<(String, Option<&Value>)> = vec![("updated".into(), data.get("updated"))];
    if let Some(ups) = data.get("updates").and_then(Value::as_array) {
        for (i, u) in ups.iter().enumerate() {
            if let Some(u) = u.as_object() {
                cands.push((format!("updates[{i}].at"), u.get("at")));
            }
        }
    }
    if let Some(p) = obj(data, "progress") {
        cands.push(("progress.estimated_at".into(), p.get("estimated_at")));
    }
    let mut parse = |(field, v): (String, Option<&Value>)| {
        // `_ts(None)` fails too; only a present, non-null value is a fault.
        let got = ts(v.unwrap_or(&Value::Null));
        if got.is_none() && v.is_some_and(|v| !v.is_null()) {
            bad(field);
        }
        got
    };
    let newest = cands.into_iter().filter_map(&mut parse).max();
    newest.or_else(|| parse(("created".into(), data.get("created"))))
}

/// Why: the Architect nudges idle tasks; Python's `stale_tasks(items, now,
/// hours)` is the reference (#10403).
/// What: open items of kind `task` with a falsy `parked` whose
/// [`last_activity`] is missing or older than `now - hours` (`now` in Unix
/// seconds), sorted undated first, then oldest, then by id.
/// Test: `pending_parity_goldens`, `stale_unparseable_updated_is_skipped`.
pub fn stale_tasks(items: &[Doc], now: f64, hours: f64) -> StaleTasks {
    let now_us = now_micros(now);
    let cutoff = now_us - (hours * 3_600_000_000.0).round() as i64;
    let mut out = StaleTasks::default();
    for doc in items {
        let d = &doc.data;
        let is = |k: &str, v: &str| d.get(k).and_then(Value::as_str) == Some(v);
        if !is("kind", "task") || !is("status", "open") || t(d, "parked") {
            continue;
        }
        let id = doc.id.clone();
        let last = activity(d, &mut |field| {
            out.skipped.push(SkippedField {
                id: id.clone(),
                field,
            });
        });
        if last.is_none_or(|l| l < cutoff) {
            out.tasks.push(StaleTask {
                last,
                id,
                session: s(d, "session"),
                title: s(d, "title"),
            });
        }
    }
    out.tasks.sort_by(|a, b| {
        let key = |x: &StaleTask| (x.last.is_some(), x.last.unwrap_or(now_us));
        key(a).cmp(&key(b)).then_with(|| a.id.cmp(&b.id))
    });
    out
}
