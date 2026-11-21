//! The reminder ops: `remind`, `remind_done`, `remind_fire`.
//!
//! Why: reminders attach to tasks and change under the ledger lock in the same
//! commit as any other op. These are the op bodies of `datg.py`
//! (`apply_remind`, `apply_remind_done`, `apply_remind_fire`); `remind.rs`
//! holds the reminder rules themselves.
//! What: each body finds the task, calls the rule, and returns the log event
//! with the note text `datg.py` writes.
//! Test: `golden_steps_match_python_byte_for_byte` (the remind, remind-done and remind-fire steps).

use super::error::{IlResult, invalid};
use super::json::Json;
use super::limits::py_or;
use super::ops::{Ctx, Ev, ev, find, slot};
use super::remind;
use super::time::{Shape, strptime};
use crate::pystr::py_repr;

fn task_of(st: &Json, id: Option<&Json>) -> IlResult<usize> {
    match find(st, id)? {
        ('T', i) => Ok(i),
        _ => invalid("reminders attach to tasks"),
    }
}

fn or_invalid<T>(r: Result<T, String>) -> IlResult<T> {
    r.or_else(invalid)
}

/// `apply_remind`: attaches a reminder; an identical live one is a no-op.
///
/// # Errors
/// `Invalid` with the reminder rule's message.
pub fn apply_remind(st: &mut Json, a: &Json) -> IlResult<Ev> {
    let i = task_of(st, a.get("id"))?;
    let t = slot(st, 'T', i);
    let (at, note, every) = (
        py_or(a.get("at")),
        py_or(a.get("note")),
        py_or(a.get("every")),
    );
    let (idx, created) = or_invalid(remind::add(t, &at, &note, &every))?;
    let r = &t.arr("reminders")[idx];
    let text = format!(
        "reminder #{} {}{}",
        r.i("n").unwrap_or(0),
        r.s("at"),
        if created { "" } else { " (exists)" }
    );
    Ok(ev(t.s("id"), "update", text))
}

/// `_remind_n`: `int(str(v).lstrip("#"))`.
fn remind_n(v: Option<&Json>) -> IlResult<i64> {
    let raw = v.map_or_else(|| "None".to_owned(), Json::py_str);
    let t = raw.trim_start_matches('#');
    match t.parse::<i64>() {
        Ok(n) => Ok(n),
        Err(_) => invalid(format!(
            "reminder number {} must be an integer",
            py_repr(&raw)
        )),
    }
}

/// `apply_remind_done`: acks reminder `n` (a repeating one moves to its next time unless `end`).
///
/// # Errors
/// `Invalid` with the reminder rule's message.
pub fn apply_remind_done(st: &mut Json, a: &Json, ctx: &Ctx) -> IlResult<Ev> {
    let i = task_of(st, a.get("id"))?;
    let now = match a.get("now").filter(|n| n.truthy()) {
        Some(n) => or_invalid(remind::parse_at(&n.py_str()))? as f64,
        None => ctx.clock.now_unix(),
    };
    let n = remind_n(a.get("n"))?;
    let end = a.get("end").is_some_and(Json::truthy);
    let t = slot(st, 'T', i);
    let (idx, _) = or_invalid(remind::mark_done(t, n, now, end))?;
    let r = &t.arr("reminders")[idx];
    let tail = if r.s("state") == "pending" {
        format!("acked, next {}", r.s("at"))
    } else {
        "done".to_owned()
    };
    Ok(ev(
        t.s("id"),
        "update",
        format!("reminder #{} {tail}", r.i("n").unwrap_or(0)),
    ))
}

/// `apply_remind_fire`: fires the reminders named in `keys` `[[task id, n, at]]` that are still
/// pending with that `at` and past due at `now` (an ISO minute), so a stale call fires nothing.
///
/// # Errors
/// `Invalid` for a bad `now` or an id that is not a task.
pub fn apply_remind_fire(st: &mut Json, a: &Json) -> IlResult<Vec<Ev>> {
    let now = or_invalid(remind::parse_at(&py_or(a.get("now"))))?;
    let mut out = Vec::new();
    for key in a.arr("keys") {
        let k = key.as_arr().unwrap_or(&[]);
        let (Some(tid), Some(n), Some(at)) = (
            k.first(),
            k.get(1).and_then(Json::as_i64),
            k.get(2).and_then(Json::as_str),
        ) else {
            continue;
        };
        let i = task_of(st, Some(tid))?;
        let t = slot(st, 'T', i);
        let Ok(ri) = remind::find(t, n) else { continue };
        let r = &t.arr("reminders")[ri];
        let due = strptime(r.s("at"), Shape::Minute).is_some_and(|d| d <= now);
        if r.s("state") != "pending" || r.s("at") != at || !due {
            continue;
        }
        let (tid, title) = (t.s("id").to_owned(), t.s("title").to_owned());
        let r = &mut t.arr_or_insert("reminders")[ri];
        remind::fire(r, now);
        let mut copy = r.clone();
        copy.set("fired_for", at);
        out.push(Ev {
            id: tid,
            ev: "update".into(),
            note: format!("reminder #{n} fired"),
            task: Some(title),
            reminder: Some(copy),
        });
    }
    Ok(out)
}
