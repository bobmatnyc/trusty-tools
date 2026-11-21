//! The `update` and `pulse` ops, and the status moves behind them.
//!
//! Why: `update` is the widest op (maps, title, rank, priority, esc, answer, to,
//! done_when, outcome, status), and a status move touches the WIP queue, the
//! evidence warning and the live-reminder rule. Split from `ops.rs` to keep each
//! file under the size cap; the bodies are `datg.py` `apply_update` and `apply_pulse`.
//! What: [`apply_update`] (one event per change, or one `update`) and
//! [`apply_pulse`] (note, timestamps, progress and token fields of a task).
//! Test: `golden_steps_match_python_byte_for_byte` (priority, status, close and pulse steps),
//! `act_records_without_bumping_the_version`.

use super::error::{IlResult, invalid};
use super::evidence::has_evidence;
use super::json::Json;
use super::limits::{lim, py_or, title};
use super::ops::{Ctx, Ev, bucket, ev, find, slot};
use super::wip::{promote_overflow, rebalance, wip_cap, wip_count};
use crate::pystr::strip;

/// `evidence_warning`: a closing task whose outcome cites nothing checkable.
fn evidence_warning(o: &Json, warnings: &mut Vec<String>) {
    if !has_evidence(o.s("outcome")) {
        let dw = Some(o.s("done_when"))
            .filter(|d| !d.is_empty())
            .unwrap_or("none");
        warnings.push(format!(
            "warning: {} closed with no evidence in the outcome (link, sha, PR/issue ref, run id or 'verified'); done_when: {dw}",
            o.s("id")
        ));
    }
}

fn valid_priority(v: &Json) -> bool {
    matches!(v, Json::Int(n) if (1..=5).contains(n))
}

fn status_tuple(pre: char) -> &'static str {
    match pre {
        'G' => "('active', 'done', 'parked')",
        'T' => "('open', 'running', 'done', 'parked')",
        _ => "('open', 'answered', 'done')",
    }
}

fn status_ok(pre: char, s: &str) -> bool {
    match pre {
        'G' => ["active", "done", "parked"].contains(&s),
        'T' => ["open", "running", "done", "parked"].contains(&s),
        _ => ["open", "answered", "done"].contains(&s),
    }
}

/// `apply_update`: edits one goal, item or task; returns one event per change (or one `update`).
///
/// # Errors
/// `Invalid` with `datg.py`'s message.
pub fn apply_update(st: &mut Json, a: &Json, ctx: &mut Ctx) -> IlResult<Vec<Ev>> {
    let (pre, i) = find(st, a.get("id"))?;
    let mut evs: Vec<&'static str> = Vec::new();
    let in_gt = matches!(pre, 'G' | 'T');
    if let Some(m) = a.opt("maps") {
        if !in_gt {
            return invalid("maps applies to goals and tasks (the dashboard goal id)");
        }
        let v = lim(
            Some(&Json::from(strip(&m.py_str()))),
            "maps",
            24,
            false,
            false,
        )?;
        slot(st, pre, i).set("maps", v);
        evs.push("update");
    }
    if let Some(t) = a.opt("title") {
        let v = title(Some(t))?;
        slot(st, pre, i).set("title", v);
    }
    if let Some(r) = a.opt("rank") {
        match r {
            Json::Int(n) if pre == 'G' && *n >= 1 => slot(st, pre, i).set("rank", *n),
            _ => return invalid("rank applies to goals, integer >= 1"),
        }
        evs.push("reprio");
    }
    if let Some(p) = a.opt("priority") {
        let pr = match p {
            Json::Str(s)
                if !strip(s).is_empty() && strip(s).bytes().all(|b| b.is_ascii_digit()) =>
            {
                strip(s).parse().map_or(Json::Null, Json::Int)
            }
            other => other.clone(),
        };
        if pre != 'T' || !valid_priority(&pr) {
            return invalid(
                "priority applies to tasks, integer 1-5 (1 critical, 2 urgent, 3 high, 4 medium, 5 low)",
            );
        }
        slot(st, pre, i).set("priority", pr);
        evs.push("update");
        for (tid, what) in rebalance(st) {
            ctx.warnings.push(format!("note: {tid} {what}"));
        }
    }
    if let Some(e) = a.opt("esc") {
        let esc = e.as_str().filter(|s| matches!(*s, "architect" | ""));
        let (Some(esc), 'D' | 'A') = (esc, pre) else {
            return invalid("esc applies to D/A, value 'architect' or ''");
        };
        slot(st, pre, i).set(
            "esc",
            if esc.is_empty() {
                Json::Null
            } else {
                esc.into()
            },
        );
        if !esc.is_empty() {
            evs.push("escalated");
        }
    }
    if let Some(ans) = a.opt("answer") {
        if !matches!(pre, 'D' | 'A') {
            return invalid("answer applies to D/A");
        }
        let v = lim(Some(ans), "answer", 300, false, true)?;
        let o = slot(st, pre, i);
        o.set("answer", v);
        o.set("status", "answered");
        evs.push("answered");
    }
    if let Some(to) = a.opt("to") {
        if pre != 'T' {
            return invalid("to applies to tasks");
        }
        slot(st, pre, i).set("to", to.clone());
    }
    if let Some(dw) = a.opt("done_when") {
        let t = strip(&dw.py_str()).to_owned();
        if pre != 'T' || t.is_empty() {
            return invalid("done_when applies to tasks and cannot be empty");
        }
        let v = lim(Some(&Json::from(t)), "done_when", 300, false, false)?;
        slot(st, pre, i).set("done_when", v);
    }
    if let Some(out) = a.opt("outcome") {
        if pre == 'G' {
            return invalid("outcome does not apply to goals");
        }
        let v = lim(Some(out), "outcome", 300, false, true)?;
        slot(st, pre, i).set("outcome", v);
    }
    if let Some(s) = a.opt("status") {
        status_change(st, pre, i, s, &mut evs, ctx)?;
    }
    let o = &st.arr(bucket(pre))[i];
    let note = py_or(a.get("note"));
    let note = if note.is_empty() {
        o.s("title").to_owned()
    } else {
        note
    };
    let list: Vec<&str> = if evs.is_empty() { vec!["update"] } else { evs };
    Ok(list
        .into_iter()
        .map(|e| ev(o.s("id"), e, note.clone()))
        .collect())
}

