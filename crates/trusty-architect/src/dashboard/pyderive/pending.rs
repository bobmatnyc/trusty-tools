//! `dashboard_derive.py`'s owner events: `pending` and `request_queue` (#12029 P2b).
//!
//! Why: these events wake the Architect; their kinds, keys, key order and
//! list order are the contract the page and the Architect read.
//! What: [`pending`] concatenates items, sessions, effort_tunes, il_steers,
//! goal parks, goal done, goal kind, goal steers, goal briefs and
//! instructions, each source in id order (a missing collection yields
//! nothing). [`request_queue`] keeps the kinds in [`REQUEST_LABELS`], adds
//! `label` and sorts by `(at, id, kind)`. Every text is Python's `str()`.
//! Test: `dashboard_derive_recordings_replay_clean_against_the_port` (`pending`).

use super::{
    PySnap, building_brief, coll, dict, done, is_selftest, list, relayed, text, text_or, ts,
};
use crate::dashboard::derive::now_micros;
use crate::pyjson_load::{PyInt, PyValue};
use crate::pystr::strip;
use crate::pyval::{get, has, int_text, is_str, py_str};
use crate::tasks::store_read::PyDoc;

/// `REQUEST_LABELS`: the request kinds and the queue's label for each.
pub(crate) const REQUEST_LABELS: [(&str, &str); 8] = [
    ("owner_status_update", "status update"),
    ("brief_request", "briefing"),
    ("status_request", "status request"),
    ("resume_request", "resume"),
    ("steer", "steer"),
    ("feedback", "feedback"),
    ("goal_kind", "kind change"),
    ("ask", "added from page"),
];

type Ev = Vec<(String, PyValue)>;

fn ev(pairs: &[(&str, String)]) -> Ev {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), PyValue::Str(v.clone())))
        .collect()
}

fn by_id(docs: &[PyDoc]) -> Vec<&PyDoc> {
    let mut v: Vec<&PyDoc> = docs.iter().collect();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
}

fn st(d: &PyValue, s: &str) -> bool {
    is_str(get(d, "status"), s)
}

/// `_ad_linked(data)`: an IL task id, and a session or a non-empty project.
fn ad_linked(d: &PyValue) -> bool {
    let projects = list(d, "projects").iter().any(crate::pyval::truthy);
    !strip(&text_or(d, "task")).is_empty()
        && (!strip(&text_or(d, "session")).is_empty() || projects)
}

/// The answer-state event of an item (the `if/elif` chain of `pending_items`).
fn status_event(d: &PyValue, id: &str) -> Option<Ev> {
    let note = get(d, "answer_note");
    let ack = ["Relayed", "Acknowledged"];
    if ["answered", "cancelled", "wont_do", "interactive"]
        .iter()
        .any(|s| st(d, s))
        && relayed(d)
    {
        return None;
    }
    if st(d, "answered") && !done(note, &["Relayed"]) {
        if ["Architect Do", "Have Architect Do"]
            .iter()
            .any(|a| is_str(get(d, "answer"), a))
        {
            let mut t = String::from("Architect Do");
            if !ad_linked(d) {
                t.push_str(" (needs target resolution)");
            }
            if let Some(PyValue::Str(n)) =
                note.filter(|n| matches!(n, PyValue::Str(s) if !strip(s).is_empty()))
            {
                t.push_str(&format!(": {}", strip(n)));
            }
            let f = |k| text(d, k);
            return Some(ev(&[
                ("id", id.into()),
                ("kind", "architect_do".into()),
                ("at", f("answered_at")),
                ("title", f("title")),
                ("task", f("task")),
                ("session", f("session")),
                ("text", t),
            ]));
        }
        let mut e = ev(&[
            ("id", id.into()),
            ("kind", "answered".into()),
            ("at", text(d, "answered_at")),
            ("text", text(d, "answer")),
        ]);
        if let Some(PyValue::List(subs)) = get(d, "subs") {
            let subs = subs
                .iter()
                .filter(|x| matches!(x, PyValue::Dict(_)))
                .map(|x| {
                    PyValue::Dict(ev(&[
                        ("key", text(x, "key")),
                        ("answer", text(x, "answer")),
                        ("note", text(x, "note")),
                    ]))
                })
                .collect();
            e.push(("subs".into(), PyValue::List(subs)));
        }
        return Some(e);
    }
    if st(d, "cancelled") && !done(note, &ack) {
        return Some(ev(&[
            ("id", id.into()),
            ("kind", "cancelled".into()),
            ("at", text(d, "cancelled_at")),
            ("text", text(d, "cancel_note")),
        ]));
    }
    if st(d, "wont_do") && !done(note, &ack) {
        let empty = PyValue::Dict(Vec::new());
        let wd = dict(d, "wont_do").unwrap_or(&empty);
        let why = strip(&text(wd, "note")).to_owned();
        let why = if why.is_empty() {
            "no reason given".to_owned()
        } else {
            why
        };
        return Some(ev(&[
            ("id", id.into()),
            ("kind", "wont_do".into()),
            ("at", text(wd, "at")),
            ("text", why),
        ]));
    }
    if st(d, "interactive") && !done(note, &ack) {
        return Some(ev(&[
            ("id", id.into()),
            ("kind", "interactive".into()),
            ("at", text(d, "answered_at")),
            ("text", text(d, "title")),
        ]));
    }
    None
}

