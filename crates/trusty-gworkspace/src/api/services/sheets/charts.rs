//! Sheets chart creation (`addChart` batchUpdate).
//!
//! Why: Charts are a first-class Sheets deliverable in agent workflows, yet the
//! raw `addChart` request shape (nested chart spec + grid-range sources +
//! domain/series split) is fiddly to hand-write. Mirroring the Python
//! `services/sheets/core.create_chart` surface lets callers ask for a
//! bar/column/line/area/pie chart over a data grid without knowing the API.
//! What: `create_chart` builds an `addChart` batchUpdate request from a source
//! `GridRange` (first column = domain, remaining columns = series) and POSTs it.
//! Test: `#[cfg(test)]` module below covers request construction for basic and
//! pie charts, header handling, axis titles, and overlay vs new-sheet position.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::SHEETS_API_BASE;
use crate::api::services::{account_of, opt_str, require_str};

/// Why: Callers pass friendly chart-type names; the API wants a fixed enum and
/// a basic-vs-pie distinction that selects a different spec key.
/// What: Case-insensitively maps a name to `(chartType, is_pie)`.
/// Test: `chart_type_maps` below.
fn normalize_chart_type(s: &str) -> Result<(&'static str, bool)> {
    match s.to_ascii_lowercase().as_str() {
        "column" | "col" => Ok(("COLUMN", false)),
        "bar" => Ok(("BAR", false)),
        "line" => Ok(("LINE", false)),
        "area" => Ok(("AREA", false)),
        "scatter" => Ok(("SCATTER", false)),
        "pie" => Ok(("PIE", true)),
        other => Err(anyhow!(
            "unknown chart_type '{other}' (expected bar|column|line|area|scatter|pie)"
        )),
    }
}

/// Why: Every domain/series source references a single-column `GridRange`.
/// What: Builds the half-open, 0-based `GridRange` object for one column band.
/// Test: Exercised via `basic_chart_request_shape` below.
fn grid_range(sheet_id: i64, sr: i64, er: i64, sc: i64, ec: i64) -> Value {
    json!({
        "sheetId": sheet_id,
        "startRowIndex": sr,
        "endRowIndex": er,
        "startColumnIndex": sc,
        "endColumnIndex": ec,
    })
}

/// Why: Extract a required integer field with a precise error message.
/// What: Reads `key` as an i64, erroring if absent/non-integer.
/// Test: Error path covered by `missing_source_field_errors` below.
fn require_i64(args: &Value, key: &str) -> Result<i64> {
    args.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow!("missing required integer field: {key}"))
}

