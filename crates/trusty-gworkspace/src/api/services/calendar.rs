//! Google Calendar service.
//!
//! Why: Calendars + events + free/busy queries are the three primary user
//! workflows; one module each Python service module.
//! What: Dispatches on the `action` field (list|create|update|delete) plus
//! `query_free_busy` as a separate tool.
//! Test: Integration only.

use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::CALENDAR_API_BASE;
use crate::api::services::{account_of, opt_str, require_str};

/// CRUD operations against the calendarList collection.
///
/// Why: The Google API splits Calendar into the resource (`/calendars`) and
/// the user's subscription list (`/users/me/calendarList`). For listing we
/// use `calendarList`; for create/update/delete we hit `/calendars`.
/// What: `action` ∈ {"list", "create", "update", "delete"}.
/// Test: Live calls only.
pub async fn manage_calendars(client: &BaseClient, args: Value) -> Result<Value> {
    let action = require_str(&args, "action")?;
    let account = account_of(&args);
    match action {
        "list" => {
            let url = format!("{CALENDAR_API_BASE}/users/me/calendarList");
            client.get(&url, account).await
        }
        "create" => {
            let summary = require_str(&args, "summary")?;
            let body = json!({
                "summary": summary,
                "description": args.get("description"),
                "timeZone": args.get("time_zone"),
            });
            let url = format!("{CALENDAR_API_BASE}/calendars");
            client.post(&url, body, account).await
        }
        "update" => {
            let calendar_id = require_str(&args, "calendar_id")?;
            let url = format!("{CALENDAR_API_BASE}/calendars/{calendar_id}");
            let body = args.get("updates").cloned().unwrap_or_else(|| json!({}));
            client.patch(&url, body, account).await
        }
        "delete" => {
            let calendar_id = require_str(&args, "calendar_id")?;
            let url = format!("{CALENDAR_API_BASE}/calendars/{calendar_id}");
            client.delete(&url, account).await
        }
        other => Err(anyhow!("unknown action for manage_calendars: {other}")),
    }
}

/// CRUD over events within a calendar.
/// Why: Single tool surfaces every event CRUD path so the model dispatches by `action`.
/// What: Routes `list|get|insert|update|delete` to the Calendar v3 `/events` endpoints.
/// Test: Live API; logic-only branches covered by argument-extraction smoke tests.
pub async fn manage_events(client: &BaseClient, args: Value) -> Result<Value> {
    let action = require_str(&args, "action")?;
    let account = account_of(&args);
    let calendar_id = opt_str(&args, "calendar_id").unwrap_or("primary");
    match action {
        "list" => {
            let mut url = format!("{CALENDAR_API_BASE}/calendars/{calendar_id}/events");
            let mut params = Vec::<(String, String)>::new();
            if let Some(t) = opt_str(&args, "time_min") {
                params.push(("timeMin".into(), t.into()));
            }
            if let Some(t) = opt_str(&args, "time_max") {
                params.push(("timeMax".into(), t.into()));
            }
            if let Some(q) = opt_str(&args, "query") {
                params.push(("q".into(), q.into()));
            }
            if let Some(max) = args.get("max_results").and_then(|v| v.as_i64()) {
                params.push(("maxResults".into(), max.to_string()));
            }
            if !params.is_empty() {
                let qs: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
                url = format!("{url}?{}", qs.join("&"));
            }
            client.get(&url, account).await
        }
        "create" => {
            let base = args.get("event").cloned().unwrap_or_else(|| json!({}));
            let body = build_event_body(base, &args);
            if body.as_object().map(Map::is_empty).unwrap_or(true) {
                return Err(anyhow!(
                    "no event fields provided: supply typed fields \
                     (start_time, end_time, attendees, location, timezone, \
                     recurrence) or a raw 'event' object"
                ));
            }
            let url = format!("{CALENDAR_API_BASE}/calendars/{calendar_id}/events");
            client.post(&url, body, account).await
        }
        "update" => {
            let event_id = require_str(&args, "event_id")?;
            let base = args.get("updates").cloned().unwrap_or_else(|| json!({}));
            let body = build_event_body(base, &args);
            let url = format!("{CALENDAR_API_BASE}/calendars/{calendar_id}/events/{event_id}");
            client.patch(&url, body, account).await
        }
        "delete" => {
            let event_id = require_str(&args, "event_id")?;
            let url = format!("{CALENDAR_API_BASE}/calendars/{calendar_id}/events/{event_id}");
            client.delete(&url, account).await
        }
        other => Err(anyhow!("unknown action for manage_events: {other}")),
    }
}

