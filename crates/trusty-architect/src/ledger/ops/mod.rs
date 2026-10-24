//! The batch ops applied to a loaded `state.json` (#9758).
//!
//! Why: the writer must change a store exactly as the prototype's
//! `apply_add`, `apply_update`, `apply_session`, `collect_closed` and
//! `prune_state` do, so the dual-run diff is empty (LDG-1.5).
//! What: [`apply`] runs one [`Op`] on a [`State`] and returns its log events
//! and the records it removed for `archive.jsonl`. [`add_fields`] and
//! [`session_fields`] hold every per-op limit once; [`Op::check`] calls them
//! before the store is read, and [`apply`] calls them again for the trimmed
//! values. An error leaves the caller's copy half-changed; the writer drops it.
//! Test: `ledger_writer_matches_prototype_bytes`, `ledger_writer_session_limits_refuse_and_leave_files`.

// #9758: the session op lives in its own file to keep each under the SLOC cap.
mod session;

use std::collections::BTreeMap;

pub(crate) use session::session_fields;

use crate::error::LedgerError;
use crate::ledger::ids::{LedgerId, Prefix};
use crate::ledger::limits::{PARAGRAPH_MAX, check_line, check_paragraph, check_title};
use crate::ledger::model::{
    Archived, Extra, Goal, GoalStatus, Item, ItemKind, ItemStatus, Opt, OptObject, Opts, State,
    Task, TaskStatus,
};
use crate::ledger::rpc::{AddOp, BatchEvent, Op, UpdateOp};

/// The clock reading one commit uses throughout.
#[derive(Debug, Clone)]
pub(crate) struct Now {
    /// `YYYY-MM-DDTHH:MMZ`, the prototype's `now_iso()`.
    pub minute: String,
    /// Whole seconds, the prototype's `int(time.time())`.
    pub epoch: i64,
    /// Seconds, fractional.
    pub unix: f64,
}

impl Now {
    /// The reading at `unix` seconds.
    pub fn at(unix: f64) -> Self {
        let iso = crate::classify::alerts::iso(unix);
        Self {
            minute: format!("{}Z", &iso[..16]),
            // Seconds since the epoch fit an i64 for any real clock.
            epoch: unix.floor() as i64,
            unix,
        }
    }
}

/// What the ops of a batch read besides the state.
pub(crate) struct Context<'a> {
    /// Last `answered` time per id, from `log.jsonl`.
    pub answered: &'a BTreeMap<String, f64>,
    /// The commit's clock reading.
    pub now: &'a Now,
}

fn bad(msg: impl Into<String>) -> LedgerError {
    LedgerError::Invalid(msg.into())
}

/// Runs `op`, already [`Op::check`]ed, on `st`: the events to log, and the
/// records it removed.
pub(crate) fn apply(
    st: &mut State,
    op: &Op,
    cx: &Context<'_>,
) -> Result<(Vec<BatchEvent>, Vec<Archived>), LedgerError> {
    match op {
        Op::Add(a) => Ok((vec![apply_add(st, a)?], Vec::new())),
        Op::Update(u) => Ok((apply_update(st, u)?, Vec::new())),
        Op::Close(c) => {
            let u = UpdateOp {
                id: c.id.clone(),
                outcome: Some(c.outcome.clone()),
                status: Some("done".into()),
                title: None,
                rank: None,
                esc: None,
                answer: None,
                to: None,
                note: None,
            };
            Ok((apply_update(st, &u)?, Vec::new()))
        }
        Op::Session(s) => Ok((vec![session::apply_session(st, s, cx.now)?], Vec::new())),
        Op::Prune(p) => {
            let hours = f64::from(p.answered_hours.unwrap_or(24));
            let gone = prune_state(st, cx.answered, hours, p.require_outcome, cx.now.unix);
            let events = (!gone.is_empty())
                .then(|| {
                    event(
                        &format!("{}-prune", st.code.as_str()),
                        "update",
                        &format!("archived {}", gone.len()),
                    )
                })
                .into_iter()
                .collect();
            Ok((events, gone))
        }
    }
}

