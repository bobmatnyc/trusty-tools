//! Reminders attached to tasks (`il_remind.py`).
//!
//! Why: a dated "remind me" is an attribute of the task it serves, not a task of
//! its own. The reminder lives in `state.json` under the task's `reminders`, and
//! every mutation runs under the ledger lock in the single commit.
//! What: parse `at` and `every`, add, ack ([`mark_done`]), fire, list and the
//! digest lines. Functions take the task `Json` and return plain values; the
//! messages are `RemindError`'s text because `datg.py` re-raises them as
//! `DatgError`.
//! Test: `parse_at_forms`, `add_is_idempotent_and_numbers_never_repeat`,
//! `ack_advances_a_repeating_reminder`, `fire_marks_fired_once`, `lines_sort_soonest_first`.

use super::json::{Json, obj};
use super::time::{Shape, fmt_min, strptime};
use crate::pystr::{collapse_space, strip};

/// Longest note.
pub const NOTE_MAX: usize = 160;
/// Live reminders per task.
pub const MAX_PER_TASK: usize = 8;
const DATE_ONLY_HOUR: i64 = 12;

/// `parse_at`: `YYYY-MM-DD` (12:00Z) or the minute/second forms, as epoch seconds.
///
/// # Errors
/// The `RemindError` text.
pub fn parse_at(s: &str) -> Result<i64, String> {
    let s = strip(s);
    if let Some(d) = strptime(s, Shape::Date) {
        return Ok(d + DATE_ONLY_HOUR * 3600);
    }
    strptime(s, Shape::Minute)
        .or_else(|| strptime(s, Shape::Second))
        .map(|e| e - e % 60)
        .ok_or_else(|| {
            format!(
                "at {} must be YYYY-MM-DD or YYYY-MM-DDTHH:MMZ (UTC)",
                crate::pystr::py_repr(s)
            )
        })
}

/// `parse_every`: `14d`, `36h`, `2w` to (canonical text, seconds); `None` for empty.
///
/// # Errors
/// The `RemindError` text.
pub fn parse_every(s: &str) -> Result<Option<(String, i64)>, String> {
    let s = strip(s).to_lowercase();
    if s.is_empty() {
        return Ok(None);
    }
    let bad = || {
        format!(
            "every {} must look like 14d, 36h or 2w",
            crate::pystr::py_repr(&s)
        )
    };
    let (num, unit) = s.split_at(s.len() - 1);
    if !(1..=4).contains(&num.len()) || !num.bytes().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    let n: i64 = num.parse().map_err(|_| bad())?;
    let secs = match unit {
        "h" => 3600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return Err(bad()),
    };
    if n < 1 {
        return Err(bad());
    }
    Ok(Some((s, n * secs)))
}

/// `_note`: one line, 1-160 characters, whitespace collapsed.
///
/// # Errors
/// The `RemindError` text.
pub fn note(s: &str) -> Result<String, String> {
    let s = collapse_space(s);
    if s.is_empty() || s.chars().count() > NOTE_MAX {
        return Err(format!("note must be one line, 1-{NOTE_MAX} chars"));
    }
    Ok(s)
}

fn reminders(task: &Json) -> &[Json] {
    task.arr("reminders")
}

/// The reminders of `task` not yet done.
pub fn live(task: &Json) -> Vec<&Json> {
    reminders(task)
        .iter()
        .filter(|r| r.s("state") != "done")
        .collect()
}

fn at_secs(r: &Json) -> Result<i64, String> {
    strptime(r.s("at"), Shape::Minute)
        .ok_or_else(|| format!("reminder at {:?} is not a minute timestamp", r.s("at")))
}

/// `add`: attaches a reminder; returns its index and whether it was created
/// (an identical live one is returned, not duplicated).
///
/// # Errors
/// The `RemindError` text.
pub fn add(task: &mut Json, at: &str, text: &str, every: &str) -> Result<(usize, bool), String> {
    let tid = task.s("id").to_owned();
    if task.s("status") == "done" {
        return Err(format!("{tid} is done; attach the reminder to a live task"));
    }
    let at = fmt_min(parse_at(at)?);
    let every = parse_every(every)?.map(|(t, _)| t);
    let text = note(text)?;
    let every_json = every.clone().map_or(Json::Null, Json::from);
    let rs = task.arr_or_insert("reminders");
    if let Some(i) = rs.iter().position(|r| {
        r.s("state") != "done"
            && r.s("at") == at
            && r.get("every").unwrap_or(&Json::Null) == &every_json
            && r.s("note") == text
    }) {
        return Ok((i, false));
    }
    if live_count_of(rs) >= MAX_PER_TASK {
        return Err(format!(
            "{tid} already has {MAX_PER_TASK} live reminders; finish one first"
        ));
    }
    let n = rs.iter().filter_map(|r| r.i("n")).max().unwrap_or(0) + 1;
    rs.push(obj([
        ("n", n.into()),
        ("at", at.into()),
        ("every", every_json),
        ("note", text.into()),
        ("state", "pending".into()),
    ]));
    while rs.len() > 2 * MAX_PER_TASK {
        // The history of finished ones is bounded; the oldest done goes first, so max n stays.
        match rs.iter().position(|r| r.s("state") == "done") {
            Some(i) => {
                rs.remove(i);
            }
            None => break,
        }
    }
    let idx = rs.iter().position(|r| r.i("n") == Some(n)).unwrap_or(0);
    Ok((idx, true))
}