/// Overlay declared typed event fields onto a base event body.
///
/// Why: Python's upstream `manage_events` exposes explicit typed properties
/// (`start_time`, `end_time`, `attendees`, `location`, `timezone`,
/// `recurrence`) so the tool schema is self-documenting, while our original
/// Rust port only accepted an opaque `event`/`updates` object. This gives
/// schema-discoverability parity without losing the raw escape hatch: the
/// base object is still honoured and typed fields take precedence, merging
/// sensibly into it.
/// What: Starts from `base` (the caller's raw `event`/`updates` object, or an
/// empty object) and, for each typed field present in `args`, maps it into the
/// Calendar v3 event body shape — `start`/`end` become `{dateTime, timeZone}`
/// objects, `attendees` string emails become `[{email}]`, `recurrence` strings
/// are normalised to RRULE lines. A non-object `base` is discarded in favour of
/// an empty object so a malformed escape hatch can never corrupt the body.
/// Test: `build_event_body_*` unit tests below.
fn build_event_body(base: Value, args: &Value) -> Value {
    let mut body = if base.is_object() { base } else { json!({}) };
    let Some(map) = body.as_object_mut() else {
        // Unreachable: guarded to be an object above. Return an empty object
        // rather than panic to honour the no-unwrap-in-library rule.
        return json!({});
    };
    let timezone = opt_str(args, "timezone");

    // start / end: typed timestamp wins; a lone timezone only decorates an
    // already-present start/end so we never emit a timeZone-only slot (invalid).
    if let Some(start) = opt_str(args, "start_time") {
        map.insert("start".into(), time_field(start, timezone));
    } else if let Some(tz) = timezone {
        set_timezone_only(map, "start", tz);
    }
    if let Some(end) = opt_str(args, "end_time") {
        map.insert("end".into(), time_field(end, timezone));
    } else if let Some(tz) = timezone {
        set_timezone_only(map, "end", tz);
    }

    if let Some(location) = opt_str(args, "location") {
        map.insert("location".into(), json!(location));
    }
    if let Some(attendees) = build_attendees(args) {
        map.insert("attendees".into(), attendees);
    }
    if let Some(recurrence) = build_recurrence(args) {
        map.insert("recurrence".into(), recurrence);
    }
    body
}

/// Build a Calendar `{dateTime, timeZone}` time field.
///
/// Why: `start`/`end` in the Calendar v3 API are objects, not bare strings.
/// What: Emits `dateTime`, plus `timeZone` only when a timezone is supplied.
/// Test: Covered via `build_event_body_*` below.
fn time_field(date_time: &str, timezone: Option<&str>) -> Value {
    match timezone {
        Some(tz) => json!({ "dateTime": date_time, "timeZone": tz }),
        None => json!({ "dateTime": date_time }),
    }
}

/// Decorate an existing start/end object with a timezone, if it exists.
///
/// Why: A caller may pass `timezone` alongside a raw `event.start` that already
/// carries a `dateTime`/`date`; the typed timezone should still apply. We do
/// NOT create the slot from scratch, since a `{timeZone}`-only object with no
/// `dateTime`/`date` is rejected by the API.
/// What: When `map[key]` is already an object, inserts/overwrites `timeZone`.
/// Test: `build_event_body_timezone_only_decorates_existing` below.
fn set_timezone_only(map: &mut Map<String, Value>, key: &str, timezone: &str) {
    if let Some(Value::Object(slot)) = map.get_mut(key) {
        slot.insert("timeZone".into(), json!(timezone));
    }
}