/// The park events of an open item: an `until` that is due, and an unanswered `clarify`.
fn park_events(d: &PyValue, id: &str, now: i64, out: &mut Vec<Ev>) {
    let Some(pk) = dict(d, "parked").filter(|_| st(d, "open")) else {
        return;
    };
    let ups: Vec<&PyValue> = list(d, "updates")
        .iter()
        .filter(|u| matches!(u, PyValue::Dict(_)))
        .collect();
    let since = text(pk, "at");
    let after = |u: &PyValue| text(u, "at") > since;
    if has(pk, "until") && ts(get(pk, "until")).is_some_and(|t| t.utc <= now) {
        let n = ups.iter().filter(|u| after(u)).count();
        let mut e = ev(&[
            ("id", id.into()),
            ("kind", "owner_unpark_due".into()),
            ("at", text(pk, "until")),
            ("text", format!("{n} updates since parked")),
        ]);
        e.push(("updates".into(), PyValue::Int(PyInt::Small(n as i64))));
        out.push(e);
    }
    if is_str(get(pk, "choice"), "clarify")
        && !has(pk, "relayed_at")
        && !ups.iter().any(|u| after(u))
    {
        let empty = PyValue::Dict(Vec::new());
        let cl = dict(pk, "clarify").unwrap_or(&empty);
        let dim = get(cl, "dimension")
            .filter(|v| crate::pyval::truthy(v))
            .map_or("general".to_owned(), py_str);
        let t = match get(cl, "option").filter(|v| crate::pyval::truthy(v)) {
            Some(opt) => format!("{dim} ({})", py_str(opt)),
            None => "general".into(),
        };
        out.push(ev(&[
            ("id", id.into()),
            ("kind", "clarify_request".into()),
            ("at", text(pk, "at")),
            ("text", t),
        ]));
    }
}

/// `pending_items(items, now)`.
fn items(docs: &[PyDoc], now: i64, out: &mut Vec<Ev>) {
    for doc in by_id(docs) {
        if is_selftest(doc) {
            continue;
        }
        let (d, id) = (&doc.data, doc.id.as_str());
        out.extend(status_event(d, id));
        for act in list(d, "owner_actions") {
            let action = ["status_update", "park"]
                .into_iter()
                .find(|a| is_str(get(act, "action"), a));
            let Some(action) = action.filter(|_| has(act, "at") && !has(act, "ack_at")) else {
                continue;
            };
            let t = match (
                action,
                get(act, "until").filter(|v| crate::pyval::truthy(v)),
            ) {
                ("status_update", _) => "status update requested".to_owned(),
                (_, Some(u)) => format!("until {}", py_str(u)),
                _ => "until unparked".to_owned(),
            };
            out.push(ev(&[
                ("id", id.into()),
                ("kind", format!("owner_{action}")),
                ("at", text(act, "at")),
                ("text", t),
                ("task", text(d, "task")),
                ("title", text(d, "title")),
            ]));
        }
        park_events(d, id, now, out);
        for field in ["status_request", "resume_request"] {
            if let Some(req) = dict(d, field).filter(|r| st(d, "open") && has(r, "at")) {
                out.push(ev(&[
                    ("id", id.into()),
                    ("kind", field.into()),
                    ("at", text(req, "at")),
                    ("text", text(d, "title")),
                ]));
            }
        }
        let (nr, nc) = (
            text_or(d, "nudge_requested_at"),
            text_or(d, "nudge_cleared_at"),
        );
        if st(d, "open") && !nr.is_empty() && (nc.is_empty() || nc < nr) {
            out.push(ev(&[
                ("id", id.into()),
                ("kind", "nudge".into()),
                ("at", nr),
                ("text", text(d, "title")),
                ("session", text(d, "session")),
            ]));
        }
        let (sr, sa) = (text_or(d, "status_requested_at"), text_or(d, "status_at"));
        if st(d, "open") && !sr.is_empty() && (sa.is_empty() || sa < sr) {
            out.push(ev(&[
                ("id", id.into()),
                ("kind", "status_request".into()),
                ("at", sr),
                ("text", text(d, "title")),
                ("via", "status_requested_at".into()),
            ]));
        }
        if (st(d, "open") || st(d, "brief_pending")) && building_brief(d) {
            let at = dict(d, "brief_request").map_or(String::new(), |r| text(r, "at"));
            out.push(ev(&[
                ("id", id.into()),
                ("kind", "brief_request".into()),
                ("at", at),
                ("text", text(d, "title")),
            ]));
        }
        steers(d, id, out);
        if let Some(oc) =
            dict(d, "owner_complete").filter(|o| st(d, "done") && has(o, "at") && !has(o, "ack_at"))
        {
            out.push(ev(&[
                ("id", id.into()),
                ("kind", "owner_complete".into()),
                ("at", text(oc, "at")),
                ("text", text(d, "title")),
                ("task", text(d, "task")),
            ]));
        }
        if let Some(fb) = dict(d, "feedback").filter(|f| is_str(get(f, "status"), "new")) {
            out.push(ev(&[
                ("id", id.into()),
                ("kind", "feedback".into()),
                ("at", text(fb, "at")),
                ("text", text(fb, "note")),
                ("to", text(fb, "to")),
            ]));
        }
    }
}

