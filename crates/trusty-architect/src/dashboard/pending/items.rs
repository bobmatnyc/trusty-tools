//! `pending_items` from `dashboard_derive.py`: the item-level owner events.
//!
//! Why: an answered, cancelled, won't-do or interactive item, a due park, a
//! clarify chip, a status/resume/brief request, a steer and new feedback each
//! need the Architect (#10403).
//! What: [`pending_items`] walks items in id order and appends events in
//! Python's per-item order. A truthy `parked.until` that does not parse is no
//! event (Python's `_due` is false) and is recorded as skipped.
//! Test: `pending_parity_goldens`, `pending_unparseable_until_is_skipped`.

use serde_json::{Map, Value};

use super::super::derive::now_micros;
use super::super::ids::{py_str, truthy};
use super::isotime::ts;
use super::{PendingEvent, SkippedField, brief_wanted, by_id, obj, s, t};
use crate::dashboard::store::Doc;
use crate::pystr::{lstrip, strip};

/// `_done`: a string note whose left-stripped text starts with a prefix.
fn done(note: Option<&Value>, prefixes: &[&str]) -> bool {
    note.and_then(Value::as_str)
        .is_some_and(|n| prefixes.iter().any(|p| lstrip(n).starts_with(p)))
}

/// `_relayed`: `relayed` is `true` or a non-blank string, or `relayed_at`
/// is a non-blank string (Architect ruling; the store holds all three forms).
/// Test: `relayed_accepts_bool_string_and_relayed_at`.
fn relayed(d: &Map<String, Value>) -> bool {
    let non_blank = |k: &str| {
        d.get(k)
            .and_then(Value::as_str)
            .is_some_and(|r| !strip(r).is_empty())
    };
    d.get("relayed") == Some(&Value::Bool(true)) || non_blank("relayed") || non_blank("relayed_at")
}

/// Appends the owner events of `items` (Python's `pending_items`).
pub(super) fn pending_items(
    items: &[Doc],
    now: f64,
    out: &mut Vec<PendingEvent>,
    skipped: &mut Vec<SkippedField>,
) {
    let now_us = now_micros(now);
    for doc in by_id(items) {
        let (d, id) = (&doc.data, doc.id.as_str());
        status_event(d, id, out);
        let status = d.get("status").and_then(Value::as_str);
        let open = status == Some("open");
        let pk = obj(d, "parked");
        let ups: &[Value] = d
            .get("updates")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice);
        let after = |at: &str| {
            ups.iter()
                .filter_map(Value::as_object)
                .filter(|u| s(u, "at").as_str() > at)
                .count()
        };
        if let (true, Some(pk)) = (open, pk) {
            if let Some(until) = pk.get("until").filter(|u| truthy(u)) {
                // #10403: Python's `_due`; an unparseable until is no event.
                match ts(until) {
                    Some(due) if due <= now_us => {
                        let n = after(&s(pk, "at"));
                        let mut e = PendingEvent::new(
                            id,
                            "owner_unpark_due",
                            py_str(until),
                            format!("{n} updates since parked"),
                        );
                        e.updates = Some(n as u64);
                        out.push(e);
                    }
                    Some(_) => {}
                    None => skipped.push(SkippedField {
                        id: id.to_owned(),
                        field: "parked.until".into(),
                    }),
                }
            }
            let clarify = pk.get("choice").and_then(Value::as_str) == Some("clarify");
            if clarify && !t(pk, "relayed_at") && after(&s(pk, "at")) == 0 {
                let empty = Map::new();
                let cl = obj(pk, "clarify").unwrap_or(&empty);
                let dim = cl
                    .get("dimension")
                    .filter(|v| truthy(v))
                    .map_or_else(|| "general".to_owned(), py_str);
                let text = match cl.get("option").filter(|v| truthy(v)) {
                    Some(opt) => format!("{dim} ({})", py_str(opt)),
                    None => "general".to_owned(),
                };
                out.push(PendingEvent::new(id, "clarify_request", s(pk, "at"), text));
            }
        }
        for field in ["status_request", "resume_request"] {
            if let (true, Some(req)) = (open, obj(d, field))
                && t(req, "at")
            {
                out.push(PendingEvent::new(id, field, s(req, "at"), s(d, "title")));
            }
        }
        if matches!(status, Some("open" | "brief_pending"))
            && let Some(req) = brief_wanted(d)
        {
            out.push(PendingEvent::new(
                id,
                "brief_request",
                s(req, "at"),
                s(d, "title"),
            ));
        }
        if let Some(list) = d.get("steer_requests").and_then(Value::as_array) {
            for st in list.iter().filter_map(Value::as_object) {
                if t(st, "text") && !t(st, "relayed_at") {
                    out.push(PendingEvent::new(id, "steer", s(st, "at"), s(st, "text")));
                }
            }
        }
        if let Some(fb) = obj(d, "feedback")
            && fb.get("status").and_then(Value::as_str) == Some("new")
        {
            let mut e = PendingEvent::new(id, "feedback", s(fb, "at"), s(fb, "note"));
            e.to = Some(s(fb, "to"));
            out.push(e);
        }
    }
}

