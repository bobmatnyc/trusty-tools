//! Google Calendar tool definitions.
//!
//! Why: Groups calendar, event, and free/busy tools together.
//! What: Appends the calendar tool group to the shared registry vector.
//! Test: Covered via `tool_list_response()` in `tools::tests`.

use super::schema::{account_schema, action_enum, tool};
use serde_json::{Value, json};

/// Append the calendar tool group to the registry.
///
/// Why: Keeps calendar-related tools colocated.
/// What: Pushes `manage_calendars`, `manage_events`, and `query_free_busy`.
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn append(tools: &mut Vec<Value>) {
    tools.push(tool(
        "manage_calendars",
        "Create, read, update, or delete Google Calendars.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["list", "create", "update", "delete"]),
            "calendar_id": { "type": "string", "description": "Calendar ID (required for update/delete)." },
            "summary": { "type": "string", "description": "Calendar title (create)." },
            "description": { "type": "string" },
            "time_zone": { "type": "string" },
            "updates": { "type": "object", "description": "Patch body for update." },
        }),
        &["action"],
    ));
    tools.push(tool(
        "manage_events",
        "Create, read, update, or delete events within a Google Calendar.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["list", "create", "update", "delete"]),
            "calendar_id": { "type": "string", "description": "Calendar ID. Defaults to 'primary'." },
            "event_id": { "type": "string" },
            "event": { "type": "object", "description": "Raw event resource (create). Escape hatch; typed fields below take precedence." },
            "updates": { "type": "object", "description": "Raw patch body (update). Escape hatch; typed fields below take precedence." },
            "start_time": { "type": "string", "description": "Event start, RFC3339 (create/update). Maps to start.dateTime." },
            "end_time": { "type": "string", "description": "Event end, RFC3339 (create/update). Maps to end.dateTime." },
            "timezone": { "type": "string", "description": "IANA timezone (e.g. 'America/Los_Angeles') applied to start/end (create/update)." },
            "location": { "type": "string", "description": "Event location (create/update)." },
            "attendees": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Attendee email addresses (create/update). Each maps to an attendee {email} object."
            },
            "recurrence": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Recurrence rules, e.g. 'FREQ=WEEKLY;COUNT=10' or a full 'RRULE:FREQ=...' line (create/update)."
            },
            "time_min": { "type": "string", "description": "RFC3339 lower bound (list)." },
            "time_max": { "type": "string", "description": "RFC3339 upper bound (list)." },
            "query": { "type": "string", "description": "Free-text search (list)." },
            "max_results": { "type": "integer" },
        }),
        &["action"],
    ));
    tools.push(tool(
        "query_free_busy",
        "Query free/busy status across calendars for a time range.",
        json!({
            "account": account_schema(),
            "time_min": { "type": "string", "description": "RFC3339 start." },
            "time_max": { "type": "string", "description": "RFC3339 end." },
            "calendar_ids": { "type": "array", "items": { "type": "string" } },
        }),
        &["time_min", "time_max"],
    ));
}
