//! Google Sheets tool definitions.
//!
//! Why: Groups spreadsheet metadata, structure, value, and formatting tools.
//! What: Appends the Sheets tool group to the shared registry vector.
//! Test: Covered via `tool_list_response()` in `tools::tests`.

use super::schema::{account_schema, action_enum, tool};
use serde_json::{Value, json};

/// Append the Sheets tool group to the registry.
///
/// Why: Keeps Sheets-related tools colocated.
/// What: Pushes the spreadsheet get/manage/values/format tools.
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn append(tools: &mut Vec<Value>) {
    tools.push(tool(
        "get_spreadsheet",
        "Fetch a spreadsheet's metadata (and optionally grid data).",
        json!({
            "account": account_schema(),
            "spreadsheet_id": { "type": "string" },
            "include_grid_data": { "type": "boolean" },
        }),
        &["spreadsheet_id"],
    ));
    tools.push(tool(
        "manage_spreadsheet",
        "Create a spreadsheet or add/delete sheets within one.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["create", "add_sheet", "delete_sheet"]),
            "spreadsheet_id": { "type": "string" },
            "title": { "type": "string" },
            "sheet_id": { "type": "integer" },
        }),
        &["action"],
    ));
    tools.push(tool(
        "modify_sheet_values",
        "Read, write, append, or clear cell values in a sheet range.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["read", "write", "update", "append", "clear"]),
            "spreadsheet_id": { "type": "string" },
            "range": { "type": "string", "description": "A1 notation, e.g. 'Sheet1!A1:C10'" },
            "values": { "type": "array", "items": { "type": "array" } },
        }),
        &["spreadsheet_id", "range"],
    ));
    tools.push(tool(
        "format_sheet",
        "Apply a batchUpdate to a spreadsheet (formatting, conditional rules, etc.).",
        json!({
            "account": account_schema(),
            "spreadsheet_id": { "type": "string" },
            "requests": { "type": "array", "items": { "type": "object" } },
        }),
        &["spreadsheet_id", "requests"],
    ));
}
