//! Docs whole-table style presets.
//!
//! Why: Applying a coherent look (borders + header + zebra striping) to a whole
//! table by hand is many `updateTableCellStyle` requests; named presets make it
//! one call.
//! What: `apply_table_style` merges a named preset (`minimal`, `bordered`,
//! `striped`, `professional`, `plain`) with optional custom overrides and emits
//! per-cell style requests plus optional header-bold text styling.
//! Test: The preset table, merge, and per-cell request builder are unit-tested
//! below; the round-trip is live-only.

use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::DOCS_API_BASE;
use crate::api::services::{account_of, require_str};

/// Why: Presets encode sensible border/padding/background defaults so callers
/// pick a name rather than assembling colours by hand.
/// What: Returns the style object for a named preset (empty for `plain`/unknown).
/// Test: `preset_lookup` below.
pub(crate) fn table_style_preset(name: &str) -> Value {
    match name {
        "minimal" => json!({
            "border_color": { "red": 0.9, "green": 0.9, "blue": 0.9 },
            "border_width": 0.5,
            "border_dash_style": "SOLID",
            "cell_padding": { "top": 4, "bottom": 4, "left": 6, "right": 6 },
        }),
        "bordered" => json!({
            "border_color": { "red": 0.4, "green": 0.4, "blue": 0.4 },
            "border_width": 1.0,
            "border_dash_style": "SOLID",
            "cell_padding": { "top": 4, "bottom": 4, "left": 6, "right": 6 },
        }),
        "striped" => json!({
            "border_color": { "red": 0.85, "green": 0.85, "blue": 0.85 },
            "border_width": 0.5,
            "border_dash_style": "SOLID",
            "odd_row_background": { "red": 1.0, "green": 1.0, "blue": 1.0 },
            "even_row_background": { "red": 0.95, "green": 0.95, "blue": 0.97 },
            "cell_padding": { "top": 4, "bottom": 4, "left": 6, "right": 6 },
        }),
        "professional" => json!({
            "header_background": { "red": 0.2, "green": 0.35, "blue": 0.6 },
            "header_text_bold": true,
            "odd_row_background": { "red": 1.0, "green": 1.0, "blue": 1.0 },
            "even_row_background": { "red": 0.93, "green": 0.95, "blue": 0.98 },
            "border_color": { "red": 0.7, "green": 0.75, "blue": 0.85 },
            "border_width": 0.75,
            "border_dash_style": "SOLID",
            "cell_padding": { "top": 5, "bottom": 5, "left": 8, "right": 8 },
        }),
        // "plain" and unknown -> Google Docs default (no changes).
        _ => json!({}),
    }
}

/// Why: `custom` overrides individual preset fields; a plain merge captures that.
/// What: Returns preset with every key from `custom` overwriting it.
/// Test: `merge_style_overrides` below.
pub(crate) fn merge_style(preset: Value, custom: &Value) -> Value {
    let mut base: Map<String, Value> = preset.as_object().cloned().unwrap_or_default();
    if let Some(over) = custom.as_object() {
        for (k, v) in over {
            base.insert(k.clone(), v.clone());
        }
    }
    Value::Object(base)
}

/// Walk the doc body for a table at `table_start_index`, returning each cell's
/// first-paragraph start index as a `[row][col]` grid.
///
/// Why: Header-bold styling needs the text start index of each header cell.
/// What: Locates the table element and reads `content[0].startIndex` per cell.
/// Test: `find_cell_indices_grid` below.
pub(crate) fn find_table_cell_indices(
    body_content: &[Value],
    table_start_index: i64,
) -> Vec<Vec<i64>> {
    for element in body_content {
        let Some(table) = element.get("table") else {
            continue;
        };
        if element.get("startIndex").and_then(|v| v.as_i64()) != Some(table_start_index) {
            continue;
        }
        let mut grid = Vec::new();
        if let Some(rows) = table.get("tableRows").and_then(|r| r.as_array()) {
            for row in rows {
                let mut row_cells = Vec::new();
                if let Some(cells) = row.get("tableCells").and_then(|c| c.as_array()) {
                    for cell in cells {
                        let idx = cell
                            .get("content")
                            .and_then(|c| c.as_array())
                            .and_then(|a| a.first())
                            .and_then(|c| c.get("startIndex"))
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0);
                        row_cells.push(idx);
                    }
                }
                grid.push(row_cells);
            }
        }
        return grid;
    }
    Vec::new()
}