/// Each `steer_requests` entry with text and no `relayed_at`.
fn steers(d: &PyValue, id: &str, out: &mut Vec<Ev>) {
    for s in list(d, "steer_requests") {
        if has(s, "text") && !has(s, "relayed_at") {
            out.push(ev(&[
                ("id", id.into()),
                ("kind", "steer".into()),
                ("at", text(s, "at")),
                ("text", text(s, "text")),
            ]));
        }
    }
}

fn efforts(docs: &[PyDoc], out: &mut Vec<Ev>) {
    for doc in by_id(docs) {
        let d = &doc.data;
        if !st(d, "new") {
            continue;
        }
        let b = match get(d, "bias_after") {
            Some(PyValue::Int(i)) => {
                let t = int_text(i);
                if t.starts_with('-') {
                    t
                } else {
                    format!("+{t}")
                }
            }
            _ => "?".to_owned(),
        };
        let (goal, dir) = (text(d, "goal"), text(d, "direction"));
        out.push(ev(&[
            ("id", doc.id.clone()),
            ("kind", "effort".into()),
            ("at", text(d, "at")),
            ("text", format!("{goal} effort {dir} (bias {b})")),
            ("goal", goal),
            ("direction", dir),
            ("bias", b),
        ]));
    }
}

fn instructions(docs: &[PyDoc], out: &mut Vec<Ev>) {
    for doc in by_id(docs) {
        let Some(a) = dict(&doc.data, "owner_action").filter(|a| is_str(get(a, "status"), "new"))
        else {
            continue;
        };
        let what = text(a, "type");
        let body = get(a, "text")
            .filter(|v| crate::pyval::truthy(v))
            .or_else(|| get(a, "project").filter(|v| crate::pyval::truthy(v)))
            .map_or(String::new(), py_str);
        let t = format!("{what}: {}", strip(&body));
        let t = t.trim_end_matches([':', ' ']).to_owned();
        out.push(ev(&[
            ("id", doc.id.clone()),
            ("kind", "owner_instruction".into()),
            ("at", text(a, "at")),
            ("type", what),
            ("text", t),
        ]));
    }
}

fn il_steers(docs: &[PyDoc], out: &mut Vec<Ev>) {
    for doc in by_id(docs) {
        let d = &doc.data;
        if !st(d, "new") || !has(d, "text") {
            continue;
        }
        let id = doc.id.clone();
        if has(d, "ask_type") {
            out.push(ev(&[
                ("id", id),
                ("kind", "ask".into()),
                ("at", text(d, "at")),
                ("type", text(d, "ask_type")),
                ("text", text(d, "text")),
                ("by", text(d, "by")),
                ("filed_as", text(d, "filed_as")),
                ("dash", text(d, "dash")),
            ]));
        } else {
            out.push(ev(&[
                ("id", id),
                ("kind", "steer".into()),
                ("at", text(d, "at")),
                ("text", text(d, "text")),
                ("task", text(d, "task")),
                ("session", text(d, "session")),
            ]));
        }
    }
}