/// Normalise the `attendees` field into Calendar `[{email}]` objects.
///
/// Why: The typed schema accepts a plain list of email strings (the common
/// case), while the raw API wants attendee objects; passing through objects
/// verbatim preserves advanced fields (`responseStatus`, `optional`, …).
/// What: Returns `Some(array)` when `attendees` is an array — string entries
/// become `{email}`, non-string entries are cloned through unchanged; returns
/// `None` (skip) when the field is absent or not an array.
/// Test: `build_event_body_attendees_*` below.
fn build_attendees(args: &Value) -> Option<Value> {
    let arr = args.get("attendees")?.as_array()?;
    let list: Vec<Value> = arr
        .iter()
        .map(|entry| match entry.as_str() {
            Some(email) => json!({ "email": email }),
            None => entry.clone(),
        })
        .collect();
    Some(Value::Array(list))
}

/// Normalise the `recurrence` field into a list of RRULE lines.
///
/// Why: Google expects recurrence entries prefixed with a rule keyword
/// (`RRULE:`, `RDATE:`, `EXDATE:`, `EXRULE:`); users routinely pass a bare
/// `FREQ=…` string, so we prefix `RRULE:` when no keyword is present.
/// What: Accepts either a single string or an array of strings; returns
/// `Some(array)` of normalised lines, or `None` when absent/empty/wrong-typed.
/// Test: `build_event_body_recurrence_*` below.
fn build_recurrence(args: &Value) -> Option<Value> {
    let value = args.get("recurrence")?;
    let lines: Vec<Value> = match value {
        Value::String(s) => vec![Value::String(normalize_rrule(s))],
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| Value::String(normalize_rrule(s)))
            .collect(),
        _ => return None,
    };
    if lines.is_empty() {
        return None;
    }
    Some(Value::Array(lines))
}

/// Prefix `RRULE:` onto a recurrence rule that lacks a rule-type keyword.
///
/// Why: See `build_recurrence`. Keeps already-qualified lines untouched.
/// What: Case-insensitively checks for a known prefix; prefixes `RRULE:` when
/// none is found.
/// Test: `build_event_body_recurrence_*` below.
fn normalize_rrule(rule: &str) -> String {
    const PREFIXES: [&str; 4] = ["RRULE:", "RDATE:", "EXDATE:", "EXRULE:"];
    let upper = rule.trim_start().to_ascii_uppercase();
    if PREFIXES.iter().any(|p| upper.starts_with(p)) {
        rule.to_string()
    } else {
        format!("RRULE:{rule}")
    }
}