fn event(id: &str, ev: &str, note: &str) -> BatchEvent {
    BatchEvent {
        id: id.to_owned(),
        ev: ev.to_owned(),
        note: note.to_owned(),
    }
}

/// `add`'s option list and `finish`, checked (at most 4 options, labels at
/// most 160 characters; `finish` one line, at most 160).
pub(crate) fn add_fields(a: &AddOp) -> Result<(Opts, String), LedgerError> {
    let opts = a.opts.as_deref().unwrap_or_default();
    if opts.len() > 4 || opts.iter().any(|o| o.l.chars().count() > 160) {
        return Err(bad("opts: at most 4 options, each label <= 160 chars"));
    }
    let finish = check_line("finish", a.finish.as_deref().unwrap_or(""), 160, false)?;
    let list = opts.iter().map(|o| {
        Opt::Object(OptObject {
            l: Some(o.l.clone()),
            label: None,
            rec: Some(o.rec),
            extra: Extra::new(),
        })
    });
    Ok((Opts::List(list.collect()), finish))
}

fn apply_add(st: &mut State, a: &AddOp) -> Result<BatchEvent, LedgerError> {
    if a.kind == Prefix::Brief {
        return Err(bad("kind must be G, D, A or T"));
    }
    let title = check_title(&a.title)?.to_owned();
    let (opts, finish) = add_fields(a)?;
    let id = match a.id.as_deref() {
        Some(x) => explicit_id(st, a.kind, x)?,
        None => st.allocate(a.kind)?.to_string(),
    };
    let goal = a.goal.clone().unwrap_or_default();
    match a.kind {
        Prefix::Goal => {
            let rank = a.rank.unwrap_or(st.goals.len() as u32 + 1);
            if rank < 1 {
                return Err(bad("rank must be an integer >= 1"));
            }
            let maps = a.maps.clone().unwrap_or_default();
            let status = GoalStatus::Active;
            let (id, title, extra) = (id.clone(), title.clone(), Extra::new());
            st.goals.push(Goal {
                id,
                rank,
                title,
                finish,
                status,
                maps,
                extra,
            });
        }
        Prefix::Task => {
            check_goal(st, &goal)?;
            let status = match a.status.as_deref().filter(|s| !s.is_empty()) {
                None | Some("open") => TaskStatus::Open,
                Some("running") => TaskStatus::Running,
                Some(_) => return Err(bad("new task status must be open or running")),
            };
            let (id, title, to) = (id.clone(), title.clone(), a.to.clone().unwrap_or_default());
            let (outcome, extra) = (String::new(), Extra::new());
            st.tasks.push(Task {
                id,
                goal,
                title,
                status,
                to,
                outcome,
                extra,
            });
        }
        _ => {
            check_goal(st, &goal)?;
            let kind = if a.kind == Prefix::Decision {
                ItemKind::Question
            } else {
                ItemKind::Action
            };
            let (id, status, title) = (id.clone(), ItemStatus::Open, title.clone());
            let (answer, outcome, esc, extra) = (String::new(), String::new(), None, Extra::new());
            st.items.push(Item {
                id,
                kind,
                status,
                goal,
                title,
                opts,
                answer,
                outcome,
                esc,
                extra,
            });
        }
    }
    let drift = a.kind == Prefix::Decision && title.to_lowercase().starts_with("drift:");
    Ok(event(&id, if drift { "drift" } else { "open" }, &title))
}

// #10235: only a live id collides; an archived id is free to reuse.
fn explicit_id(st: &State, kind: Prefix, x: &str) -> Result<String, LedgerError> {
    let id = LedgerId::parse(x)
        .ok()
        .filter(|id| id.code() == &st.code && id.prefix() == kind)
        .ok_or_else(|| bad(format!("bad id {x:?} for {}", kind.letter())))?;
    let live = st
        .goals
        .iter()
        .map(|g| &g.id)
        .chain(st.items.iter().map(|i| &i.id));
    if live.chain(st.tasks.iter().map(|t| &t.id)).any(|l| l == x) {
        return Err(bad(format!("id collision: {x} exists")));
    }
    Ok(x.to_owned())
}

