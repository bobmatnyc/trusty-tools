//! Owner events the Architect must act on, and the stale-task list (#10403).
//!
//! Why: `dashboard_derive.py` (`pending`, `stale_tasks`) decides what wakes
//! the Architect; DOC-82 moves dashboard data management into this crate, so
//! the rules move here as pure functions of `(&Snapshot, now)`.
//! What: [`pending`] concatenates seven sources in Python's fixed order (items,
//! sessions, effort_tunes, il_steers, goal parks, goal done, goal briefs), each
//! in id order. [`PendingEvent`] serializes to the Python dict's keys in its
//! order. A missing collection yields nothing. A timestamp Python cannot
//! parse keeps Python's result and is also listed in [`Pending::skipped`].
//! Test: `tests/dashboard_pending.rs`.

mod isotime;
mod items;
mod stale;

use serde::Serialize;
use serde_json::{Map, Value};

use super::ids::{py_str, truthy};
use super::store::{Doc, Snapshot};

pub use stale::{StaleTask, StaleTasks, last_activity, stale_tasks};

/// One owner event, with Python's keys in Python's order; the optional
/// fields appear only on the kinds that carry them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PendingEvent {
    /// Doc id.
    pub id: String,
    /// Event kind (`answered`, `owner_unpark_due`, `goal_park`, ...).
    pub kind: String,
    /// When it happened, as stored.
    pub at: String,
    /// One-line text.
    pub text: String,
    /// `owner_command`: the command type.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub command_type: Option<String>,
    /// `owner_command`, `effort`: the goal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// `effort`: the direction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    /// `effort`: the signed bias, or `?`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bias: Option<String>,
    /// `steer` from `il_steers`: the Intent Ledger task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// `owner_command`, `steer` from `il_steers`: the session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// `owner_unpark_due`: updates since the park.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updates: Option<u64>,
    /// `feedback`: the reclassify target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

impl PendingEvent {
    fn new(id: &str, kind: &str, at: String, text: String) -> Self {
        Self {
            id: id.to_owned(),
            kind: kind.to_owned(),
            at,
            text,
            ..Self::default()
        }
    }
}

/// A timestamp field the rules could not parse (Python's fail-open case).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedField {
    /// Doc id.
    pub id: String,
    /// Field path, e.g. `parked.until` or `updates[2].at`.
    pub field: String,
}

/// [`pending`]'s result: the events (the parity output) and the skipped fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pending {
    events: Vec<PendingEvent>,
    skipped: Vec<SkippedField>,
}

impl Pending {
    /// The events, byte-equal to Python's list once serialized.
    pub fn events(&self) -> &[PendingEvent] {
        &self.events
    }

    /// Unparseable timestamps that suppressed an event; not part of the
    /// parity output.
    pub fn skipped(&self) -> &[SkippedField] {
        &self.skipped
    }

    /// The events, by value.
    pub fn into_events(self) -> Vec<PendingEvent> {
        self.events
    }
}

/// Why: the Architect wakes on these events; Python's `pending(snap, now)` is
/// the reference and its output order is part of the contract (#10403).
/// What: items, sessions, effort_tunes, il_steers, then goals three times
/// (parks, done, briefs); `now` is Unix seconds and is read only to decide
/// whether a `parked.until` is due. No clock or filesystem read.
/// Test: `pending_parity_goldens`, `pending_missing_collections_yield_nothing`,
/// `pending_unparseable_until_is_skipped`.
pub fn pending(snap: &Snapshot, now: f64) -> Pending {
    let coll = |name: &str| snap.get(name).map_or(&[][..], Vec::as_slice);
    let mut out = Pending::default();
    items::pending_items(coll("items"), now, &mut out.events, &mut out.skipped);
    out.events.extend(commands(coll("sessions")));
    out.events.extend(efforts(coll("effort_tunes")));
    out.events.extend(il_steers(coll("il_steers")));
    let goals = coll("goals");
    out.events.extend(goal_parks(goals));
    out.events.extend(goal_done(goals));
    out.events.extend(goal_briefs(goals));
    out
}

/// Python's `sorted(docs, key=lambda x: x["id"])`.
pub(crate) fn by_id(docs: &[Doc]) -> Vec<&Doc> {
    let mut v: Vec<&Doc> = docs.iter().collect();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}

/// `str(d.get(key, ""))`.
pub(crate) fn s(d: &Map<String, Value>, key: &str) -> String {
    d.get(key).map(py_str).unwrap_or_default()
}

/// `bool(d.get(key))`.
pub(crate) fn t(d: &Map<String, Value>, key: &str) -> bool {
    d.get(key).is_some_and(truthy)
}

/// `d.get(key)` when it is a dict.
pub(crate) fn obj<'a>(d: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    d.get(key).and_then(Value::as_object)
}

fn status_is(d: &Map<String, Value>, want: &str) -> bool {
    d.get("status").and_then(Value::as_str) == Some(want)
}

