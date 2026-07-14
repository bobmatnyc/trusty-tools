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
/// start/end row/column indices (absent bounds mean "unbounded" per the API —
/// callers that need a fully-bounded range must additionally call
/// `require_bounded_range`).
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

/// Why: An unbounded `GridRange` (any of the four row/column bounds omitted)
/// means "the rest of the sheet" to the Sheets API. For `merge` this is
/// destructive — `MERGE_ALL` over an unbounded range merges the entire tab
/// into one cell, silently discarding every non-top-left value — and for
/// `format_cells`/`set_number_format` it silently restyles far more of the
/// sheet than an LLM caller that omitted a bound likely intended. Rejecting
/// locally turns a silent data-loss/over-broad-mutation bug into a clear
/// caller-facing error.
/// What: Errors unless all four of start/end row/column index are present on
/// the built range.
/// Test: `merge_without_bounds_is_rejected`,
/// `format_cells_without_bounds_is_rejected`,
/// `number_format_without_bounds_is_rejected` below.
fn require_bounded_range(range: &Value) -> Result<()> {
    for key in [
        "startRowIndex",
        "endRowIndex",
        "startColumnIndex",
        "endColumnIndex",
    ] {
        if range.get(key).is_none() {
            return Err(anyhow!(
                "explicit start_row_index, end_row_index, start_column_index, and \
                 end_column_index are all required for this action (missing '{key}') — \
                 omitting a bound would apply to the rest of the sheet"
            ));
        }
    }
    Ok(())
}

/// Why: A single numeric colour channel must be a number in `[0.0, 1.0]`;
/// silently defaulting a missing/non-numeric/out-of-range channel to `0.0`
/// (as an `unwrap_or`-based parse would) turns a caller typo (e.g. passing
/// `255` instead of `1.0`) into a silently-wrong colour applied to the sheet.
/// What: Extracts `v` as an `f64` and validates it falls within `[0.0, 1.0]`.
/// Test: Covered via `parse_color` tests below.
fn parse_channel(v: &Value, name: &str) -> Result<f64> {
    let n = v
        .as_f64()
        .ok_or_else(|| anyhow!("color channel '{name}' must be a number in [0.0, 1.0]"))?;
    if !(0.0..=1.0).contains(&n) {
        return Err(anyhow!(
            "color channel '{name}' must be in [0.0, 1.0], got {n}"
        ));
    }
    Ok(n)
}

/// Why: Colours arrive either as an RGB(A) array or a `{red,green,blue,alpha}`
/// object; the API always wants the object form with validated 0.0–1.0
/// channels — malformed input (missing/non-numeric/out-of-range channels)
/// must be rejected rather than silently coerced to black (see
/// `parse_channel`).
/// What: Normalises an array `[r,g,b(,a)]` to the object form (validating
/// each channel), or validates-and-passes-through an object's present
/// channels.
/// Test: `color_array_normalises`, `color_array_rejects_out_of_range`,
/// `color_array_rejects_non_numeric` below.
fn parse_color(v: &Value) -> Result<Value> {
    if let Some(obj) = v.as_object() {
        let mut color = json!({});
        for key in ["red", "green", "blue", "alpha"] {
            if let Some(channel) = obj.get(key) {
                color[key] = json!(parse_channel(channel, key)?);
            }
        }
        return Ok(color);
    }
    let arr = v.as_array().ok_or_else(|| {
        anyhow!("color must be an array [r,g,b(,a)] or a {{red,green,blue,alpha}} object")
    })?;
    if arr.len() < 3 {
        return Err(anyhow!("color array must have at least 3 channels [r,g,b]"));
    }
    let mut color = json!({
        "red": parse_channel(&arr[0], "red")?,
        "green": parse_channel(&arr[1], "green")?,
        "blue": parse_channel(&arr[2], "blue")?,
    });
    if let Some(a) = arr.get(3) {
        color["alpha"] = json!(parse_channel(a, "alpha")?);
    }
    Ok(color)
}