fn check_goal(st: &State, goal: &str) -> Result<(), LedgerError> {
    let g = st
        .goals
        .iter()
        .find(|g| g.id == goal)
        .filter(|_| LedgerId::parse(goal).is_ok_and(|id| id.prefix() == Prefix::Goal));
    match g {
        None => Err(bad(format!("goal {goal:?} is not a goal of this project"))),
        Some(g) if g.status != GoalStatus::Active => Err(bad(format!(
            "goal {goal} is {}, not active",
            name(&g.status)
        ))),
        Some(_) => Ok(()),
    }
}

/// A status enum's text.
fn name<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A record found by id, borrowed for an update.
enum Found<'a> {
    Goal(&'a mut Goal),
    Item(&'a mut Item),
    Task(&'a mut Task),
}

fn find<'a>(st: &'a mut State, id: &str) -> Result<Found<'a>, LedgerError> {
    let code = st.code.as_str().to_owned();
    let bad_id = || bad(format!("bad id {id:?}; expected {code}-<G|D|A|T><hex2>"));
    let parsed = LedgerId::parse(id).map_err(|_| bad_id())?;
    if parsed.code().as_str() != code || parsed.prefix() == Prefix::Brief {
        return Err(bad_id());
    }
    let found = match parsed.prefix() {
        Prefix::Goal => st.goals.iter_mut().find(|g| g.id == id).map(Found::Goal),
        Prefix::Task => st.tasks.iter_mut().find(|t| t.id == id).map(Found::Task),
        _ => st.items.iter_mut().find(|i| i.id == id).map(Found::Item),
    };
    found.ok_or_else(|| bad(format!("unknown id {id}")))
}

fn apply_update(st: &mut State, u: &UpdateOp) -> Result<Vec<BatchEvent>, LedgerError> {
    let mut found = find(st, &u.id)?;
    let mut evs = Vec::new();
    if let Some(t) = &u.title {
        let t = check_title(t)?.to_owned();
        match &mut found {
            Found::Goal(g) => g.title = t,
            Found::Item(i) => i.title = t,
            Found::Task(k) => k.title = t,
        }
    }
    if let Some(rank) = u.rank {
        match &mut found {
            Found::Goal(g) if rank >= 1 => g.rank = rank,
            _ => return Err(bad("rank applies to goals, integer >= 1")),
        }
        evs.push("reprio");
    }
    if let Some(esc) = &u.esc {
        match &mut found {
            Found::Item(i) if esc.is_empty() || esc == "architect" => {
                i.esc = (!esc.is_empty()).then(|| esc.clone());
            }
            _ => return Err(bad("esc applies to D/A, value 'architect' or ''")),
        }
        if !esc.is_empty() {
            evs.push("escalated");
        }
    }
    if let Some(answer) = &u.answer {
        let Found::Item(i) = &mut found else {
            return Err(bad("answer applies to D/A"));
        };
        i.answer = check_paragraph("answer", answer, PARAGRAPH_MAX)?.to_owned();
        i.status = ItemStatus::Answered;
        evs.push("answered");
    }
    if let Some(to) = &u.to {
        let Found::Task(k) = &mut found else {
            return Err(bad("to applies to tasks"));
        };
        k.to = to.clone();
    }
    if let Some(outcome) = &u.outcome {
        let o = check_paragraph("outcome", outcome, PARAGRAPH_MAX)?.to_owned();
        match &mut found {
            Found::Goal(_) => return Err(bad("outcome does not apply to goals")),
            Found::Item(i) => i.outcome = o,
            Found::Task(k) => k.outcome = o,
        }
    }
    if let Some(status) = &u.status {
        set_status(&mut found, status)?;
        if status == "done" {
            evs.push("done");
        }
    }
    let (id, title) = match &found {
        Found::Goal(g) => (&g.id, &g.title),
        Found::Item(i) => (&i.id, &i.title),
        Found::Task(k) => (&k.id, &k.title),
    };
    let note = u.note.as_deref().filter(|n| !n.is_empty()).unwrap_or(title);
    if evs.is_empty() {
        evs.push("update");
    }
    Ok(evs.into_iter().map(|ev| event(id, ev, note)).collect())
}

