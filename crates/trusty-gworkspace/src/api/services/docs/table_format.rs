//! Docs whole-document table beautifier.
//!
//! Why: A post-processing pass that walks every table in a doc and applies
//! borders, a styled header row, and content-aware column widths saves callers
//! from computing per-table geometry.
//! What: `format_document_tables` reads the doc once, then for each table emits
//! border/header/width `batchUpdate` requests (chunked under the API limit).
//! Test: `compute_content_aware_widths` and the request builders are unit-tested
//! below; the round-trip is live-only.

use anyhow::Result;
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::DOCS_API_BASE;
use crate::api::services::{account_of, require_str};

const CONTENT_CAP: usize = 60;
const MIN_COL_PT: f64 = 40.0;
const FALLBACK_WIDTH: f64 = 468.0;
const BORDER_WIDTH_PT: f64 = 1.0;
const BATCH_CHUNK: usize = 200;

fn border_color() -> Value {
    json!({ "red": 0.4, "green": 0.4, "blue": 0.4 })
}
fn header_bg() -> Value {
    json!({ "red": 0.9, "green": 0.9, "blue": 0.9 })
}

/// Why: Text-heavy columns should be wider than narrow numeric ones without a
/// font engine; a capped content-length weighting approximates that.
/// What: Weights each column by `min(max_chars, cap)`, distributes
/// `usable_width` proportionally, clamps to `MIN_COL_PT`, and renormalises so
/// widths sum to `usable_width`.
/// Test: `content_aware_widths_*` below.
pub(crate) fn compute_content_aware_widths(
    table_cells: &[Vec<String>],
    usable_width: f64,
    cap: usize,
    min_col_pt: f64,
) -> Vec<f64> {
    if table_cells.is_empty() {
        return Vec::new();
    }
    let num_cols = table_cells.iter().map(|r| r.len()).max().unwrap_or(0);
    if num_cols == 0 {
        return Vec::new();
    }

    let weights: Vec<f64> = (0..num_cols)
        .map(|c| {
            let max_chars = table_cells
                .iter()
                .filter_map(|row| row.get(c))
                .map(|cell| cell.chars().count())
                .max()
                .unwrap_or(1)
                .max(1);
            max_chars.min(cap) as f64
        })
        .collect();

    let mut final_widths = vec![0.0f64; num_cols];
    let mut clamped = vec![false; num_cols];
    let mut remaining = usable_width;

    // NOTE (non-blocking, matches the Python upstream algorithm verbatim):
    // `free_weight` is recomputed once per outer pass, then reused as the
    // denominator for every column checked within that SAME inner loop —
    // including columns that get clamped partway through the pass. On a
    // wide table with many columns this can under-allocate a column that
    // is checked late in a pass (its provisional share is computed against
    // a `free_weight` that hasn't yet shed the weight of columns clamped
    // earlier in the same pass), pushing it slightly below `min_col_pt`
    // before the final renormalisation pass runs. This never panics and
    // the post-loop renormalisation still makes the total sum to
    // `usable_width` exactly; only the very last column(s) processed in a
    // pass could end up marginally under the nominal minimum on pathological
    // (very-wide, many-column) inputs.
    for _ in 0..=num_cols {
        let free_weight: f64 = (0..num_cols)
            .filter(|&i| !clamped[i])
            .map(|i| weights[i])
            .sum();
        if free_weight <= 0.0 {
            break;
        }
        let mut newly_clamped = false;
        for i in 0..num_cols {
            if clamped[i] {
                continue;
            }
            let provisional = remaining * weights[i] / free_weight;
            if provisional < min_col_pt {
                final_widths[i] = min_col_pt;
                clamped[i] = true;
                remaining -= min_col_pt;
                newly_clamped = true;
            }
        }
        if !newly_clamped {
            for i in 0..num_cols {
                if !clamped[i] {
                    final_widths[i] = remaining * weights[i] / free_weight;
                }
            }
            break;
        }
    }

    for w in &mut final_widths {
        if *w == 0.0 {
            *w = min_col_pt;
        }
    }

    // Renormalise so the widths sum to usable_width exactly.
    let total_clamped: f64 = (0..num_cols)
        .filter(|&i| clamped[i])
        .map(|i| final_widths[i])
        .sum();
    let free_cols: Vec<usize> = (0..num_cols).filter(|&i| !clamped[i]).collect();
    if !free_cols.is_empty() {
        let remaining_for_free = usable_width - total_clamped;
        let current_free_total: f64 = free_cols.iter().map(|&i| final_widths[i]).sum();
        if current_free_total > 0.0 && remaining_for_free > 0.0 {
            let scale = remaining_for_free / current_free_total;
            for &i in &free_cols {
                final_widths[i] *= scale;
            }
        }
    } else {
        let total: f64 = final_widths.iter().sum();
        if total > 0.0 {
            let scale = usable_width / total;
            for w in &mut final_widths {
                *w *= scale;
            }
        }
    }

    final_widths
}

