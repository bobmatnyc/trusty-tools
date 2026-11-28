//! `dashboard_derive.py`'s counts and run windows (#12029 P2b).
//!
//! Why: the footer, `dashboard-db.py summary` and the page share one
//! decision count, and an `interactive` item stays open until its
//! answer is written. See #12029.
//! What: [`decision_counts`], [`tally`] (the `interactive` key only when
//! non-zero), [`run_progress`] and [`running_count`]. `now` is Unix seconds
//! (UTC); a run date is `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM[:SS]Z` with any
//! Unicode digit, as `re`'s `\d` and `int()` read them.
//! Test: `dashboard_derive_recordings_replay_clean_against_the_port` (`predicates`).

use super::{is_parked, is_relayed, is_selftest};
use crate::dashboard::derive::{iso_date, now_micros, run_time};
use crate::pyjson_load::{PyInt, PyValue};
use crate::pystr::ascii_digits;
use crate::pyval::{get, is_str};
use crate::tasks::store_read::PyDoc;

fn int(n: i64) -> PyValue {
    PyValue::Int(PyInt::Small(n))
}

fn status(d: &PyValue, any: &[&str]) -> bool {
    any.iter().any(|s| is_str(get(d, "status"), s))
}

/// `decision_counts(items)`: `{"to_decide", "waiting"}` over `D` items, self-tests skipped.
pub(crate) fn decision_counts(items: &[PyDoc]) -> (i64, i64) {
    let (mut to_decide, mut waiting) = (0, 0);
    for doc in items {
        let d = &doc.data;
        if is_selftest(doc) || !doc.id.starts_with('D') {
            continue;
        }
        if status(d, &["open", "interactive"]) {
            to_decide += i64::from(!is_parked(d));
        } else if status(d, &["answered"]) && !is_relayed(d) {
            waiting += 1;
        }
    }
    (to_decide, waiting)
}

/// `decision_counts` as the Python dict.
pub(crate) fn decision_counts_py(items: &[PyDoc]) -> PyValue {
    let (to_decide, waiting) = decision_counts(items);
    PyValue::Dict(vec![
        ("to_decide".into(), int(to_decide)),
        ("waiting".into(), int(waiting)),
    ])
}

/// Why: the one count path for the footer and the summary.
/// What: `tally(items)`; see the module doc. Keys in Python's order.
/// Test: `dashboard_derive_recordings_replay_clean_against_the_port`.
pub(crate) fn tally(items: &[PyDoc]) -> PyValue {
    let (mut q, mut a, mut t) = (0, 0, 0);
    let (mut answered, mut parked, mut blockers, mut interactive) = (0, 0, 0, 0);
    for doc in items {
        let d = &doc.data;
        if is_selftest(doc) {
            continue;
        }
        if is_parked(d) {
            parked += 1;
        } else if status(d, &["open", "interactive"]) {
            interactive += i64::from(status(d, &["interactive"]));
            match get(d, "kind") {
                Some(PyValue::Str(k)) if k == "question" || k == "decision" => q += 1,
                Some(PyValue::Str(k)) if k == "action" => a += 1,
                Some(PyValue::Str(k)) if k == "task" => t += 1,
                _ => {}
            }
            blockers += i64::from(doc.id.starts_with('B'));
        } else if status(d, &["answered"]) && !is_relayed(d) {
            answered += 1;
        }
    }
    let mut out = vec![
        ("open".into(), int(q + a + t)),
        ("questions".into(), int(q)),
        ("actions".into(), int(a)),
        ("tasks".into(), int(t)),
        ("answered_unrelayed".into(), int(answered)),
        ("parked".into(), int(parked)),
        // See #12029: the decision count is `decision_counts`' own.
        ("decisions".into(), int(decision_counts(items).0)),
        ("blockers".into(), int(blockers)),
    ];
    if interactive > 0 {
        out.push(("interactive".into(), int(interactive)));
    }
    PyValue::Dict(out)
}

/// `_run_time(value, end_of_day)` as Unix seconds.
fn run_at(v: Option<&PyValue>, end: bool) -> Option<i64> {
    match v {
        Some(PyValue::Str(s)) => run_time(&ascii_digits(s), end),
        _ => None,
    }
}

/// `for x in value or []` over a JSON value: list items, string characters, dict keys.
fn py_iter(v: Option<&PyValue>) -> Vec<PyValue> {
    match v {
        Some(PyValue::List(l)) => l.clone(),
        Some(PyValue::Str(s)) => s.chars().map(|c| PyValue::Str(c.into())).collect(),
        Some(PyValue::Dict(p)) => p.iter().map(|(k, _)| PyValue::Str(k.clone())).collect(),
        _ => Vec::new(),
    }
}

fn head(s: &str) -> String {
    s.chars().take(10).collect()
}

/// Why: the page and the Architect show the same day count and next run.
/// What: `run_progress(item, now)`: `None` without a valid window, else
/// `{day, days, ended, running, next_run}`.
/// Test: `dashboard_derive_recordings_replay_clean_against_the_port`.
pub(crate) fn run_progress(item: &PyValue, now: f64) -> PyValue {
    let (Some(start), Some(end)) = (
        run_at(get(item, "run_start"), false),
        run_at(get(item, "run_end"), true),
    ) else {
        return PyValue::Null;
    };
    if end < start {
        return PyValue::Null;
    }
    let now_us = now_micros(now);
    let (start_us, end_us) = (start * 1_000_000, end * 1_000_000);
    let (sd, ed, nd) = (
        start.div_euclid(86_400),
        end.div_euclid(86_400),
        now_us.div_euclid(86_400_000_000),
    );
    let days = ed - sd + 1;
    let day = if now_us < start_us {
        0
    } else {
        days.min(nd - sd + 1)
    };
    let logged: Vec<String> = py_iter(get(item, "run_log"))
        .iter()
        .filter_map(|e| match get(e, "at") {
            Some(PyValue::Str(at)) => Some(head(at)),
            _ => None,
        })
        .collect();
    let today = iso_date(nd);
    let mut next: Vec<String> = py_iter(get(item, "run_schedule"))
        .into_iter()
        .filter_map(|x| match x {
            PyValue::Str(x) if head(&x) >= today && !logged.contains(&head(&x)) => Some(x),
            _ => None,
        })
        .collect();
    next.sort();
    let open = is_str(get(item, "status"), "open");
    PyValue::Dict(vec![
        ("day".into(), int(day)),
        ("days".into(), int(days)),
        (
            "ended".into(),
            PyValue::Bool(now_us > end_us && !is_str(get(item, "status"), "done")),
        ),
        (
            "running".into(),
            PyValue::Bool(open && start_us <= now_us && now_us <= end_us),
        ),
        (
            "next_run".into(),
            next.first()
                .map_or(PyValue::Null, |x| PyValue::Str(head(x))),
        ),
    ])
}

/// `running_count(items, now)`: open items whose run window contains `now`.
pub(crate) fn running_count(items: &[PyDoc], now: f64) -> i64 {
    let running =
        |doc: &PyDoc| get(&run_progress(&doc.data, now), "running") == Some(&PyValue::Bool(true));
    items.iter().filter(|d| running(d)).count() as i64
}
