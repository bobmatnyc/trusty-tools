//! The write ops of `datg.py`: add, update, close, pulse.
//!
//! Why: every write verb is an op applied to an in-memory copy of the state
//! under the lock and committed once. The op bodies carry the ledger's rules
//! (limits, status moves, WIP cap, evidence warning), so they are ported
//! statement by statement, keeping the order keys are inserted in.
//! What: [`Ctx`] (clock, warnings, `$DATG_SESSION`), [`Ev`] (a log event),
//! [`apply_op`] and the `apply_*` bodies. Ops are `Json` objects, the same shape
//! as `datg.py batch`. Update and pulse live in `ops_update.rs`, session and
//! reminder ops in `session.rs` and `ops_remind.rs`.
//! Test: `golden_steps_match_python_byte_for_byte`, `closed_ids_are_reused`,
//! `batch_input_must_be_a_list`.

use super::error::{IlResult, invalid};
use super::ids::{next_id, parse_id};
use super::json::{Json, obj};
use super::limits::{lim, py_or, title};
use super::ops_update::{apply_pulse, apply_update};
use super::wip::{rebalance, wip_check};
use crate::Clock;
use crate::pystr::{py_repr, strip};

/// Statuses a goal may have.
const ONE_SHOT: [&str; 4] = ["relay", "answer", "ack", "check"];
const ADHOC_TITLE: &str = "Handle ad-hoc tasks (standing)";

/// What an op needs from its caller: the clock, `$DATG_SESSION`, and the stderr notes collected.
pub struct Ctx<'a> {
    /// Wall time.
    pub clock: &'a dyn Clock,
    /// `$DATG_SESSION`, empty when unset.
    pub env_session: String,
    /// `WARNINGS`: printed on stderr as `datg: <msg>` after a successful write.
    pub warnings: Vec<String>,
}

impl Ctx<'_> {
    /// Whole seconds since the epoch.
    pub fn epoch(&self) -> i64 {
        self.clock.now_unix().floor() as i64
    }

    /// `now_iso()`: `%Y-%m-%dT%H:%MZ`.
    pub fn now_iso(&self) -> String {
        super::time::fmt_min(self.epoch())
    }
}

/// A log event an op produced.
#[derive(Debug, Clone, Default)]
pub struct Ev {
    /// The id the event is about.
    pub id: String,
    /// `open`, `update`, `done`, `answered`, `escalated`, `reprio`, `drift`, or `act`.
    pub ev: String,
    /// The log note; empty means none.
    pub note: String,
    /// `remind_fire`: the task title.
    pub task: Option<String>,
    /// `remind_fire`: the fired reminder with `fired_for`.
    pub reminder: Option<Json>,
}

pub(super) fn ev(id: &str, kind: &str, note: impl Into<String>) -> Ev {
    Ev {
        id: id.to_owned(),
        ev: kind.to_owned(),
        note: note.into(),
        ..Ev::default()
    }
}

/// The bucket an id prefix lives in.
pub(super) fn bucket(pre: char) -> &'static str {
    match pre {
        'G' => "goals",
        'T' => "tasks",
        _ => "items",
    }
}

/// `_find`: the kind and `(bucket, index)` of a live id.
///
/// # Errors
/// `Invalid` for a bad or unknown id.
pub(super) fn find(st: &Json, full_id: Option<&Json>) -> IlResult<(char, usize)> {
    let id = full_id
        .filter(|j| j.truthy())
        .map(Json::py_str)
        .unwrap_or_default();
    let code = st.s("code");
    let Some((_, pre, _)) = parse_id(&id).filter(|(c, ..)| *c == code) else {
        return invalid(format!(
            "bad id {}; expected {code}-<G|D|A|T><2 base-34 digits: 0-9, a-z without o and l>",
            py_repr(&id)
        ));
    };
    match st.arr(bucket(pre)).iter().position(|o| o.s("id") == id) {
        Some(i) => Ok((pre, i)),
        None => invalid(format!(
            "unknown id {id} (not live; a closed id is archived: datg.py archive-show {id})"
        )),
    }
}

pub(super) fn slot(st: &mut Json, pre: char, i: usize) -> &mut Json {
    &mut st.arr_or_insert(bucket(pre))[i]
}

fn adhoc_goal(st: &Json) -> Option<&Json> {
    st.arr("goals")
        .iter()
        .find(|g| g.s("status") == "active" && g.s("title").starts_with("Handle ad-hoc tasks"))
}

