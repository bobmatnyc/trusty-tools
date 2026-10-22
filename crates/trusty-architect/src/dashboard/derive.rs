//! Item rules the counts are derived from, as pure functions.
//!
//! Why: three scripts and the page each computed "open" their own way; one
//! module owns the rules and callers pass an explicit `now` (DOC-82 LDG-5.7,
//! `dashboard_derive.py`).
//! What: [`is_parked`] (an open item parked with a wake date, or by
//! `clarify`/`stale`), [`is_running`] (an open item whose `run_start`..`run_end`
//! window holds `now`; a date-only end runs to 23:59:59 UTC) and
//! [`running_count`]. No function reads the clock or the filesystem; the
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
fn run_time(value: &str, end_of_day: bool) -> Option<i64> {
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

fn days_in_month(y: i64, m: i64) -> i64 {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    match m {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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