fn live_count_of(rs: &[Json]) -> usize {
    rs.iter().filter(|r| r.s("state") != "done").count()
}

/// `find`: the index of reminder number `n`.
///
/// # Errors
/// "has no reminder #n".
pub fn find(task: &Json, n: i64) -> Result<usize, String> {
    reminders(task)
        .iter()
        .position(|r| r.i("n") == Some(n))
        .ok_or_else(|| format!("{} has no reminder #{n}", task.s("id")))
}

/// `mark_done`: acks reminder `n`. A repeating one that fired (or is past due) moves to its next
/// time and stays pending unless `end`; anything else becomes done. Returns the index and whether it changed.
///
/// # Errors
/// The `RemindError` text.
pub fn mark_done(task: &mut Json, n: i64, now: f64, end: bool) -> Result<(usize, bool), String> {
    let idx = find(task, n)?;
    let r = &task.arr("reminders")[idx];
    if r.s("state") == "done" {
        return Ok((idx, false));
    }
    let step = parse_every(r.get("every").and_then(Json::as_str).unwrap_or(""))?.map(|(_, s)| s);
    let at = at_secs(r)?;
    let fired = r.s("state") == "fired";
    let rs = task.arr_or_insert("reminders");
    if let Some(step) = step
        && !end
        && (fired || (at as f64) <= now)
    {
        let k = ((now - at as f64) / step as f64).floor() as i64;
        rs[idx].set("at", fmt_min(at + step * (k + 1)));
        rs[idx].set("state", "pending");
        rs[idx].set("acked_at", fmt_min(now as i64));
        return Ok((idx, true));
    }
    rs[idx].set("state", "done");
    Ok((idx, true))
}

/// `fire`: marks one due reminder fired (it stays fired until acked).
pub fn fire(r: &mut Json, now: i64) {
    let fires = r.i("fires").unwrap_or(0) + 1;
    r.set("fired_at", fmt_min(now));
    r.set("fires", fires);
    r.set("state", "fired");
}

/// `label`: `#n at [every E] [state] note`.
pub fn label(r: &Json) -> String {
    let every = r
        .get("every")
        .and_then(Json::as_str)
        .filter(|e| !e.is_empty());
    format!(
        "#{} {}{} [{}] {}",
        r.i("n").unwrap_or(0),
        r.s("at"),
        every.map_or(String::new(), |e| format!(" every {e}")),
        r.s("state"),
        r.s("note")
    )
}

/// `lines`: the digest lines for the live reminders of `task`, soonest first.
pub fn lines(task: &Json) -> Vec<String> {
    let mut rs = live(task);
    rs.sort_by(|a, b| a.s("at").cmp(b.s("at")));
    rs.into_iter()
        .map(|r| format!("    remind {}", label(r)))
        .collect()
}

/// One row of the `reminders` listing.
pub fn row(task: &Json, r: &Json) -> Json {
    obj([
        ("task", task.s("id").into()),
        (
            "title",
            task.get("title").cloned().unwrap_or_else(|| "".into()),
        ),
        ("n", r.get("n").cloned().unwrap_or(Json::Null)),
        ("at", r.s("at").into()),
        ("every", r.get("every").cloned().unwrap_or(Json::Null)),
        ("note", r.s("note").into()),
        ("state", r.s("state").into()),
        ("fired_at", r.get("fired_at").cloned().unwrap_or(Json::Null)),
    ])
}

/// `listing`: rows for `reminders`, soonest first. `due_only` keeps pending ones past their time and fired ones.
pub fn listing(st: &Json, now: i64, due_only: bool, with_done: bool) -> Vec<Json> {
    let mut rows = Vec::new();
    for t in st.arr("tasks") {
        for r in reminders(t) {
            let state = r.s("state");
            if state == "done" && !with_done {
                continue;
            }
            let past = at_secs(r).is_ok_and(|a| a <= now);
            if due_only && !(state == "fired" || (state == "pending" && past)) {
                continue;
            }
            rows.push(row(t, r));
        }
    }
    rows.sort_by(|a, b| {
        (a.s("at"), a.s("task"), a.i("n")).cmp(&(b.s("at"), b.s("task"), b.i("n")))
    });
    rows
}

/// `fmt_row`: `TASK#n at every=E [state] note`.
pub fn fmt_row(x: &Json) -> String {
    let every = x
        .get("every")
        .and_then(Json::as_str)
        .filter(|e| !e.is_empty())
        .unwrap_or("-");
    format!(
        "{}#{} {} every={every} [{}] {}",
        x.s("task"),
        x.i("n").unwrap_or(0),
        x.s("at"),
        x.s("state"),
        x.s("note")
    )
}