/// `ensure_adhoc_goal`: the id of the ad-hoc goal, created when the ledger lacks one.
fn ensure_adhoc_goal(st: &mut Json) -> IlResult<String> {
    if let Some(g) = adhoc_goal(st) {
        return Ok(g.s("id").to_owned());
    }
    let id = next_id(st, 'G', None)?;
    let rank = st
        .arr("goals")
        .iter()
        .filter_map(|g| g.i("rank"))
        .max()
        .unwrap_or(0)
        + 1;
    st.arr_or_insert("goals").push(obj([
        ("id", id.clone().into()),
        ("rank", rank.into()),
        ("title", ADHOC_TITLE.into()),
        (
            "finish",
            "standing: collects tasks that meet no other goal".into(),
        ),
        ("status", "active".into()),
        ("maps", "".into()),
    ]));
    Ok(id)
}

fn check_goal(st: &Json, goal: Option<&Json>) -> IlResult<()> {
    let name = goal
        .filter(|j| j.truthy())
        .map(Json::py_str)
        .unwrap_or_default();
    let g = parse_id(&name)
        .filter(|(_, p, _)| *p == 'G')
        .and_then(|_| st.arr("goals").iter().find(|g| g.s("id") == name));
    let Some(g) = g else {
        let shown = match goal {
            Some(Json::Str(s)) => py_repr(s),
            Some(j) => j.py_str(),
            None => "None".to_owned(),
        };
        return invalid(format!("goal {shown} is not a goal of this project"));
    };
    if g.s("status") != "active" {
        return invalid(format!("goal {name} is {}, not active", g.s("status")));
    }
    Ok(())
}

fn check_opts(opts: Option<&Json>) -> IlResult<Json> {
    let opts = opts
        .filter(|j| j.truthy())
        .cloned()
        .unwrap_or_else(|| Json::Arr(Vec::new()));
    let long = opts
        .as_arr()
        .unwrap_or(&[])
        .iter()
        .any(|o| o.s("l").chars().count() > 160);
    if opts.as_arr().map_or(0, <[Json]>::len) > 4 || long {
        return invalid("opts: at most 4 options, each label <= 160 chars");
    }
    Ok(opts)
}

fn one_shot_check(title: &str, a: &Json) -> IlResult<()> {
    let lower = title.to_lowercase();
    let w: String = strip(&lower)
        .chars()
        .take_while(|c| crate::pystr::is_word(*c))
        .collect();
    if ONE_SHOT.contains(&w.as_str()) && !a.get("multistep").is_some_and(Json::truthy) {
        let head: String = title.chars().take(40).collect();
        return invalid(format!(
            "a task is multi-step artifact/research work; {} reads as a one-shot ({w}). Record it as a Q (answer) or an `act:` line, or pass --multistep (ilio: `multistep`) if it truly is multi-step",
            py_repr(&head)
        ));
    }
    Ok(())
}

fn truthy_or<'a>(a: &'a Json, k: &str, default: &'a str) -> Json {
    a.get(k)
        .filter(|j| j.truthy())
        .cloned()
        .unwrap_or_else(|| default.into())
}