/// `pending_commands`: a session `command` with status `new`.
fn commands(sessions: &[Doc]) -> Vec<PendingEvent> {
    let mut out = Vec::new();
    for doc in by_id(sessions) {
        let Some(cmd) = obj(&doc.data, "command").filter(|c| status_is(c, "new")) else {
            continue;
        };
        let ctype = s(cmd, "type");
        let key = if ctype == "activate" { "text" } else { "note" };
        let text = cmd.get(key).filter(|v| truthy(v)).map(py_str);
        let mut e = PendingEvent::new(
            &doc.id,
            "owner_command",
            s(cmd, "at"),
            text.unwrap_or_default(),
        );
        e.command_type = Some(ctype);
        e.goal = Some(s(cmd, "goal"));
        e.session = Some(s(&doc.data, "name"));
        out.push(e);
    }
    out
}

/// `pending_efforts`: an effort tune with status `new`.
fn efforts(docs: &[Doc]) -> Vec<PendingEvent> {
    let mut out = Vec::new();
    for doc in by_id(docs) {
        let d = &doc.data;
        if !status_is(d, "new") {
            continue;
        }
        let bias = match d.get("bias_after").and_then(Value::as_number) {
            Some(n) if n.is_i64() || n.is_u64() => {
                let v = n.to_string();
                if v.starts_with('-') {
                    v
                } else {
                    format!("+{v}")
                }
            }
            _ => "?".to_owned(),
        };
        let (goal, direction) = (s(d, "goal"), s(d, "direction"));
        let text = format!("{goal} effort {direction} (bias {bias})");
        let mut e = PendingEvent::new(&doc.id, "effort", s(d, "at"), text);
        e.goal = Some(goal);
        e.direction = Some(direction);
        e.bias = Some(bias);
        out.push(e);
    }
    out
}

/// `pending_il_steers`: an Intent Ledger steer with status `new` and text.
fn il_steers(docs: &[Doc]) -> Vec<PendingEvent> {
    let mut out = Vec::new();
    for doc in by_id(docs) {
        let d = &doc.data;
        if status_is(d, "new") && t(d, "text") {
            let mut e = PendingEvent::new(&doc.id, "steer", s(d, "at"), s(d, "text"));
            e.task = Some(s(d, "task"));
            e.session = Some(s(d, "session"));
            out.push(e);
        }
    }
    out
}

/// `pending_goal_parks`: an unacknowledged goal park or unpark.
fn goal_parks(goals: &[Doc]) -> Vec<PendingEvent> {
    let mut out = Vec::new();
    for doc in by_id(goals) {
        let d = &doc.data;
        if t(d, "park_ack") {
            continue;
        }
        let title = s(d, "title");
        if let Some(pk) = obj(d, "parked") {
            let text = match pk.get("until").filter(|u| truthy(u)) {
                Some(u) => format!("parked until {}", py_str(u)),
                None => format!("until unparked: {title}"),
            };
            out.push(PendingEvent::new(&doc.id, "goal_park", s(pk, "at"), text));
        } else if t(d, "unparked_at") {
            out.push(PendingEvent::new(
                &doc.id,
                "goal_unpark",
                s(d, "unparked_at"),
                title,
            ));
        }
    }
    out
}

/// `pending_goal_done`: a goal marked done and not yet acknowledged.
fn goal_done(goals: &[Doc]) -> Vec<PendingEvent> {
    let mut out = Vec::new();
    for doc in by_id(goals) {
        let d = &doc.data;
        let done = d.get("lifecycle").and_then(Value::as_str) == Some("done");
        if done && t(d, "done_at") && !t(d, "done_ack") {
            let note = s(d, "done_note");
            let note = crate::pystr::strip(&note);
            let mut text = s(d, "title");
            if !note.is_empty() {
                text = format!("{text} \u{2014} {note}");
            }
            out.push(PendingEvent::new(
                &doc.id,
                "goal_done",
                s(d, "done_at"),
                text,
            ));
        }
    }
    out
}

/// `brief_request {at}` with no `brief`, or with `refresh: true`.
pub(crate) fn brief_wanted(d: &Map<String, Value>) -> Option<&Map<String, Value>> {
    let req = obj(d, "brief_request")?;
    let refresh = req.get("refresh") == Some(&Value::Bool(true));
    (t(req, "at") && (!t(d, "brief") || refresh)).then_some(req)
}

/// `pending_goal_briefs`: a goal brief request.
fn goal_briefs(goals: &[Doc]) -> Vec<PendingEvent> {
    by_id(goals)
        .into_iter()
        .filter_map(|doc| {
            let req = brief_wanted(&doc.data)?;
            Some(PendingEvent::new(
                &doc.id,
                "brief_request",
                s(req, "at"),
                s(&doc.data, "title"),
            ))
        })
        .collect()
}