fn status_change(
    st: &mut Json,
    pre: char,
    i: usize,
    s: &Json,
    evs: &mut Vec<&'static str>,
    ctx: &mut Ctx,
) -> IlResult<()> {
    let new = s.as_str().filter(|x| status_ok(pre, x));
    let Some(new) = new else {
        return invalid(format!(
            "status for {pre} must be one of {}",
            status_tuple(pre)
        ));
    };
    let o = &st.arr(bucket(pre))[i];
    let done = new == "done";
    if done && pre != 'G' && !o.get("outcome").is_some_and(Json::truthy) {
        return invalid("done needs a verified one-line outcome");
    }
    if done && pre == 'T' {
        evidence_warning(o, &mut ctx.warnings);
        let live = super::remind::live(o);
        if !live.is_empty() {
            let ns: Vec<String> = live
                .iter()
                .map(|r| format!("#{}", r.i("n").unwrap_or(0)))
                .collect();
            return invalid(format!(
                "{} has live reminders ({}); attach them to another task (remind), then remind-done here",
                o.s("id"),
                ns.join(", ")
            ));
        }
    }
    let was = o.s("status").to_owned();
    let id = o.s("id").to_owned();
    slot(st, pre, i).set("status", new);
    let (active, was_active) = (
        matches!(new, "open" | "running"),
        matches!(was.as_str(), "open" | "running"),
    );
    if active {
        slot(st, pre, i).remove("overflow"); // a hand-promoted overflow task is no longer overflow
        if pre == 'T' && !was_active && wip_count(st) > wip_cap(st) {
            ctx.warnings.push(format!(
                "note: {id} reopened past the cap: {} active tasks (cap {})",
                wip_count(st),
                wip_cap(st)
            ));
        }
    }
    if done {
        evs.push("done");
    }
    if pre == 'T' && was_active && !active {
        for tid in promote_overflow(st) {
            ctx.warnings
                .push(format!("note: {tid} promoted from overflow to open"));
        }
    }
    Ok(())
}

/// `apply_pulse`: note, timestamps, progress and token fields of a task. Never touches status or outcome.
///
/// # Errors
/// `Invalid` with `datg.py`'s message.
pub fn apply_pulse(st: &mut Json, a: &Json) -> IlResult<Ev> {
    let (pre, i) = find(st, a.get("id"))?;
    if pre != 'T' {
        return invalid("pulse applies to tasks");
    }
    if let Some(n) = a.opt("note") {
        let v = lim(Some(n), "pulse", 200, false, false)?;
        slot(st, pre, i).set("pulse", v);
    }
    for k in ["last_msg_at", "last_signal_at", "progress_at"] {
        if let Some(v) = a.get(k).filter(|v| v.truthy()) {
            let s = lim(Some(v), k, 30, true, false)?;
            slot(st, pre, i).set(k, s);
        }
    }
    if let Some(p) = a.opt("pct") {
        match digits(p) {
            Some(n) if n <= 100 => slot(st, pre, i).set("pct", n),
            _ => return invalid("pulse pct must be an integer 0-100"),
        }
    }
    for (k, mx) in [("est", 80), ("remaining", 80)] {
        if let Some(v) = a.opt(k) {
            let s = lim(Some(v), k, mx, false, false)?;
            slot(st, pre, i).set(k, s);
        }
    }
    if let Some(v) = a.opt("progress_note") {
        let s = lim(Some(v), "progress_note", 200, false, false)?;
        slot(st, pre, i).set("progress_note", s);
    }
    for k in ["est_in", "est_out", "act_in", "act_out"] {
        if let Some(v) = a.opt(k) {
            let Some(n) = digits(v) else {
                return invalid(format!(
                    "pulse {k} must be a non-negative integer token count"
                ));
            };
            slot(st, pre, i).set(k, n);
        }
    }
    if let Some(v) = a.opt("act_models") {
        let s = lim(Some(v), "act_models", 120, false, false)?;
        slot(st, pre, i).set("act_models", s);
    }
    Ok(ev(st.arr("tasks")[i].s("id"), "update", "pulse"))
}

/// `str(v).isdigit()` then `int(v)`: a non-negative integer from a number or a digit string.
fn digits(v: &Json) -> Option<i64> {
    let s = v.py_str();
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
}