/// Why: Isolating the pure request builder makes the `addChart` shape unit
/// testable without a live client or network.
/// What: Builds the full `{"requests":[{"addChart":…}]}` batchUpdate body from
/// the tool arguments; first data column is the domain, the rest are series.
/// `PieChartSpec` has no `headerCount` field (unlike `BasicChartSpec`), so for
/// pie charts the header row is excluded by bumping the effective start row
/// instead of relying on the API to skip it.
/// Test: `basic_chart_request_shape`, `pie_chart_request_shape`,
/// `pie_domain_and_series_offset_when_headers_present`,
/// `pie_domain_not_offset_when_no_headers`, `no_headers_sets_zero_header_count`,
/// `overlay_position_when_not_new_sheet`, `column_chart_missing_series_errors`.
fn build_chart_request(args: &Value) -> Result<Value> {
    let (chart_type, is_pie) = normalize_chart_type(require_str(args, "chart_type")?)?;
    let source_sheet = require_i64(args, "source_sheet_id")?;
    let sr = require_i64(args, "start_row_index")?;
    let er = require_i64(args, "end_row_index")?;
    let sc = require_i64(args, "start_column_index")?;
    let ec = require_i64(args, "end_column_index")?;
    if ec <= sc {
        return Err(anyhow!(
            "end_column_index ({ec}) must be greater than start_column_index ({sc})"
        ));
    }
    // Need at least one domain column plus one series column.
    if ec - sc < 2 {
        return Err(anyhow!(
            "chart source range needs at least 2 columns (1 domain + >=1 series), got {}",
            ec - sc
        ));
    }

    let has_headers = args
        .get("has_headers")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let header_count = if has_headers { 1 } else { 0 };

    // BasicChartSpec has a headerCount field so the API skips the header row
    // itself; PieChartSpec does not, so pie ranges must exclude it manually or
    // the header label shows up as a bogus extra slice.
    let pie_sr = sr + header_count;
    if is_pie && pie_sr >= er {
        return Err(anyhow!(
            "chart source range has no data rows left after excluding the header row"
        ));
    }
    let effective_sr = if is_pie { pie_sr } else { sr };

    // First column is the domain; each remaining column is one series.
    let domain_source = json!({
        "sourceRange": { "sources": [grid_range(source_sheet, effective_sr, er, sc, sc + 1)] }
    });
    let series: Vec<Value> = (sc + 1..ec)
        .map(|col| {
            json!({
                "series": { "sourceRange": { "sources": [grid_range(source_sheet, effective_sr, er, col, col + 1)] } }
            })
        })
        .collect();

    let legend = opt_str(args, "legend_position").unwrap_or("BOTTOM_LEGEND");

    let mut spec = json!({});
    if let Some(title) = opt_str(args, "title") {
        spec["title"] = json!(title);
    }

    if is_pie {
        // Guaranteed non-empty by the `ec - sc < 2` check above; the
        // `ok_or_else` is a defensive invariant, not a reachable error path.
        let first_series = series
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("pie chart needs at least one series column"))?;
        spec["pieChart"] = json!({
            "legendPosition": legend,
            "domain": domain_source,
            "series": first_series["series"].clone(),
        });
    } else {
        let mut axis = Vec::<Value>::new();
        if let Some(t) = opt_str(args, "x_axis_title") {
            axis.push(json!({ "position": "BOTTOM_AXIS", "title": t }));
        }
        if let Some(t) = opt_str(args, "y_axis_title") {
            axis.push(json!({ "position": "LEFT_AXIS", "title": t }));
        }
        let mut basic = json!({
            "chartType": chart_type,
            "legendPosition": legend,
            "headerCount": header_count,
            "domains": [{ "domain": domain_source }],
            "series": series,
        });
        if !axis.is_empty() {
            basic["axis"] = json!(axis);
        }
        spec["basicChart"] = basic;
    }

    // Position: default a new sheet; otherwise overlay on an anchor cell.
    let new_sheet = args
        .get("new_sheet")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let position = if new_sheet {
        json!({ "newSheet": true })
    } else {
        let anchor_sheet = args
            .get("position_sheet_id")
            .and_then(Value::as_i64)
            .unwrap_or(source_sheet);
        let anchor_row = args.get("anchor_row").and_then(Value::as_i64).unwrap_or(0);
        let anchor_col = args
            .get("anchor_column")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        json!({
            "overlayPosition": {
                "anchorCell": {
                    "sheetId": anchor_sheet,
                    "rowIndex": anchor_row,
                    "columnIndex": anchor_col,
                }
            }
        })
    };

    Ok(json!({
        "requests": [{
            "addChart": { "chart": { "spec": spec, "position": position } }
        }]
    }))
}