/// The status-driven event (answered, architect_do, cancelled, wont_do,
/// interactive), unless the item is relayed or its note says so.
fn status_event(d: &Map<String, Value>, id: &str, out: &mut Vec<PendingEvent>) {
    let status = d.get("status").and_then(Value::as_str);
    let note = d.get("answer_note");
    let closed = ["Relayed", "Acknowledged"];
    let relayable = matches!(
        status,
        Some("answered" | "cancelled" | "wont_do" | "interactive")
    );
    if relayable && relayed(d) {
        return;
    }
    let event = match status {
        Some("answered") if !done(note, &["Relayed"]) => {
            let answer = d.get("answer").and_then(Value::as_str);
            // #10765: both kinds append the owner's typed answer_note.
            let extra = note
                .and_then(Value::as_str)
                .map(strip)
                .filter(|n| !n.is_empty())
                .map_or_else(String::new, |n| format!(": {n}"));
            if matches!(answer, Some("Architect Do" | "Have Architect Do")) {
                PendingEvent::new(
                    id,
                    "architect_do",
                    s(d, "answered_at"),
                    format!("Architect Do{extra}"),
                )
            } else {
                let text = format!("{}{extra}", s(d, "answer"));
                PendingEvent::new(id, "answered", s(d, "answered_at"), text)
            }
        }
        Some("cancelled") if !done(note, &closed) => {
            PendingEvent::new(id, "cancelled", s(d, "cancelled_at"), s(d, "cancel_note"))
        }
        Some("wont_do") if !done(note, &closed) => {
            let empty = Map::new();
            let wd = obj(d, "wont_do").unwrap_or(&empty);
            let reason = s(wd, "note");
            let reason = strip(&reason);
            let text = if reason.is_empty() {
                "no reason given"
            } else {
                reason
            };
            PendingEvent::new(id, "wont_do", s(wd, "at"), text.to_owned())
        }
        Some("interactive") if !done(note, &closed) => {
            PendingEvent::new(id, "interactive", s(d, "answered_at"), s(d, "title"))
        }
        _ => return,
    };
    out.push(event);
}

#[cfg(test)]
mod tests {
    use super::relayed;
    use serde_json::{Value, json};

    fn is_relayed(v: Value) -> bool {
        relayed(v.as_object().expect("object"))
    }

    /// Architect ruling: `relayed: true`, a non-blank `relayed` string or a
    /// non-blank `relayed_at` count; nothing else does.
    #[test]
    fn relayed_accepts_bool_string_and_relayed_at() {
        assert!(is_relayed(json!({"relayed": true})));
        assert!(is_relayed(json!({"relayed": "2026-10-11T11:00Z"})));
        assert!(is_relayed(json!({"relayed_at": "2026-10-11T11:00Z"})));
        assert!(!is_relayed(json!({"relayed": false})));
        assert!(!is_relayed(json!({"relayed": "  "})));
        assert!(!is_relayed(json!({"relayed_at": " "})));
        assert!(!is_relayed(json!({"relayed": null, "relayed_at": null})));
        assert!(!is_relayed(json!({"relayed": 1, "relayed_at": 1})));
        assert!(!is_relayed(json!({})));
    }
}