/// Why: `repeatCell` for cell styling needs both a `userEnteredFormat` object
/// and a precise field mask listing exactly the subfields set, or unset fields
/// get clobbered. It must also target an explicitly-bounded range — an
/// omitted bound would silently restyle the rest of the sheet.
/// What: Builds the `format_cells` request from bold/italic/font_size/colours/
/// alignment/wrap params, emitting only the touched field paths.
/// Test: `format_cells_field_mask_is_precise`,
/// `format_cells_without_bounds_is_rejected` below.
fn build_format_cells(args: &Value) -> Result<Vec<Value>> {
    let range = parse_grid_range(args)?;
    require_bounded_range(&range)?;
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
    if let Some(v) = args.get("text_color") {
        text_format["foregroundColor"] = parse_color(v)?;
        fields.push("userEnteredFormat.textFormat.foregroundColor".into());
    }

    let mut cell_format = json!({});
    if text_format.as_object().is_some_and(|o| !o.is_empty()) {
        cell_format["textFormat"] = text_format;
    }
    if let Some(v) = args.get("background_color") {
        cell_format["backgroundColor"] = parse_color(v)?;
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
/// need with a small typed surface (type enum + optional pattern). It must
/// also target an explicitly-bounded range — an omitted bound would silently
/// reformat the rest of the sheet.
/// What: Builds a `repeatCell` request that sets `numberFormat`.
/// Test: `number_format_request_shape`, `number_format_without_bounds_is_rejected`
/// below.
fn build_number_format(args: &Value) -> Result<Vec<Value>> {
    let range = parse_grid_range(args)?;
    require_bounded_range(&range)?;
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
/// raw API expresses as `mergeCells` with a merge-type enum. This is the
/// single most destructive structured action — `MERGE_ALL` over an unbounded
/// range merges the *entire tab* into one cell, silently discarding every
/// non-top-left cell's value — so an explicitly-bounded range is mandatory,
/// not optional.
/// What: Builds a `mergeCells` request (default `MERGE_ALL`) over a fully
/// bounded range.
/// Test: `merge_request_shape`, `merge_without_bounds_is_rejected` below.
fn build_merge(args: &Value) -> Result<Vec<Value>> {
    let range = parse_grid_range(args)?;
    require_bounded_range(&range)?;
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

    /// Shared fully-bounded range fragment for actions that require one.
    fn bounded_range_args() -> Value {
        json!({
            "sheet_id": 0,
            "start_row_index": 0, "end_row_index": 1,
            "start_column_index": 0, "end_column_index": 1,
        })
    }

    #[test]
    fn color_array_normalises() {
        let c = parse_color(&json!([1.0, 0.5, 0.0, 0.8])).unwrap();
        assert_eq!(c["red"], 1.0);
        assert_eq!(c["green"], 0.5);
        assert_eq!(c["blue"], 0.0);
        assert_eq!(c["alpha"], 0.8);
        // Object form passes through (partial channels allowed).
        let obj = parse_color(&json!({ "red": 0.2 })).unwrap();
        assert_eq!(obj["red"], 0.2);
    }

    #[test]
    fn color_array_rejects_out_of_range() {
        // 255 is a common caller mistake (0-255 scale instead of 0.0-1.0).
        assert!(parse_color(&json!([255, 0, 0])).is_err());
        assert!(parse_color(&json!([1.0, 1.5, 0.0])).is_err());
        assert!(parse_color(&json!({ "red": -0.1 })).is_err());
    }

    #[test]
    fn color_array_rejects_non_numeric() {
        assert!(parse_color(&json!(["red", 0, 0])).is_err());
        assert!(
            parse_color(&json!([1.0, 0.0])).is_err(),
            "needs >= 3 channels"
        );
        assert!(parse_color(&json!("red")).is_err());
    }

    #[test]
    fn format_cells_field_mask_is_precise() {
        let mut args = bounded_range_args();
        args["bold"] = json!(true);
        args["background_color"] = json!([0.9, 0.9, 0.9]);
        args["horizontal_alignment"] = json!("CENTER");
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
        // Bounded range but no style field set at all.
        assert!(build_format_cells(&bounded_range_args()).is_err());
    }

    #[test]
    fn format_cells_without_bounds_is_rejected() {
        let args = json!({ "sheet_id": 0, "bold": true });
        let err = build_format_cells(&args).unwrap_err().to_string();
        assert!(err.contains("required"), "unexpected message: {err}");
    }

    #[test]
    fn number_format_request_shape() {
        let mut args = bounded_range_args();
        args["number_format_type"] = json!("CURRENCY");
        args["pattern"] = json!("$#,##0.00");
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
    fn number_format_without_bounds_is_rejected() {
        let args = json!({ "sheet_id": 0, "number_format_type": "CURRENCY" });
        assert!(build_number_format(&args).is_err());
    }

    #[test]
    fn merge_request_shape() {
        let reqs = build_merge(&bounded_range_args()).unwrap();
        assert_eq!(reqs[0]["mergeCells"]["mergeType"], "MERGE_ALL");
        let mut args2 = bounded_range_args();
        args2["merge_type"] = json!("MERGE_COLUMNS");
        let reqs2 = build_merge(&args2).unwrap();
        assert_eq!(reqs2[0]["mergeCells"]["mergeType"], "MERGE_COLUMNS");
    }

    #[test]
    fn merge_without_bounds_is_rejected() {
        // CRITICAL regression guard: omitting any bound must NOT silently
        // build an unbounded GridRange — MERGE_ALL over the whole sheet
        // would discard every non-top-left cell's value.
        let err = build_merge(&json!({ "sheet_id": 0 }))
            .unwrap_err()
            .to_string();
        assert!(err.contains("required"), "unexpected message: {err}");

        // Partial bounds (missing end_column_index) must also be rejected.
        let partial = json!({
            "sheet_id": 0,
            "start_row_index": 0, "end_row_index": 5, "start_column_index": 0,
        });
        assert!(build_merge(&partial).is_err());
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
