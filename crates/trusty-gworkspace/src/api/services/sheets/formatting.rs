//! Structured Sheets formatting (`format_sheet`).
//!
//! Why: The raw batchUpdate `requests` passthrough is powerful but forces the
//! caller to hand-author deeply nested Sheets JSON. Mirroring the Python
//! `services/sheets/formatting` surface, this module adds discrete,
//! schema-guided actions (`format_cells`, `set_number_format`, `merge`,
//! `set_column_width`) that build the correct request + field mask from a few
//! typed params — while keeping `raw` as an escape hatch so nothing regresses.
//! What: `format_sheet` dispatches on `action`, builds the batchUpdate
//! `requests` array via a pure per-action builder, and POSTs it.
//! Test: `#[cfg(test)]` module covers each action's request/field-mask shape and
//! the raw passthrough.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::SHEETS_API_BASE;
use crate::api::services::{account_of, opt_str, require_str};

/// Why: Every cell-scoped action targets a half-open, 0-based `GridRange`.
/// What: Builds the `GridRange` from `sheet_id` (required) plus optional
/// start/end row/column indices (absent bounds mean "unbounded" per the API).
/// Test: `grid_range_includes_only_present_bounds` below.
fn parse_grid_range(args: &Value) -> Result<Value> {
    let sheet_id = args
        .get("sheet_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("missing required integer field: sheet_id"))?;
    let mut range = json!({ "sheetId": sheet_id });
    for (arg_key, api_key) in [
        ("start_row_index", "startRowIndex"),
        ("end_row_index", "endRowIndex"),
        ("start_column_index", "startColumnIndex"),
        ("end_column_index", "endColumnIndex"),
    ] {
        if let Some(v) = args.get(arg_key).and_then(Value::as_i64) {
            range[api_key] = json!(v);
        }
    }
    Ok(range)
}

/// Why: Colours arrive either as an RGB(A) array or a `{red,green,blue,alpha}`
/// object; the API always wants the object form with 0.0–1.0 channels.
/// What: Normalises an array `[r,g,b(,a)]` to the object form, or passes an
/// object through unchanged.
/// Test: `color_array_normalises` below.
fn parse_color(v: &Value) -> Option<Value> {
    if v.is_object() {
        return Some(v.clone());
    }
    let arr = v.as_array()?;
    let ch = |i: usize| arr.get(i).and_then(Value::as_f64).unwrap_or(0.0);
    let mut color = json!({ "red": ch(0), "green": ch(1), "blue": ch(2) });
    if let Some(a) = arr.get(3).and_then(Value::as_f64) {
        color["alpha"] = json!(a);
    }
    Some(color)
}

/// Why: `repeatCell` for cell styling needs both a `userEnteredFormat` object
/// and a precise field mask listing exactly the subfields set, or unset fields
/// get clobbered.
/// What: Builds the `format_cells` request from bold/italic/font_size/colours/
/// alignment/wrap params, emitting only the touched field paths.
/// Test: `format_cells_field_mask_is_precise` below.
fn build_format_cells(args: &Value) -> Result<Vec<Value>> {
    let range = parse_grid_range(args)?;
    let mut text_format = json!({});
    let mut fields = Vec::<String>::new();

    if let Some(b) = args.get("bold").and_then(Value::as_bool) {
        text_format["bold"] = json!(b);
        fields.push("userEnteredFormat.textFormat.bold".into());
    }
    if let Some(i) = args.get("italic").and_then(Value::as_bool) {
        text_format["italic"] = json!(i);
        fields.push("userEnteredFormat.textFormat.italic".into());
    }
    if let Some(s) = args.get("font_size").and_then(Value::as_f64) {
        text_format["fontSize"] = json!(s);
        fields.push("userEnteredFormat.textFormat.fontSize".into());
    }
    if let Some(c) = args.get("text_color").and_then(parse_color) {
        text_format["foregroundColor"] = c;
        fields.push("userEnteredFormat.textFormat.foregroundColor".into());
    }

    let mut cell_format = json!({});
    if text_format.as_object().is_some_and(|o| !o.is_empty()) {
        cell_format["textFormat"] = text_format;
    }
    if let Some(c) = args.get("background_color").and_then(parse_color) {
        cell_format["backgroundColor"] = c;
        fields.push("userEnteredFormat.backgroundColor".into());
    }
    if let Some(a) = opt_str(args, "horizontal_alignment") {
        cell_format["horizontalAlignment"] = json!(a);
        fields.push("userEnteredFormat.horizontalAlignment".into());
    }
    if let Some(a) = opt_str(args, "vertical_alignment") {
        cell_format["verticalAlignment"] = json!(a);
        fields.push("userEnteredFormat.verticalAlignment".into());
    }
    if let Some(w) = opt_str(args, "wrap_strategy") {
        cell_format["wrapStrategy"] = json!(w);
        fields.push("userEnteredFormat.wrapStrategy".into());
    }

    if fields.is_empty() {
        return Err(anyhow!(
            "format_cells requires at least one style field (bold, italic, font_size, text_color, background_color, horizontal_alignment, vertical_alignment, wrap_strategy)"
        ));
    }
    Ok(vec![json!({
        "repeatCell": {
            "range": range,
            "cell": { "userEnteredFormat": cell_format },
            "fields": fields.join(","),
        }
    })])
}

/// Why: Number/date/currency display formats are a distinct, common styling
/// need with a small typed surface (type enum + optional pattern).
/// What: Builds a `repeatCell` request that sets `numberFormat`.
/// Test: `number_format_request_shape` below.
fn build_number_format(args: &Value) -> Result<Vec<Value>> {
    let range = parse_grid_range(args)?;
    let fmt_type = require_str(args, "number_format_type")?;
    let mut number_format = json!({ "type": fmt_type });
    if let Some(pattern) = opt_str(args, "pattern") {
        number_format["pattern"] = json!(pattern);
    }
    Ok(vec![json!({
        "repeatCell": {
            "range": range,
            "cell": { "userEnteredFormat": { "numberFormat": number_format } },
            "fields": "userEnteredFormat.numberFormat",
        }
    })])
}

/// Why: Merging a header banner or label block is a one-line intent that the
/// raw API expresses as `mergeCells` with a merge-type enum.
/// What: Builds a `mergeCells` request (default `MERGE_ALL`).
/// Test: `merge_request_shape` below.
fn build_merge(args: &Value) -> Result<Vec<Value>> {
    let range = parse_grid_range(args)?;
    let merge_type = opt_str(args, "merge_type").unwrap_or("MERGE_ALL");
    Ok(vec![json!({
        "mergeCells": { "range": range, "mergeType": merge_type }
    })])
}

/// Why: Auto-fit is unavailable via the values API; explicit pixel widths are
/// the reliable way to size columns (or, via `dimension`, rows).
/// What: Builds an `updateDimensionProperties` request setting `pixelSize`.
/// Test: `column_width_request_shape` below.
fn build_column_width(args: &Value) -> Result<Vec<Value>> {
    let sheet_id = args
        .get("sheet_id")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("missing required integer field: sheet_id"))?;
    let start = args
        .get("start_index")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("missing required integer field: start_index"))?;
    let end = args
        .get("end_index")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("missing required integer field: end_index"))?;
    let pixel_size = args
        .get("pixel_size")
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("missing required integer field: pixel_size"))?;
    let dimension = opt_str(args, "dimension").unwrap_or("COLUMNS");
    Ok(vec![json!({
        "updateDimensionProperties": {
            "range": {
                "sheetId": sheet_id,
                "dimension": dimension,
                "startIndex": start,
                "endIndex": end,
            },
            "properties": { "pixelSize": pixel_size },
            "fields": "pixelSize",
        }
    })])
}