/// Return the concatenated, trimmed plain text of a table cell.
pub(crate) fn extract_cell_text(cell: &Value) -> String {
    let mut parts = String::new();
    if let Some(content) = cell.get("content").and_then(|c| c.as_array()) {
        for el in content {
            if let Some(elements) = el
                .get("paragraph")
                .and_then(|p| p.get("elements"))
                .and_then(|e| e.as_array())
            {
                for pe in elements {
                    if let Some(t) = pe
                        .get("textRun")
                        .and_then(|t| t.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        parts.push_str(t);
                    }
                }
            }
        }
    }
    parts.trim().to_string()
}

/// Extract usable page width (page width minus L/R margins) or a fallback.
pub(crate) fn get_usable_width(doc: &Value) -> f64 {
    let ds = doc.get("documentStyle");
    let mag = |path: &[&str]| -> f64 {
        let mut cur = ds;
        for p in path {
            cur = cur.and_then(|v| v.get(p));
        }
        cur.and_then(|v| v.get("magnitude"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    };
    let page_width = mag(&["pageSize", "width"]);
    let margin_left = mag(&["marginLeft"]);
    let margin_right = mag(&["marginRight"]);
    if page_width > 0.0 && (margin_left + margin_right) < page_width {
        page_width - margin_left - margin_right
    } else {
        FALLBACK_WIDTH
    }
}

/// Build 1pt-solid border requests for every cell in a table.
pub(crate) fn build_border_requests(
    table_start_index: i64,
    num_rows: i64,
    num_cols: i64,
) -> Vec<Value> {
    let border_obj = json!({
        "color": { "color": { "rgbColor": border_color() } },
        "width": { "magnitude": BORDER_WIDTH_PT, "unit": "PT" },
        "dashStyle": "SOLID",
    });
    let mut requests = Vec::new();
    for row_idx in 0..num_rows {
        for col_idx in 0..num_cols {
            requests.push(json!({
                "updateTableCellStyle": {
                    "tableCellStyle": {
                        "borderTop": border_obj,
                        "borderBottom": border_obj,
                        "borderLeft": border_obj,
                        "borderRight": border_obj,
                    },
                    "fields": "borderTop,borderBottom,borderLeft,borderRight",
                    "tableRange": {
                        "tableCellLocation": {
                            "tableStartLocation": { "index": table_start_index },
                            "rowIndex": row_idx,
                            "columnIndex": col_idx,
                        },
                        "rowSpan": 1,
                        "columnSpan": 1,
                    },
                }
            }));
        }
    }
    requests
}

/// Build header-row requests: light-grey background per column + bold over each
/// header cell's full text range.
pub(crate) fn build_header_requests(
    table_start_index: i64,
    num_cols: i64,
    header_cell_ranges: &[(i64, i64)],
) -> Vec<Value> {
    let mut requests = Vec::new();
    for col_idx in 0..num_cols {
        requests.push(json!({
            "updateTableCellStyle": {
                "tableCellStyle": { "backgroundColor": { "color": { "rgbColor": header_bg() } } },
                "fields": "backgroundColor",
                "tableRange": {
                    "tableCellLocation": {
                        "tableStartLocation": { "index": table_start_index },
                        "rowIndex": 0,
                        "columnIndex": col_idx,
                    },
                    "rowSpan": 1,
                    "columnSpan": 1,
                },
            }
        }));
    }
    for &(start, end) in header_cell_ranges {
        if start > 0 && end > start {
            requests.push(json!({
                "updateTextStyle": {
                    "range": { "startIndex": start, "endIndex": end },
                    "textStyle": { "bold": true },
                    "fields": "bold",
                }
            }));
        }
    }
    requests
}

/// Build fixed-width `updateTableColumnProperties` requests.
pub(crate) fn build_column_width_requests(table_start_index: i64, widths: &[f64]) -> Vec<Value> {
    widths
        .iter()
        .enumerate()
        .map(|(col_idx, &w)| {
            json!({
                "updateTableColumnProperties": {
                    "tableStartLocation": { "index": table_start_index },
                    "columnIndices": [col_idx as i64],
                    "tableColumnProperties": {
                        "widthType": "FIXED_WIDTH",
                        "width": { "magnitude": w, "unit": "PT" },
                    },
                    "fields": "widthType,width",
                }
            })
        })
        .collect()
}

/// Return `(startIndex, endIndex)` of the first paragraph of each header cell.
pub(crate) fn find_header_cell_ranges(
    body_content: &[Value],
    table_start_index: i64,
) -> Vec<(i64, i64)> {
    for element in body_content {
        let Some(table) = element.get("table") else {
            continue;
        };
        if element.get("startIndex").and_then(|v| v.as_i64()) != Some(table_start_index) {
            continue;
        }
        let rows = table.get("tableRows").and_then(|r| r.as_array());
        let Some(rows) = rows else { return Vec::new() };
        let Some(first_row) = rows.first() else {
            return Vec::new();
        };
        let mut ranges = Vec::new();
        if let Some(cells) = first_row.get("tableCells").and_then(|c| c.as_array()) {
            for cell in cells {
                let content = cell
                    .get("content")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first());
                let start = content
                    .and_then(|c| c.get("startIndex"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let end = content
                    .and_then(|c| c.get("endIndex"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(start);
                ranges.push((start, end));
            }
        }
        return ranges;
    }
    Vec::new()
}

/// Why: One call to normalise the appearance of every table in a doc.
/// What: Reads the doc, gathers tables, and posts border+header+width requests
/// per table in chunks under the batchUpdate limit.
/// Test: The pure builders above are unit-tested; the round-trip is live-only.
pub async fn format_document_tables(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "document_id")?;
    let doc = client
        .get(&format!("{DOCS_API_BASE}/documents/{id}"), account)
        .await?;
    let batch_url = format!("{DOCS_API_BASE}/documents/{id}:batchUpdate");

    let usable_width = get_usable_width(&doc);
    let empty = Vec::new();
    let body_content = doc
        .get("body")
        .and_then(|b| b.get("content"))
        .and_then(|c| c.as_array())
        .unwrap_or(&empty);

    // Gather tables: (start_index, num_rows, num_cols, cell_texts).
    struct TableInfo {
        start_index: i64,
        num_rows: i64,
        num_cols: i64,
        cell_texts: Vec<Vec<String>>,
    }
    let mut tables = Vec::<TableInfo>::new();
    for element in body_content {
        let Some(table) = element.get("table") else {
            continue;
        };
        let rows = table.get("tableRows").and_then(|r| r.as_array());
        let Some(rows) = rows else { continue };
        let num_rows = rows.len() as i64;
        let num_cols = rows
            .iter()
            .map(|r| {
                r.get("tableCells")
                    .and_then(|c| c.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0) as i64;
        if num_rows == 0 || num_cols == 0 {
            continue;
        }
        let cell_texts: Vec<Vec<String>> = rows
            .iter()
            .map(|row| {
                row.get("tableCells")
                    .and_then(|c| c.as_array())
                    .map(|cells| cells.iter().map(extract_cell_text).collect())
                    .unwrap_or_default()
            })
            .collect();
        let start_index = element
            .get("startIndex")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        tables.push(TableInfo {
            start_index,
            num_rows,
            num_cols,
            cell_texts,
        });
    }

    if tables.is_empty() {
        return Ok(json!({
            "status": "no_tables_found",
            "document_id": id,
            "tables_processed": 0,
        }));
    }

    let mut total_requests_sent = 0usize;
    for table in &tables {
        let mut all_requests =
            build_border_requests(table.start_index, table.num_rows, table.num_cols);
        let header_ranges = find_header_cell_ranges(body_content, table.start_index);
        all_requests.extend(build_header_requests(
            table.start_index,
            table.num_cols,
            &header_ranges,
        ));
        let widths =
            compute_content_aware_widths(&table.cell_texts, usable_width, CONTENT_CAP, MIN_COL_PT);
        all_requests.extend(build_column_width_requests(table.start_index, &widths));

        for chunk in all_requests.chunks(BATCH_CHUNK) {
            client
                .post(&batch_url, json!({ "requests": chunk }), account)
                .await?;
            total_requests_sent += chunk.len();
        }
    }

    Ok(json!({
        "status": "formatted",
        "document_id": id,
        "document_url": format!("https://docs.google.com/document/d/{id}/edit"),
        "tables_processed": tables.len(),
        "usable_width_pt": usable_width,
        "total_requests_sent": total_requests_sent,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells(grid: &[&[&str]]) -> Vec<Vec<String>> {
        grid.iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    #[test]
    fn content_aware_widths_sum_to_usable() {
        let data = cells(&[
            &["a", "much longer content here"],
            &["b", "another wide body"],
        ]);
        let widths = compute_content_aware_widths(&data, 468.0, CONTENT_CAP, MIN_COL_PT);
        assert_eq!(widths.len(), 2);
        let total: f64 = widths.iter().sum();
        assert!((total - 468.0).abs() < 0.01, "total {total} != 468");
        // Text-heavy column 1 wider than the narrow column 0.
        assert!(widths[1] > widths[0]);
    }

    #[test]
    fn content_aware_widths_respect_min_clamp() {
        let data = cells(&[&["x", "y", "z"]]);
        let widths = compute_content_aware_widths(&data, 468.0, CONTENT_CAP, MIN_COL_PT);
        assert!(widths.iter().all(|w| *w >= MIN_COL_PT - 0.01));
    }

    #[test]
    fn content_aware_widths_empty() {
        assert!(compute_content_aware_widths(&[], 468.0, CONTENT_CAP, MIN_COL_PT).is_empty());
    }

    #[test]
    fn border_requests_one_per_cell() {
        let reqs = build_border_requests(10, 2, 3);
        assert_eq!(reqs.len(), 6);
        assert_eq!(
            reqs[0]["updateTableCellStyle"]["fields"],
            "borderTop,borderBottom,borderLeft,borderRight"
        );
    }

    #[test]
    fn header_requests_bg_plus_bold() {
        let reqs = build_header_requests(0, 2, &[(3, 8), (9, 14)]);
        // 2 background requests + 2 bold requests.
        assert_eq!(reqs.len(), 4);
        assert_eq!(reqs[2]["updateTextStyle"]["range"]["startIndex"], 3);
        assert_eq!(reqs[2]["updateTextStyle"]["range"]["endIndex"], 8);
    }

    #[test]
    fn header_requests_skip_empty_ranges() {
        let reqs = build_header_requests(0, 1, &[(0, 0), (5, 5)]);
        // Only 1 background request; both bold ranges are degenerate.
        assert_eq!(reqs.len(), 1);
    }

    #[test]
    fn column_width_requests_fixed() {
        let reqs = build_column_width_requests(7, &[80.0, 120.0]);
        assert_eq!(reqs.len(), 2);
        assert_eq!(
            reqs[1]["updateTableColumnProperties"]["tableColumnProperties"]["width"]["magnitude"],
            120.0
        );
    }

    #[test]
    fn usable_width_from_document_style() {
        let doc = json!({
            "documentStyle": {
                "pageSize": { "width": { "magnitude": 612.0 } },
                "marginLeft": { "magnitude": 72.0 },
                "marginRight": { "magnitude": 72.0 },
            }
        });
        assert_eq!(get_usable_width(&doc), 468.0);
    }

    #[test]
    fn usable_width_fallback() {
        assert_eq!(get_usable_width(&json!({})), FALLBACK_WIDTH);
    }

    #[test]
    fn extract_cell_text_trims() {
        let cell = json!({
            "content": [ { "paragraph": { "elements": [ { "textRun": { "content": " Name \n" } } ] } } ]
        });
        assert_eq!(extract_cell_text(&cell), "Name");
    }

    #[test]
    fn header_cell_ranges_from_body() {
        let body = vec![json!({
            "startIndex": 5,
            "table": { "tableRows": [
                { "tableCells": [
                    { "content": [{ "startIndex": 7, "endIndex": 12 }] },
                    { "content": [{ "startIndex": 12, "endIndex": 15 }] },
                ] },
            ] }
        })];
        assert_eq!(find_header_cell_ranges(&body, 5), vec![(7, 12), (12, 15)]);
    }
}