fn commands(sessions: &[PyDoc], out: &mut Vec<Ev>) {
    for doc in by_id(sessions) {
        let Some(cmd) = dict(&doc.data, "command").filter(|c| is_str(get(c, "status"), "new"))
        else {
            continue;
        };
        let ctype = text(cmd, "type");
        let key = if ctype == "activate" { "text" } else { "note" };
        out.push(ev(&[
            ("id", doc.id.clone()),
            ("kind", "owner_command".into()),
            ("at", text(cmd, "at")),
            ("text", text_or(cmd, key)),
            ("type", ctype),
            ("goal", text(cmd, "goal")),
            ("session", text(&doc.data, "name")),
        ]));
    }
}

fn goals(docs: &[PyDoc], out: &mut Vec<Ev>) {
    let sorted = by_id(docs);
    for doc in &sorted {
        let (d, id) = (&doc.data, doc.id.clone());
        if has(d, "park_ack") {
            continue;
        }
        let title = text(d, "title");
        if let Some(pk) = dict(d, "parked") {
            let t = match get(pk, "until").filter(|v| crate::pyval::truthy(v)) {
                Some(u) => format!("parked until {}", py_str(u)),
                None => format!("until unparked: {title}"),
            };
            out.push(ev(&[
                ("id", id),
                ("kind", "goal_park".into()),
                ("at", text(pk, "at")),
                ("text", t),
            ]));
        } else if has(d, "unparked_at") {
            out.push(ev(&[
                ("id", id),
                ("kind", "goal_unpark".into()),
                ("at", text(d, "unparked_at")),
                ("text", title),
            ]));
        }
    }
    for doc in &sorted {
        let d = &doc.data;
        if is_str(get(d, "lifecycle"), "done") && has(d, "done_at") && !has(d, "done_ack") {
            let note = strip(&text(d, "done_note")).to_owned();
            let mut t = text(d, "title");
            if !note.is_empty() {
                t.push_str(&format!(" \u{2014} {note}"));
            }
            out.push(ev(&[
                ("id", doc.id.clone()),
                ("kind", "goal_done".into()),
                ("at", text(d, "done_at")),
                ("text", t),
            ]));
        }
    }
    for doc in &sorted {
        let d = &doc.data;
        if is_str(get(d, "kind_set_by"), "owner") && has(d, "kind_set_at") && !has(d, "kind_ack") {
            let t = format!("{} -> {}", text(d, "title"), text(d, "kind"));
            out.push(ev(&[
                ("id", doc.id.clone()),
                ("kind", "goal_kind".into()),
                ("at", text(d, "kind_set_at")),
                ("text", t),
            ]));
        }
    }
    for doc in &sorted {
        steers(&doc.data, &doc.id, out);
    }
    for doc in &sorted {
        if building_brief(&doc.data) {
            let at = dict(&doc.data, "brief_request").map_or(String::new(), |r| text(r, "at"));
            out.push(ev(&[
                ("id", doc.id.clone()),
                ("kind", "brief_request".into()),
                ("at", at),
                ("text", text(&doc.data, "title")),
            ]));
        }
    }
}

/// Why: the owner events the Architect must act on.
/// What: `pending(snap, now)`; see the module doc. `now` is Unix seconds.
/// Test: `dashboard_derive_recordings_replay_clean_against_the_port`.
pub(crate) fn pending(snap: &PySnap, now: f64) -> Vec<PyValue> {
    let mut out = Vec::new();
    items(coll(snap, "items"), now_micros(now), &mut out);
    commands(coll(snap, "sessions"), &mut out);
    efforts(coll(snap, "effort_tunes"), &mut out);
    il_steers(coll(snap, "il_steers"), &mut out);
    goals(coll(snap, "goals"), &mut out);
    instructions(coll(snap, "instructions"), &mut out);
    out.into_iter().map(PyValue::Dict).collect()
}

/// `request_queue(snap, now)`: the pending owner requests, labelled, oldest first.
pub(crate) fn request_queue(snap: &PySnap, now: f64) -> Vec<PyValue> {
    let field = |e: &PyValue, k: &str| get(e, k).map_or(String::new(), py_str);
    let mut out: Vec<PyValue> = pending(snap, now)
        .into_iter()
        .filter_map(|e| {
            let kind = field(&e, "kind");
            let label = REQUEST_LABELS.iter().find(|(k, _)| *k == kind)?.1;
            let PyValue::Dict(mut pairs) = e else {
                return None;
            };
            pairs.push(("label".into(), PyValue::Str(label.into())));
            Some(PyValue::Dict(pairs))
        })
        .collect();
    out.sort_by(|a, b| {
        (field(a, "at"), field(a, "id"), field(a, "kind")).cmp(&(
            field(b, "at"),
            field(b, "id"),
            field(b, "kind"),
        ))
    });
    out
}