/// Why: One pure dispatcher keeps `format_sheet`'s async wrapper trivial and the
/// per-action logic unit-testable without a client.
/// What: Maps `action` to the matching request builder; `raw` returns the
/// caller-supplied `requests` passthrough unchanged.
/// Test: Every branch is covered by the `#[cfg(test)]` module.
fn build_format_requests(action: &str, args: &Value) -> Result<Vec<Value>> {
    match action {
        "format_cells" => build_format_cells(args),
        "set_number_format" => build_number_format(args),
        "merge" => build_merge(args),
        "set_column_width" => build_column_width(args),
        "raw" => {
            let requests = args
                .get("requests")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| anyhow!("action 'raw' requires a 'requests' array"))?;
            Ok(requests)
        }
        other => Err(anyhow!("unknown action for format_sheet: {other}")),
    }
}

/// Why: Cell formatting is a batchUpdate surface; discrete typed actions cover
/// the common cases while `raw` remains a full-power escape hatch.
/// What: Dispatches on `action` (defaulting to `raw` when a `requests` array is
/// supplied without an explicit action, preserving the prior contract), builds
/// the `requests`, and POSTs a single batchUpdate.
/// Test: Body building via the `#[cfg(test)]` module; the POST leg is live-only.
pub async fn format_sheet(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "spreadsheet_id")?;
    // Back-compat: a bare `requests` array with no `action` means `raw`.
    let action = opt_str(&args, "action").unwrap_or(if args.get("requests").is_some() {
        "raw"
    } else {
        ""
    });
    let requests = build_format_requests(action, &args)?;
    let body = json!({ "requests": requests });
    let url = format!("{SHEETS_API_BASE}/spreadsheets/{id}:batchUpdate");
    client.post(&url, body, account).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_range_includes_only_present_bounds() {
        let args = json!({ "sheet_id": 3, "start_row_index": 1, "end_column_index": 4 });
        let r = parse_grid_range(&args).unwrap();
        assert_eq!(r["sheetId"], 3);
        assert_eq!(r["startRowIndex"], 1);
        assert_eq!(r["endColumnIndex"], 4);
        assert!(r.get("endRowIndex").is_none());
        assert!(parse_grid_range(&json!({})).is_err());
    }

    #[test]
    fn color_array_normalises() {
        let c = parse_color(&json!([1.0, 0.5, 0.0, 0.8])).unwrap();
        assert_eq!(c["red"], 1.0);
        assert_eq!(c["green"], 0.5);
        assert_eq!(c["blue"], 0.0);
        assert_eq!(c["alpha"], 0.8);
        // Object form passes through.
        let obj = parse_color(&json!({ "red": 0.2 })).unwrap();
        assert_eq!(obj["red"], 0.2);
    }

    #[test]
    fn format_cells_field_mask_is_precise() {
        let args = json!({
            "sheet_id": 0,
            "start_row_index": 0, "end_row_index": 1,
            "bold": true,
            "background_color": [0.9, 0.9, 0.9],
            "horizontal_alignment": "CENTER",
        });
        let reqs = build_format_cells(&args).unwrap();
        let req = &reqs[0]["repeatCell"];
        let cf = &req["cell"]["userEnteredFormat"];
        assert_eq!(cf["textFormat"]["bold"], true);
        assert_eq!(cf["backgroundColor"]["red"], 0.9);
        assert_eq!(cf["horizontalAlignment"], "CENTER");
        let mask = req["fields"].as_str().unwrap();
        assert!(mask.contains("userEnteredFormat.textFormat.bold"));
        assert!(mask.contains("userEnteredFormat.backgroundColor"));
        assert!(mask.contains("userEnteredFormat.horizontalAlignment"));
        // Untouched subfields are absent from the mask.
        assert!(!mask.contains("italic"));
        assert!(!mask.contains("verticalAlignment"));
    }

    #[test]
    fn format_cells_requires_a_field() {
        assert!(build_format_cells(&json!({ "sheet_id": 0 })).is_err());
    }

    #[test]
    fn number_format_request_shape() {
        let args = json!({
            "sheet_id": 0,
            "number_format_type": "CURRENCY",
            "pattern": "$#,##0.00",
        });
        let reqs = build_number_format(&args).unwrap();
        let nf = &reqs[0]["repeatCell"]["cell"]["userEnteredFormat"]["numberFormat"];
        assert_eq!(nf["type"], "CURRENCY");
        assert_eq!(nf["pattern"], "$#,##0.00");
        assert_eq!(
            reqs[0]["repeatCell"]["fields"],
            "userEnteredFormat.numberFormat"
        );
    }

    #[test]
    fn merge_request_shape() {
        let reqs = build_merge(&json!({ "sheet_id": 0, "end_column_index": 3 })).unwrap();
        assert_eq!(reqs[0]["mergeCells"]["mergeType"], "MERGE_ALL");
        let reqs2 = build_merge(&json!({ "sheet_id": 0, "merge_type": "MERGE_COLUMNS" })).unwrap();
        assert_eq!(reqs2[0]["mergeCells"]["mergeType"], "MERGE_COLUMNS");
    }

    #[test]
    fn column_width_request_shape() {
        let args = json!({
            "sheet_id": 2,
            "start_index": 0, "end_index": 2,
            "pixel_size": 160,
        });
        let reqs = build_column_width(&args).unwrap();
        let req = &reqs[0]["updateDimensionProperties"];
        assert_eq!(req["range"]["dimension"], "COLUMNS");
        assert_eq!(req["range"]["startIndex"], 0);
        assert_eq!(req["properties"]["pixelSize"], 160);
        assert_eq!(req["fields"], "pixelSize");
        assert!(build_column_width(&json!({ "sheet_id": 2 })).is_err());
    }

    #[test]
    fn raw_passthrough_preserved() {
        let raw = json!([{ "some": "batchUpdateRequest" }]);
        let out = build_format_requests("raw", &json!({ "requests": raw.clone() })).unwrap();
        assert_eq!(json!(out), raw);
    }

    #[test]
    fn unknown_action_errors() {
        assert!(build_format_requests("bogus", &json!({})).is_err());
    }
}
