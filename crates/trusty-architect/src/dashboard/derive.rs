//! Item rules the counts are derived from, as pure functions.
//!
//! Why: three scripts and the page each computed "open" their own way; one
//! module owns the rules and callers pass an explicit `now` (DOC-82 LDG-5.7,
//! `dashboard_derive.py`).
//! What: [`is_parked`] (an open item parked with a wake date, or by
//! `clarify`/`stale`), [`is_running`] (an open item whose `run_start`..`run_end`
//! window holds `now`; a date-only end runs to 23:59:59 UTC) and
//! [`running_count`], plus [`run_progress`], [`ordered`] and [`goal_sessions`]
//! (#10403). No function reads the clock or the filesystem; the
//! calendar arithmetic is written here (the crate takes no date dependency,
//! #9171 A1).
//! Test: `derive_parked_follows_the_page_rule`, `derive_running_window_is_inclusive`.

use serde_json::{Map, Value};

use super::store::Doc;

/// An open item the owner parked with a wake date, or by `clarify` / `stale`
/// (the page's `isParked`).
pub fn is_parked(data: &Map<String, Value>) -> bool {
    let open = data.get("status").and_then(Value::as_str) == Some("open");
    let Some(pk) = data.get("parked").and_then(Value::as_object) else {
        return false;
    };
    let until = pk.get("until").is_some_and(super::ids::truthy);
    let choice = pk.get("choice").and_then(Value::as_str);
    open && (until || matches!(choice, Some("clarify" | "stale")))
}

/// Whether `item` is open and `now` (Unix seconds) is inside its run window.
pub fn is_running(item: &Map<String, Value>, now: f64) -> bool {
    let time = |key: &str, end: bool| {
        item.get(key)
            .and_then(Value::as_str)
            .and_then(|s| run_time(s, end))
    };
    let (Some(start), Some(end)) = (time("run_start", false), time("run_end", true)) else {
        return false;
    };
    let open = item.get("status").and_then(Value::as_str) == Some("open");
    open && end >= start && start as f64 <= now && now <= end as f64
}

/// Open items whose run window contains `now`.
pub fn running_count(items: &[Doc], now: f64) -> usize {
    items.iter().filter(|d| is_running(&d.data, now)).count()
}

/// Unix seconds for `YYYY-MM-DD` (start of day, or 23:59:59 when `end_of_day`)
/// or `YYYY-MM-DDTHH:MM[:SS]Z`; `None` for any other form or an impossible
/// date (Python's `datetime` raises there). Python's `$` also matches before
/// one trailing newline.
pub(crate) fn run_time(value: &str, end_of_day: bool) -> Option<i64> {
    let s = value.strip_suffix('\n').unwrap_or(value);
    let num = |t: &str| {
        (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
            .then(|| t.parse::<i64>().ok())
            .flatten()
    };
    let (date, clock) = match s.split_once('T') {
        Some((d, c)) => (d, Some(c.strip_suffix('Z')?)),
        None => (s, None),
    };
    let d: Vec<&str> = date.split('-').collect();
    let [y, m, dd] = d.as_slice() else {
        return None;
    };
    if (y.len(), m.len(), dd.len()) != (4, 2, 2) {
        return None;
    }
    let (y, m, dd) = (num(y)?, num(m)?, num(dd)?);
    if !(1..=12).contains(&m) || dd < 1 || dd > days_in_month(y, m) || y < 1 {
        return None;
    }
    let day = days_from_civil(y, m, dd) * 86_400;
    let Some(clock) = clock else {
        return Some(if end_of_day { day + 86_399 } else { day });
    };
    let t: Vec<&str> = clock.split(':').collect();
    let parts = match t.as_slice() {
        [h, mi] => [*h, *mi, "00"],
        [h, mi, sec] => [*h, *mi, *sec],
        _ => return None,
    };
    if parts.iter().any(|p| p.len() != 2) {
        return None;
    }
    let (h, mi, sec) = (num(parts[0])?, num(parts[1])?, num(parts[2])?);
    (h < 24 && mi < 60 && sec < 60).then_some(day + h * 3600 + mi * 60 + sec)
}

pub(crate) fn days_in_month(y: i64, m: i64) -> i64 {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    match m {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's algorithm).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD` for epoch days (Python's `date.isoformat()`).
pub(crate) fn iso_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `now` (Unix seconds) as whole microseconds, the resolution of Python's
/// `datetime`.
pub(crate) fn now_micros(now: f64) -> i64 {
    // #10403: the derive rules compare instants in microseconds.
    (now * 1_000_000.0).round() as i64
}

/// What Python sees when it iterates `value` with `for x in value or []`: a
/// list's elements, a string's characters, an object's keys. Anything else
/// is empty (Python raises on a truthy number; this port does not).
pub(crate) fn py_iter(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(a)) => a.clone(),
        Some(Value::String(s)) => s.chars().map(|c| Value::String(c.into())).collect(),
        Some(Value::Object(o)) => o.keys().map(|k| Value::String(k.clone())).collect(),
        _ => Vec::new(),
    }
}

/// An item's run-window state (`run_progress` in `dashboard_derive.py`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RunProgress {
    /// 1-based UTC calendar day of the window `now` is on (0 before the start).
    pub day: i64,
    /// Calendar days in the window, inclusive.
    pub days: i64,
    /// `now` is past `run_end` and the item is not done.
    pub ended: bool,
    /// The item is open and `now` is inside the window.
    pub running: bool,
    /// First `run_schedule` date on or after today with no `run_log` entry.
    pub next_run: Option<String>,
}

