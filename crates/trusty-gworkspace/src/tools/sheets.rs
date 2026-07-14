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
        "Format a range via a discrete action (format_cells, set_number_format, \
         merge, set_column_width) or 'raw' to pass batchUpdate requests directly.",
        json!({
            "account": account_schema(),
            "spreadsheet_id": { "type": "string" },
            "action": action_enum(&[
                "format_cells", "set_number_format", "merge", "set_column_width", "raw",
            ]),
            // GridRange (0-based, half-open) — used by format_cells / set_number_format / merge.
            "sheet_id": { "type": "integer", "description": "Sheet (tab) id the range lives in." },
            "start_row_index": { "type": "integer" },
            "end_row_index": { "type": "integer" },
            "start_column_index": { "type": "integer" },
            "end_column_index": { "type": "integer" },
            // format_cells params.
            "bold": { "type": "boolean" },
            "italic": { "type": "boolean" },
            "font_size": { "type": "number" },
            "text_color": {
                "description": "RGB(A) array [r,g,b(,a)] (0..1) or {red,green,blue,alpha} object.",
                "oneOf": [{ "type": "array" }, { "type": "object" }],
            },
            "background_color": {
                "description": "RGB(A) array [r,g,b(,a)] (0..1) or {red,green,blue,alpha} object.",
                "oneOf": [{ "type": "array" }, { "type": "object" }],
            },
            "horizontal_alignment": {
                "type": "string", "enum": ["LEFT", "CENTER", "RIGHT"],
            },
            "vertical_alignment": {
                "type": "string", "enum": ["TOP", "MIDDLE", "BOTTOM"],
            },
            "wrap_strategy": {
                "type": "string",
                "enum": ["OVERFLOW_CELL", "LEGACY_WRAP", "CLIP", "WRAP"],
            },
            // set_number_format params.
            "number_format_type": {
                "type": "string",
                "enum": ["TEXT", "NUMBER", "PERCENT", "CURRENCY", "DATE", "TIME", "DATE_TIME", "SCIENTIFIC"],
            },
            "pattern": { "type": "string", "description": "Number/date format pattern." },
            // merge params.
            "merge_type": {
                "type": "string", "enum": ["MERGE_ALL", "MERGE_COLUMNS", "MERGE_ROWS"],
            },
            // set_column_width params (DimensionRange).
            "dimension": { "type": "string", "enum": ["COLUMNS", "ROWS"] },
            "start_index": { "type": "integer" },
            "end_index": { "type": "integer" },
            "pixel_size": { "type": "integer" },
            // raw escape hatch.
            "requests": { "type": "array", "items": { "type": "object" } },
        }),
        &["spreadsheet_id"],
    ));
    tools.push(tool(
        "create_chart",
        "Add a bar/column/line/area/pie chart over a data grid (first column = \
         labels/domain, remaining columns = series).",
        json!({
            "account": account_schema(),
            "spreadsheet_id": { "type": "string" },
            "chart_type": {
                "type": "string",
                "enum": ["column", "bar", "line", "area", "scatter", "pie"],
            },
            "source_sheet_id": { "type": "integer", "description": "Sheet id holding the source data." },
            "start_row_index": { "type": "integer" },
            "end_row_index": { "type": "integer" },
            "start_column_index": { "type": "integer" },
            "end_column_index": { "type": "integer" },
            "title": { "type": "string" },
            "x_axis_title": { "type": "string" },
            "y_axis_title": { "type": "string" },
            "has_headers": { "type": "boolean", "description": "First data row is a header (default true)." },
            "legend_position": {
                "type": "string",
                "enum": ["BOTTOM_LEGEND", "LEFT_LEGEND", "RIGHT_LEGEND", "TOP_LEGEND", "NO_LEGEND"],
            },
            "new_sheet": { "type": "boolean", "description": "Place chart on a new sheet (default true)." },
            "position_sheet_id": { "type": "integer", "description": "Overlay target sheet when new_sheet=false." },
            "anchor_row": { "type": "integer" },
            "anchor_column": { "type": "integer" },
        }),
        &[
            "spreadsheet_id", "chart_type", "source_sheet_id",
            "start_row_index", "end_row_index", "start_column_index", "end_column_index",
        ],
    ));
}