/// Build the per-cell `updateTableCellStyle` requests for a merged style.
///
/// Why: Isolating the request assembly keeps it pure and unit-testable.
/// What: For each cell applies border (if color+width), padding, and row
/// background (header/odd/even), emitting a request only when something is set.
/// Test: `style_requests_*` below.
pub(crate) fn build_table_style_requests(
    table_start_index: i64,
    num_rows: i64,
    num_columns: i64,
    header_row: bool,
    style: &Value,
) -> Vec<Value> {
    let border_color = style.get("border_color");
    let border_width = style.get("border_width").and_then(|v| v.as_f64());
    let border_dash = style
        .get("border_dash_style")
        .and_then(|v| v.as_str())
        .unwrap_or("SOLID");
    let cell_padding = style.get("cell_padding");
    let header_background = style.get("header_background");
    let odd_row_background = style.get("odd_row_background");
    let even_row_background = style.get("even_row_background");

    let mut requests = Vec::new();
    for row_idx in 0..num_rows {
        let is_header = header_row && row_idx == 0;
        let row_bg: Option<&Value> = if is_header {
            header_background
        } else if row_idx % 2 == 0 {
            even_row_background
        } else {
            odd_row_background
        };

        for col_idx in 0..num_columns {
            let mut cell_style = json!({});
            let mut fields = Vec::<&str>::new();

            if let (Some(color), Some(width)) = (border_color, border_width) {
                let border_obj = json!({
                    "color": { "color": { "rgbColor": color } },
                    "width": { "magnitude": width, "unit": "PT" },
                    "dashStyle": border_dash,
                });
                for key in ["borderTop", "borderBottom", "borderLeft", "borderRight"] {
                    cell_style[key] = border_obj.clone();
                    fields.push(key);
                }
            }

            if let Some(pad) = cell_padding {
                for (side, key) in [
                    ("top", "paddingTop"),
                    ("bottom", "paddingBottom"),
                    ("left", "paddingLeft"),
                    ("right", "paddingRight"),
                ] {
                    let mag = pad.get(side).and_then(|v| v.as_f64()).unwrap_or(0.0);
                    cell_style[key] = json!({ "magnitude": mag, "unit": "PT" });
                    fields.push(key);
                }
            }

            if let Some(bg) = row_bg {
                cell_style["backgroundColor"] = json!({ "color": { "rgbColor": bg } });
                fields.push("backgroundColor");
            }

            if !fields.is_empty() {
                requests.push(json!({
                    "updateTableCellStyle": {
                        "tableCellStyle": cell_style,
                        "fields": fields.join(","),
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
    }
    requests
}

/// Why: Present a whole-table styling verb keyed by a named preset.
/// What: Merges preset+custom, builds per-cell requests, optionally fetches the
/// doc to bold header cells, then posts one batch.
/// Test: The builders above are unit-tested; the call is live-only.
pub async fn apply_table_style(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "document_id")?;
    let table_start_index = args
        .get("table_start_index")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing table_start_index"))?;
    let num_rows = args
        .get("num_rows")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing num_rows"))?;
    let num_columns = args
        .get("num_columns")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing num_columns"))?;
    let header_row = args
        .get("header_row")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let preset_name = args
        .get("preset")
        .and_then(|v| v.as_str())
        .unwrap_or("plain");
    let custom = args.get("custom").cloned().unwrap_or_else(|| json!({}));

    let style = merge_style(table_style_preset(preset_name), &custom);
    if style.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        return Ok(
            json!({ "status": "no_style_applied", "preset": preset_name, "document_id": id }),
        );
    }

    let mut requests =
        build_table_style_requests(table_start_index, num_rows, num_columns, header_row, &style);

    // Header bold requires the doc to resolve cell text start indices.
    let header_text_bold = style
        .get("header_text_bold")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if header_row && header_text_bold {
        let url = format!("{DOCS_API_BASE}/documents/{id}?fields=body");
        let doc = client.get(&url, account).await?;
        let empty = Vec::new();
        let body_content = doc
            .get("body")
            .and_then(|b| b.get("content"))
            .and_then(|c| c.as_array())
            .unwrap_or(&empty);
        let grid = find_table_cell_indices(body_content, table_start_index);
        if let Some(first_row) = grid.first() {
            for &start_idx in first_row {
                if start_idx > 0 {
                    requests.push(json!({
                        "updateTextStyle": {
                            "range": { "startIndex": start_idx, "endIndex": start_idx + 1 },
                            "textStyle": { "bold": true },
                            "fields": "bold",
                        }
                    }));
                }
            }
        }
    }

    if requests.is_empty() {
        return Ok(
            json!({ "status": "no_style_applied", "preset": preset_name, "document_id": id }),
        );
    }
    let url = format!("{DOCS_API_BASE}/documents/{id}:batchUpdate");
    client
        .post(&url, json!({ "requests": requests.clone() }), account)
        .await?;
    Ok(json!({
        "status": "styled",
        "document_id": id,
        "table_start_index": table_start_index,
        "preset": preset_name,
        "num_rows": num_rows,
        "num_columns": num_columns,
        "header_row": header_row,
        "requests_sent": requests.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_lookup() {
        assert!(table_style_preset("plain").as_object().unwrap().is_empty());
        assert_eq!(table_style_preset("bordered")["border_width"], 1.0);
        assert_eq!(table_style_preset("professional")["header_text_bold"], true);
        assert!(table_style_preset("nope").as_object().unwrap().is_empty());
    }

    #[test]
    fn merge_style_overrides() {
        let merged = merge_style(
            table_style_preset("bordered"),
            &json!({ "border_width": 3.0, "header_text_bold": true }),
        );
        assert_eq!(merged["border_width"], 3.0);
        assert_eq!(merged["header_text_bold"], true);
        // Untouched preset key survives.
        assert_eq!(merged["border_dash_style"], "SOLID");
    }

    #[test]
    fn style_requests_border_and_padding() {
        let style = table_style_preset("bordered");
        let reqs = build_table_style_requests(10, 2, 2, true, &style);
        // 2x2 cells, each with border+padding -> 4 requests.
        assert_eq!(reqs.len(), 4);
        let fields = reqs[0]["updateTableCellStyle"]["fields"].as_str().unwrap();
        assert!(fields.contains("borderTop"));
        assert!(fields.contains("paddingTop"));
    }

    #[test]
    fn style_requests_zebra_backgrounds() {
        let style = table_style_preset("striped");
        let reqs = build_table_style_requests(0, 3, 1, false, &style);
        // Row 0 (even) gets even_row_background; row 1 (odd) gets odd.
        assert_eq!(
            reqs[0]["updateTableCellStyle"]["tableCellStyle"]["backgroundColor"]["color"]["rgbColor"]
                ["blue"],
            0.97
        );
        assert_eq!(
            reqs[1]["updateTableCellStyle"]["tableCellStyle"]["backgroundColor"]["color"]["rgbColor"]
                ["blue"],
            1.0
        );
    }

    #[test]
    fn style_requests_header_background() {
        let style = table_style_preset("professional");
        let reqs = build_table_style_requests(0, 2, 1, true, &style);
        // Header row 0 gets header_background (blue 0.6).
        assert_eq!(
            reqs[0]["updateTableCellStyle"]["tableCellStyle"]["backgroundColor"]["color"]["rgbColor"]
                ["blue"],
            0.6
        );
    }

    #[test]
    fn style_requests_empty_for_plain() {
        let reqs = build_table_style_requests(0, 2, 2, true, &json!({}));
        assert!(reqs.is_empty());
    }

    #[test]
    fn find_cell_indices_grid() {
        let body = vec![json!({
            "startIndex": 5,
            "table": {
                "tableRows": [
                    { "tableCells": [
                        { "content": [{ "startIndex": 7 }] },
                        { "content": [{ "startIndex": 12 }] },
                    ] },
                ]
            }
        })];
        let grid = find_table_cell_indices(&body, 5);
        assert_eq!(grid, vec![vec![7, 12]]);
        // Wrong index => empty.
        assert!(find_table_cell_indices(&body, 99).is_empty());
    }
}