/// Free/busy query — useful for scheduling.
/// Why: Scheduling assistants need a fast availability check across multiple calendars.
/// What: POSTs to `/freeBusy` with `time_min`, `time_max`, and a calendar id list.
/// Test: Live API.
pub async fn query_free_busy(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let time_min = require_str(&args, "time_min")?;
    let time_max = require_str(&args, "time_max")?;
    let calendars: Vec<Value> = args
        .get("calendar_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| json!({ "id": s }))
                .collect()
        })
        .unwrap_or_else(|| vec![json!({ "id": "primary" })]);
    let body = json!({
        "timeMin": time_min,
        "timeMax": time_max,
        "items": calendars,
    });
    let url = format!("{CALENDAR_API_BASE}/freeBusy");
    client.post(&url, body, account).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_event_body_maps_all_typed_fields() {
        let args = json!({
            "start_time": "2026-08-01T09:00:00-07:00",
            "end_time": "2026-08-01T10:00:00-07:00",
            "timezone": "America/Los_Angeles",
            "location": "Room 4",
            "attendees": ["a@example.com", "b@example.com"],
            "recurrence": "FREQ=WEEKLY;COUNT=3",
        });
        let body = build_event_body(json!({}), &args);
        assert_eq!(
            body["start"],
            json!({ "dateTime": "2026-08-01T09:00:00-07:00", "timeZone": "America/Los_Angeles" })
        );
        assert_eq!(
            body["end"],
            json!({ "dateTime": "2026-08-01T10:00:00-07:00", "timeZone": "America/Los_Angeles" })
        );
        assert_eq!(body["location"], json!("Room 4"));
        assert_eq!(
            body["attendees"],
            json!([{ "email": "a@example.com" }, { "email": "b@example.com" }])
        );
        // Bare FREQ string is normalised to an RRULE line.
        assert_eq!(body["recurrence"], json!(["RRULE:FREQ=WEEKLY;COUNT=3"]));
    }

    #[test]
    fn build_event_body_start_without_timezone_omits_timezone() {
        let args = json!({ "start_time": "2026-08-01T09:00:00Z" });
        let body = build_event_body(json!({}), &args);
        assert_eq!(body["start"], json!({ "dateTime": "2026-08-01T09:00:00Z" }));
        assert!(body.get("end").is_none());
    }

    #[test]
    fn build_event_body_typed_fields_override_raw_escape_hatch() {
        // The raw `event` object is honoured, but typed fields win where they
        // overlap and leave unrelated raw fields (summary) intact.
        let base = json!({
            "summary": "Standup",
            "location": "old room",
            "start": { "dateTime": "2000-01-01T00:00:00Z" },
        });
        let args = json!({
            "start_time": "2026-08-01T09:00:00Z",
            "location": "new room",
        });
        let body = build_event_body(base, &args);
        assert_eq!(body["summary"], json!("Standup"));
        assert_eq!(body["location"], json!("new room"));
        assert_eq!(body["start"], json!({ "dateTime": "2026-08-01T09:00:00Z" }));
    }

    #[test]
    fn build_event_body_raw_only_is_passed_through_unchanged() {
        // No typed fields: the escape hatch must be returned verbatim so
        // nothing regresses for existing callers.
        let base = json!({
            "summary": "Legacy",
            "start": { "dateTime": "2026-08-01T09:00:00Z" },
            "end": { "dateTime": "2026-08-01T10:00:00Z" },
        });
        let body = build_event_body(base.clone(), &json!({}));
        assert_eq!(body, base);
    }

    #[test]
    fn build_event_body_timezone_only_decorates_existing() {
        // A lone timezone applies to a pre-existing start/end but must not
        // fabricate a timeZone-only slot (which the API rejects).
        let base = json!({ "start": { "dateTime": "2026-08-01T09:00:00Z" } });
        let args = json!({ "timezone": "Europe/Paris" });
        let body = build_event_body(base, &args);
        assert_eq!(
            body["start"],
            json!({ "dateTime": "2026-08-01T09:00:00Z", "timeZone": "Europe/Paris" })
        );
        // No `end` existed, so none is invented.
        assert!(body.get("end").is_none());
    }

    #[test]
    fn build_event_body_attendee_objects_pass_through() {
        // Advanced attendee objects are preserved; plain strings are wrapped.
        let args = json!({
            "attendees": [ "a@example.com", { "email": "b@example.com", "optional": true } ],
        });
        let body = build_event_body(json!({}), &args);
        assert_eq!(
            body["attendees"],
            json!([
                { "email": "a@example.com" },
                { "email": "b@example.com", "optional": true }
            ])
        );
    }

    #[test]
    fn build_event_body_recurrence_array_and_prefix_preserved() {
        // Array form; an already-qualified RRULE/EXDATE line is left untouched.
        let args = json!({
            "recurrence": ["RRULE:FREQ=DAILY", "EXDATE:20260901T090000Z"],
        });
        let body = build_event_body(json!({}), &args);
        assert_eq!(
            body["recurrence"],
            json!(["RRULE:FREQ=DAILY", "EXDATE:20260901T090000Z"])
        );
    }

    #[test]
    fn build_event_body_empty_stays_empty() {
        // Neither raw nor typed fields → empty object (create-path guard relies
        // on this to reject a content-free request).
        let body = build_event_body(json!({}), &json!({}));
        assert_eq!(body, json!({}));
        // A non-object escape hatch is discarded rather than corrupting the body.
        let body = build_event_body(json!("not-an-object"), &json!({}));
        assert_eq!(body, json!({}));
    }
}