/// Why: Charts round out the Sheets tool surface at parity with the Python
/// upstream; agents can visualise a data range in one call.
/// What: Builds the `addChart` batchUpdate body and POSTs it to the spreadsheet.
/// Test: Body construction covered by the `#[cfg(test)]` module; the POST leg is
/// live-only.
pub async fn create_chart(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "spreadsheet_id")?;
    let body = build_chart_request(&args)?;
    let url = format!("{SHEETS_API_BASE}/spreadsheets/{id}:batchUpdate");
    client.post(&url, body, account).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_args(chart_type: &str) -> Value {
        json!({
            "spreadsheet_id": "SS",
            "chart_type": chart_type,
            "source_sheet_id": 0,
            "start_row_index": 0,
            "end_row_index": 5,
            "start_column_index": 0,
            "end_column_index": 3,
        })
    }

    #[test]
    fn chart_type_maps() {
        assert_eq!(normalize_chart_type("Column").unwrap(), ("COLUMN", false));
        assert_eq!(normalize_chart_type("PIE").unwrap(), ("PIE", true));
        assert_eq!(normalize_chart_type("area").unwrap(), ("AREA", false));
        assert!(normalize_chart_type("donut").is_err());
    }

    #[test]
    fn basic_chart_request_shape() {
        let mut args = base_args("column");
        args["title"] = json!("Sales");
        args["x_axis_title"] = json!("Month");
        args["y_axis_title"] = json!("Revenue");
        let body = build_chart_request(&args).unwrap();
        let spec = &body["requests"][0]["addChart"]["chart"]["spec"];
        assert_eq!(spec["title"], "Sales");
        let basic = &spec["basicChart"];
        assert_eq!(basic["chartType"], "COLUMN");
        assert_eq!(basic["headerCount"], 1);
        // Domain = column 0; series = columns 1 and 2 (two series).
        assert_eq!(basic["series"].as_array().unwrap().len(), 2);
        let domain_src = &basic["domains"][0]["domain"]["sourceRange"]["sources"][0];
        assert_eq!(domain_src["startColumnIndex"], 0);
        assert_eq!(domain_src["endColumnIndex"], 1);
        let s1 = &basic["series"][0]["series"]["sourceRange"]["sources"][0];
        assert_eq!(s1["startColumnIndex"], 1);
        assert_eq!(s1["endColumnIndex"], 2);
        assert_eq!(basic["axis"].as_array().unwrap().len(), 2);
        // Default position is a new sheet.
        assert_eq!(
            body["requests"][0]["addChart"]["chart"]["position"]["newSheet"],
            true
        );
    }

    #[test]
    fn pie_chart_request_shape() {
        let body = build_chart_request(&base_args("pie")).unwrap();
        let spec = &body["requests"][0]["addChart"]["chart"]["spec"];
        assert!(spec.get("basicChart").is_none());
        let pie = &spec["pieChart"];
        assert_eq!(pie["legendPosition"], "BOTTOM_LEGEND");
        // Pie domain = column 0, series = first value column (column 1).
        let dom = &pie["domain"]["sourceRange"]["sources"][0];
        assert_eq!(dom["startColumnIndex"], 0);
        let ser = &pie["series"]["sourceRange"]["sources"][0];
        assert_eq!(ser["startColumnIndex"], 1);
        assert_eq!(ser["endColumnIndex"], 2);
    }

    #[test]
    fn pie_domain_and_series_offset_when_headers_present() {
        // has_headers defaults to true; the header row (row 0) must be
        // excluded from both the pie domain and series ranges, or the header
        // label shows up as a bogus extra slice.
        let body = build_chart_request(&base_args("pie")).unwrap();
        let spec = &body["requests"][0]["addChart"]["chart"]["spec"];
        let dom = &spec["pieChart"]["domain"]["sourceRange"]["sources"][0];
        assert_eq!(dom["startRowIndex"], 1);
        let ser = &spec["pieChart"]["series"]["sourceRange"]["sources"][0];
        assert_eq!(ser["startRowIndex"], 1);
    }

    #[test]
    fn pie_domain_not_offset_when_no_headers() {
        let mut args = base_args("pie");
        args["has_headers"] = json!(false);
        let body = build_chart_request(&args).unwrap();
        let spec = &body["requests"][0]["addChart"]["chart"]["spec"];
        let dom = &spec["pieChart"]["domain"]["sourceRange"]["sources"][0];
        assert_eq!(dom["startRowIndex"], 0);
        let ser = &spec["pieChart"]["series"]["sourceRange"]["sources"][0];
        assert_eq!(ser["startRowIndex"], 0);
    }

    #[test]
    fn pie_all_rows_consumed_by_header_errors() {
        let mut args = base_args("pie");
        args["start_row_index"] = json!(4);
        args["end_row_index"] = json!(5); // single row, which is the header
        assert!(build_chart_request(&args).is_err());
    }

    #[test]
    fn no_headers_sets_zero_header_count() {
        let mut args = base_args("bar");
        args["has_headers"] = json!(false);
        let body = build_chart_request(&args).unwrap();
        assert_eq!(
            body["requests"][0]["addChart"]["chart"]["spec"]["basicChart"]["headerCount"],
            0
        );
    }

    #[test]
    fn overlay_position_when_not_new_sheet() {
        let mut args = base_args("line");
        args["new_sheet"] = json!(false);
        args["position_sheet_id"] = json!(7);
        args["anchor_row"] = json!(2);
        args["anchor_column"] = json!(4);
        let body = build_chart_request(&args).unwrap();
        let anchor =
            &body["requests"][0]["addChart"]["chart"]["position"]["overlayPosition"]["anchorCell"];
        assert_eq!(anchor["sheetId"], 7);
        assert_eq!(anchor["rowIndex"], 2);
        assert_eq!(anchor["columnIndex"], 4);
    }

    #[test]
    fn missing_source_field_errors() {
        let mut args = base_args("column");
        args.as_object_mut().unwrap().remove("end_row_index");
        assert!(build_chart_request(&args).is_err());
    }

    #[test]
    fn empty_column_span_errors() {
        let mut args = base_args("column");
        args["end_column_index"] = json!(0);
        assert!(build_chart_request(&args).is_err());
    }

    #[test]
    fn column_chart_missing_series_errors() {
        // Domain-only range (end_col == start_col + 1) leaves zero series
        // columns for a non-pie chart too — must be rejected locally rather
        // than sent to the API with an empty `series: []`.
        let mut args = base_args("column");
        args["end_column_index"] = json!(1);
        assert!(build_chart_request(&args).is_err());
    }
}