/// Pending reminders past `now`, as `(task index, reminder index)`, oldest first.
pub fn due(st: &Json, now: i64) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (ti, t) in st.arr("tasks").iter().enumerate() {
        for (ri, r) in reminders(t).iter().enumerate() {
            if r.s("state") == "pending" && at_secs(r).is_ok_and(|a| a <= now) {
                out.push((ti, ri));
            }
        }
    }
    let key = |&(ti, ri): &(usize, usize)| {
        let t = &st.arr("tasks")[ti];
        (
            reminders(t)[ri].s("at").to_owned(),
            t.s("id").to_owned(),
            reminders(t)[ri].i("n"),
        )
    };
    out.sort_by_key(key);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::time::strptime as sp;

    fn task() -> Json {
        obj([
            ("id", "tt-T01".into()),
            ("title", "t".into()),
            ("status", "open".into()),
        ])
    }

    fn min(s: &str) -> i64 {
        sp(s, Shape::Minute).unwrap()
    }

    #[test]
    fn parse_at_forms() {
        assert_eq!(
            fmt_min(parse_at("2026-11-20").unwrap()),
            "2026-11-20T12:00Z"
        );
        assert_eq!(
            fmt_min(parse_at(" 2026-11-20T08:30:45Z ").unwrap()),
            "2026-11-20T08:30Z"
        );
        assert!(
            parse_at("tomorrow")
                .unwrap_err()
                .starts_with("at 'tomorrow' must be")
        );
        assert_eq!(
            parse_every("2W").unwrap(),
            Some(("2w".to_owned(), 1_209_600))
        );
        assert!(parse_every("0d").is_err());
        assert!(parse_every("5").is_err());
        assert_eq!(parse_every("").unwrap(), None);
    }

    #[test]
    fn add_is_idempotent_and_numbers_never_repeat() {
        let mut t = task();
        assert_eq!(add(&mut t, "2026-11-20", "do it", "").unwrap(), (0, true));
        assert_eq!(
            add(&mut t, "2026-11-20", "do   it", "").unwrap(),
            (0, false)
        );
        assert_eq!(
            add(&mut t, "2026-11-21", "again", "14d").unwrap(),
            (1, true)
        );
        assert_eq!(t.arr("reminders")[1].i("n"), Some(2));
        assert_eq!(
            t.arr("reminders")[0].dump(),
            r#"{"n":1,"at":"2026-11-20T12:00Z","every":null,"note":"do it","state":"pending"}"#
        );
        let mut done = task();
        done.set("status", "done");
        assert!(
            add(&mut done, "2026-11-20", "x", "")
                .unwrap_err()
                .contains("is done")
        );
    }

    #[test]
    fn ack_advances_a_repeating_reminder() {
        let mut t = task();
        add(&mut t, "2026-11-01", "x", "7d").unwrap();
        let now = min("2026-11-20T00:00Z");
        mark_done(&mut t, 1, now as f64, false).unwrap();
        let r = &t.arr("reminders")[0];
        assert_eq!(
            (r.s("at"), r.s("state"), r.s("acked_at")),
            ("2026-11-22T12:00Z", "pending", "2026-11-20T00:00Z")
        );
        mark_done(&mut t, 1, now as f64, true).unwrap();
        assert_eq!(t.arr("reminders")[0].s("state"), "done");
        assert_eq!(mark_done(&mut t, 1, now as f64, true).unwrap(), (0, false));
        assert!(
            mark_done(&mut t, 9, 0.0, false)
                .unwrap_err()
                .contains("no reminder #9")
        );
    }

    #[test]
    fn fire_marks_fired_once() {
        let mut t = task();
        add(&mut t, "2026-11-01", "x", "").unwrap();
        let st = obj([("tasks", Json::Arr(vec![t.clone()]))]);
        assert_eq!(due(&st, min("2026-11-02T00:00Z")), vec![(0, 0)]);
        assert!(due(&st, min("2026-10-31T00:00Z")).is_empty());
        let mut r = t.arr("reminders")[0].clone();
        fire(&mut r, min("2026-11-02T00:00Z"));
        assert_eq!(
            r.dump(),
            r#"{"n":1,"at":"2026-11-01T12:00Z","every":null,"note":"x","state":"fired","fired_at":"2026-11-02T00:00Z","fires":1}"#
        );
    }

    #[test]
    fn lines_sort_soonest_first() {
        let mut t = task();
        add(&mut t, "2026-11-21", "later", "").unwrap();
        add(&mut t, "2026-11-20", "sooner", "36h").unwrap();
        assert_eq!(
            lines(&t),
            vec![
                "    remind #2 2026-11-20T12:00Z every 36h [pending] sooner",
                "    remind #1 2026-11-21T12:00Z [pending] later"
            ]
        );
    }
}