/// `apply_add`: adds a goal, decision, action or task.
///
/// # Errors
/// `Invalid` with `datg.py`'s message.
pub fn apply_add(st: &mut Json, a: &Json, ctx: &mut Ctx) -> IlResult<Ev> {
    let pre = match a.s("kind") {
        "G" => 'G',
        "D" => 'D',
        "A" => 'A',
        "T" => 'T',
        _ => return invalid("kind must be G, D, A or T"),
    };
    let title = title(a.get("title"))?;
    let new_id = next_id(st, pre, a.get("id").and_then(Json::as_str))?;
    if pre == 'G' {
        let rank = match a.opt("rank") {
            None => st.arr("goals").len() as i64 + 1,
            Some(Json::Int(n)) if *n >= 1 => *n,
            Some(_) => return invalid("rank must be an integer >= 1"),
        };
        let g = obj([
            ("id", new_id.clone().into()),
            ("rank", rank.into()),
            ("title", title.clone().into()),
            (
                "finish",
                lim(a.get("finish"), "finish", 160, false, false)?.into(),
            ),
            ("status", "active".into()),
            ("maps", truthy_or(a, "maps", "")),
        ]);
        st.arr_or_insert("goals").push(g);
    } else {
        let mut goal = a.get("goal").cloned();
        let known = goal
            .as_ref()
            .and_then(Json::as_str)
            .is_some_and(|g| st.arr("goals").iter().any(|x| x.s("id") == g));
        if pre == 'T' && !known {
            goal = Some(ensure_adhoc_goal(st)?.into()); // no goal, or an unknown one: the ad-hoc default goal
        }
        check_goal(st, goal.as_ref())?;
        let goal = goal.unwrap_or(Json::Null);
        if pre == 'T' {
            one_shot_check(&title, a)?;
            let dw = strip(&py_or(a.get("done_when"))).to_owned();
            if dw.is_empty() {
                return invalid(
                    "a new task needs done_when: the acceptance criteria, one line or `;`-separated (datg.py add T --done-when '...'; ilio: done_when=\"...\")",
                );
            }
            let mut t = obj([
                ("id", new_id.clone().into()),
                ("goal", goal),
                ("title", title.clone().into()),
                ("status", truthy_or(a, "status", "open")),
                ("to", truthy_or(a, "to", "")),
                ("outcome", "".into()),
                ("created", ctx.now_iso().into()),
                (
                    "done_when",
                    lim(Some(&Json::from(dw)), "done_when", 300, false, false)?.into(),
                ),
            ]);
            if !matches!(t.s("status"), "open" | "running") {
                return invalid("new task status must be open or running");
            }
            wip_check(st, &new_id, &mut t, &mut ctx.warnings);
            st.arr_or_insert("tasks").push(t);
        } else {
            let mut it = obj([
                ("id", new_id.clone().into()),
                (
                    "kind",
                    if pre == 'D' { "question" } else { "action" }.into(),
                ),
                ("status", "open".into()),
                ("goal", goal),
                ("title", title.clone().into()),
                ("opts", check_opts(a.get("opts"))?),
                ("answer", "".into()),
                ("outcome", "".into()),
                ("esc", Json::Null),
            ]);
            if let Some(d) = a.get("dash").filter(|d| d.truthy()) {
                it.set("dash", d.clone());
            }
            st.arr_or_insert("items").push(it);
        }
    }
    let kind = if pre == 'D' && title.to_lowercase().starts_with("drift:") {
        "drift"
    } else {
        "open"
    };
    Ok(ev(&new_id, kind, title))
}

/// `apply_op`: dispatches one batch op to its body.
///
/// # Errors
/// `Invalid` for an unknown op or any body's refusal.
pub fn apply_op(st: &mut Json, a: &Json, ctx: &mut Ctx) -> IlResult<Vec<Ev>> {
    match a.get("op").and_then(Json::as_str) {
        Some("add") => Ok(vec![apply_add(st, a, ctx)?]),
        Some("update") => apply_update(st, a, ctx),
        Some("session") => Ok(vec![super::session::apply_session(st, a, ctx)?]),
        Some("pulse") => Ok(vec![apply_pulse(st, a)?]),
        Some("remind") => Ok(vec![super::ops_remind::apply_remind(st, a)?]),
        Some("remind_done") => Ok(vec![super::ops_remind::apply_remind_done(st, a, ctx)?]),
        Some("remind_fire") => super::ops_remind::apply_remind_fire(st, a),
        Some("rebalance") => Ok(rebalance(st)
            .into_iter()
            .map(|(id, what)| ev(&id, "update", what))
            .collect()),
        Some("close") => {
            let out = a.get("outcome").filter(|o| o.truthy());
            let Some(out) = out else {
                return invalid("close needs --outcome");
            };
            let u = obj([
                ("id", a.get("id").cloned().unwrap_or(Json::Null)),
                ("outcome", out.clone()),
                ("status", "done".into()),
            ]);
            apply_update(st, &u, ctx)
        }
        other => invalid(format!(
            "unknown op {}",
            other.map_or_else(|| "None".to_owned(), py_repr)
        )),
    }
}

/// `collect_closed`: removes the items and tasks a `done` event closed; returns `(kind, object)` to archive.
pub fn collect_closed(st: &mut Json, evs: &[Ev]) -> Vec<(String, Json)> {
    let ids: Vec<&str> = evs
        .iter()
        .filter(|e| e.ev == "done")
        .map(|e| e.id.as_str())
        .collect();
    let mut out = Vec::new();
    for (field, kind) in [("items", "item"), ("tasks", "task")] {
        let list = st.arr_or_insert(field);
        let (gone, keep): (Vec<Json>, Vec<Json>) = std::mem::take(list)
            .into_iter()
            .partition(|o| ids.contains(&o.s("id")) && o.s("status") == "done");
        *list = keep;
        out.extend(gone.into_iter().map(|o| (kind.to_owned(), o)));
    }
    out
}