/// Why: the page and the Architect both show a run's day count and next run;
/// one rule keeps them equal (#10403, `run_progress`).
/// What: `None` without a valid `run_start`/`run_end` pair or with the end
/// before the start; otherwise the [`RunProgress`] at `now` (Unix seconds).
/// `next_run` is the earliest `run_schedule` entry whose first ten
/// characters are on or after `now`'s UTC date and not a `run_log` date.
/// Test: `pending_parity_goldens`.
pub fn run_progress(item: &Map<String, Value>, now: f64) -> Option<RunProgress> {
    let time = |key: &str, end: bool| {
        item.get(key)
            .and_then(Value::as_str)
            .and_then(|s| run_time(s, end))
    };
    let (start, end) = (time("run_start", false)?, time("run_end", true)?);
    if end < start {
        return None;
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
    let head = |s: &str| s.chars().take(10).collect::<String>();
    let logged: Vec<String> = py_iter(item.get("run_log"))
        .iter()
        .filter_map(|e| e.as_object()?.get("at")?.as_str().map(head))
        .collect();
    let today = iso_date(nd);
    let mut next: Vec<&str> = Vec::new();
    let schedule = py_iter(item.get("run_schedule"));
    for x in schedule.iter().filter_map(Value::as_str) {
        let h = head(x);
        if h >= today && !logged.contains(&h) {
            next.push(x);
        }
    }
    next.sort_unstable();
    let status = item.get("status").and_then(Value::as_str);
    Some(RunProgress {
        day,
        days,
        ended: now_us > end_us && status != Some("done"),
        running: status == Some("open") && start_us <= now_us && now_us <= end_us,
        next_run: next.first().map(|x| head(x)),
    })
}

/// Why: every list the server returns is in one order (#10403, `ordered`).
/// What: documents by integer `rank` (booleans, non-integers and a missing
/// rank sort last), then by id. Stable.
/// Test: `pending_parity_goldens`.
pub fn ordered(docs: &[Doc]) -> Vec<&Doc> {
    let rank = |d: &Doc| -> Option<i128> {
        let n = d.data.get("rank")?.as_number()?;
        n.as_i64()
            .map(i128::from)
            .or_else(|| n.as_u64().map(i128::from))
    };
    let mut out: Vec<&Doc> = docs.iter().collect();
    out.sort_by(|a, b| {
        let key = |d: &Doc| match rank(d) {
            Some(r) => (0, r),
            None => (1, 0),
        };
        key(a).cmp(&key(b)).then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// Goal-to-session links, both ways, in Python's dict insertion order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoalSessions {
    /// `(goal id, sorted session names that exist)`, in goal order.
    pub by_goal: Vec<(String, Vec<String>)>,
    /// `(session name, sorted goal ids)`, in first-seen order.
    pub by_session: Vec<(String, Vec<String>)>,
}

/// Why: the page groups sessions under goals; a removed, done or parked goal
/// and a session that no longer exists must not show (#10403, `goal_sessions`).
/// What: for each goal whose `lifecycle` is not `removed`/`done`/`parked`, its
/// `sessions` entries that name an existing session doc, sorted, and the
/// inverse map. Only string names link (Python would also link equal
/// non-string values; the page writes strings).
/// Test: `pending_parity_goldens`.
pub fn goal_sessions(goals: &[Doc], sessions: &[Doc]) -> GoalSessions {
    let names: Vec<&str> = sessions
        .iter()
        .filter_map(|s| s.data.get("name").and_then(Value::as_str))
        .collect();
    let mut out = GoalSessions::default();
    for g in goals {
        let life = g.data.get("lifecycle").and_then(Value::as_str);
        if matches!(life, Some("removed" | "done" | "parked")) {
            continue;
        }
        let linked: Vec<String> = py_iter(g.data.get("sessions"))
            .iter()
            .filter_map(Value::as_str)
            .filter(|n| names.contains(n))
            .map(str::to_owned)
            .collect();
        for n in &linked {
            match out.by_session.iter_mut().find(|(k, _)| k == n) {
                Some((_, v)) => v.push(g.id.clone()),
                None => out.by_session.push((n.clone(), vec![g.id.clone()])),
            }
        }
        let mut sorted = linked;
        sorted.sort();
        out.by_goal.push((g.id.clone(), sorted));
    }
    for (_, v) in &mut out.by_session {
        v.sort();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn derive_parked_follows_the_page_rule() {
        let open = |pk: Value| obj(json!({"status": "open", "parked": pk}));
        assert!(is_parked(&open(json!({"until": "2999-01-01"}))));
        assert!(is_parked(&open(json!({"choice": "clarify"}))));
        assert!(is_parked(&open(json!({"choice": "stale"}))));
        assert!(!is_parked(&open(json!({"choice": "later", "until": ""}))));
        assert!(!is_parked(&open(json!(true))));
        assert!(!is_parked(&obj(
            json!({"status": "done", "parked": {"until": "x"}})
        )));
    }

    #[test]
    fn derive_running_window_is_inclusive() {
        // 2026-10-12T00:00:00Z
        let day = 1_791_763_200.0;
        let item =
            |s: &str, e: &str, st: &str| obj(json!({"run_start": s, "run_end": e, "status": st}));
        let a = item("2026-10-12", "2026-10-12", "open");
        assert!(is_running(&a, day));
        assert!(is_running(&a, day + 86_399.0));
        assert!(!is_running(&a, day + 86_399.5));
        assert!(!is_running(&a, day - 0.5));
        assert!(!is_running(&item("2026-10-12", "2026-10-12", "done"), day));
        assert!(is_running(
            &item("2026-10-11T23:59Z", "2026-10-12T00:00:00Z", "open"),
            day
        ));
        assert!(!is_running(&item("2026-10-13", "2026-10-12", "open"), day));
        assert!(!is_running(&item("2026-02-30", "2026-12-01", "open"), day));
        assert!(!is_running(&item("2026-10-1", "2026-12-01", "open"), day));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }
}