fn set_status(found: &mut Found<'_>, status: &str) -> Result<(), LedgerError> {
    let parse = |letter: &str, allowed: [&str; 3]| {
        allowed.contains(&status).then_some(()).ok_or_else(|| {
            bad(format!(
                "status for {letter} must be one of ('{}', '{}', '{}')",
                allowed[0], allowed[1], allowed[2]
            ))
        })
    };
    let outcome_missing = match found {
        Found::Item(i) => i.outcome.is_empty(),
        Found::Task(k) => k.outcome.is_empty(),
        Found::Goal(_) => false,
    };
    let value = serde_json::Value::String(status.to_owned());
    match found {
        Found::Goal(g) => {
            parse("G", ["active", "done", "parked"])?;
            g.status = serde_json::from_value(value)?;
            return Ok(());
        }
        Found::Item(i) => {
            parse(
                &i.id[i.id.len() - 3..i.id.len() - 2],
                ["open", "answered", "done"],
            )?;
        }
        Found::Task(_) => parse("T", ["open", "running", "done"])?,
    }
    if status == "done" && outcome_missing {
        return Err(bad("done needs a verified one-line outcome"));
    }
    match found {
        Found::Item(i) => i.status = serde_json::from_value(value)?,
        Found::Task(k) => k.status = serde_json::from_value(value)?,
        Found::Goal(_) => {}
    }
    Ok(())
}

/// Removes the items and tasks a `done` event closed; returns them.
pub(crate) fn collect_closed(st: &mut State, events: &[BatchEvent]) -> Vec<Archived> {
    let done = |id: &str| events.iter().any(|e| e.ev == "done" && e.id == id);
    let mut out = Vec::new();
    let (items, keep): (Vec<Item>, Vec<Item>) = std::mem::take(&mut st.items)
        .into_iter()
        .partition(|i| done(&i.id) && i.status == ItemStatus::Done);
    st.items = keep;
    out.extend(items.into_iter().map(Archived::Item));
    let (tasks, keep): (Vec<Task>, Vec<Task>) = std::mem::take(&mut st.tasks)
        .into_iter()
        .partition(|t| done(&t.id) && t.status == TaskStatus::Done);
    st.tasks = keep;
    out.extend(tasks.into_iter().map(Archived::Task));
    out
}

/// Removes every done item and task, and every answered item whose last
/// answer is at least `hours` old (an id with no `answered` line is old);
/// with `require_outcome`, an answered item needs an outcome.
pub(crate) fn prune_state(
    st: &mut State,
    answered: &BTreeMap<String, f64>,
    hours: f64,
    require_outcome: bool,
    now: f64,
) -> Vec<Archived> {
    let old = |id: &str| answered.get(id).is_none_or(|t| now - t >= hours * 3600.0);
    let (gone, keep): (Vec<Item>, Vec<Item>) =
        std::mem::take(&mut st.items).into_iter().partition(|i| {
            i.status == ItemStatus::Done
                || (i.status == ItemStatus::Answered
                    && old(&i.id)
                    && (!i.outcome.is_empty() || !require_outcome))
        });
    st.items = keep;
    let mut out: Vec<Archived> = gone.into_iter().map(Archived::Item).collect();
    let (gone, keep): (Vec<Task>, Vec<Task>) = std::mem::take(&mut st.tasks)
        .into_iter()
        .partition(|t| t.status == TaskStatus::Done);
    st.tasks = keep;
    out.extend(gone.into_iter().map(Archived::Task));
    out
}
