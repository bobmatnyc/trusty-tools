//! Docs table cell styling and column widths.
//!
//! Why: Beyond structural row/column edits, agents need per-cell styling
//! (padding, borders, fill, vertical alignment) and explicit or auto-balanced
//! column widths.
//! What: `format_table_cells` (`updateTableCellStyle`) and
//! `set_table_column_widths` (`updateTableColumnProperties`), sharing the pure
//! cell-style builder and the `balance_column_widths` algorithm.
//! Test: The builders and the balancing algorithm are unit-tested below; the
//! round-trip is live-only.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::DOCS_API_BASE;
use crate::api::services::{account_of, require_str};

const DEFAULT_AVAILABLE_WIDTH: f64 = 468.0;
const DEFAULT_FONT_SIZE: f64 = 11.0;
const DEFAULT_MIN_COL_WIDTH: f64 = 60.0;

/// Why: Cell styling (padding/border/background/alignment) is shared by
/// `format_table_cells` and, conceptually, the table-preset pass.
/// What: Builds one `updateTableCellStyle` request for `(row, col)` populating
/// only the supplied properties and its field mask; returns `None` when nothing
/// was set.
/// Test: `cell_style_request_*` below.
pub(crate) fn build_cell_style_request(
    table_start_index: i64,
    row_idx: i64,
    col_idx: i64,
    padding: Option<&Value>,
    border: Option<&Value>,
    background_color: Option<&Value>,
    content_alignment: Option<&str>,
) -> Option<Value> {
    let mut cell_style = json!({});
    let mut fields = Vec::<String>::new();

    if let Some(pad) = padding {
        for side in ["top", "bottom", "left", "right"] {
            if let Some(mag) = pad.get(side).and_then(|v| v.as_f64()) {
                let key = format!("padding{}", capitalize(side));
                cell_style[&key] = json!({ "magnitude": mag, "unit": "PT" });
                fields.push(key);
            }
        }
    }

    if let Some(b) = border {
        let color = b
            .get("color")
            .cloned()
            .unwrap_or_else(|| json!({ "red": 0.0, "green": 0.0, "blue": 0.0 }));
        let width = b.get("width").and_then(|v| v.as_f64()).unwrap_or(1.0);
        let dash = b
            .get("dash_style")
            .and_then(|v| v.as_str())
            .unwrap_or("SOLID");
        let border_obj = json!({
            "color": { "color": { "rgbColor": color } },
            "width": { "magnitude": width, "unit": "PT" },
            "dashStyle": dash,
        });
        let default_sides = vec![json!("top"), json!("bottom"), json!("left"), json!("right")];
        let sides = b
            .get("sides")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or(default_sides);
        for side in sides {
            if let Some(s) = side.as_str() {
                let key = format!("border{}", capitalize(s));
                cell_style[&key] = border_obj.clone();
                fields.push(key);
            }
        }
    }

    if let Some(bg) = background_color {
        cell_style["backgroundColor"] = json!({ "color": { "rgbColor": bg } });
        fields.push("backgroundColor".to_string());
    }

    if let Some(align) = content_alignment {
        cell_style["contentAlignment"] = json!(align);
        fields.push("contentAlignment".to_string());
    }

    if fields.is_empty() {
        return None;
    }

    Some(json!({
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
    }))
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Why: One column-width request either fixes a width or reverts a column to
/// even distribution (null / non-positive width).
/// What: Builds an `updateTableColumnProperties` request for `col_idx`.
/// Test: `column_width_request_*` below.
pub(crate) fn build_column_width_request(
    table_start_index: i64,
    col_idx: i64,
    width: Option<f64>,
) -> Value {
    match width {
        Some(w) if w > 0.0 => json!({
            "updateTableColumnProperties": {
                "tableStartLocation": { "index": table_start_index },
                "columnIndices": [col_idx],
                "tableColumnProperties": {
                    "widthType": "FIXED_WIDTH",
                    "width": { "magnitude": w, "unit": "PT" },
                },
                "fields": "widthType,width",
            }
        }),
        _ => json!({
            "updateTableColumnProperties": {
                "tableStartLocation": { "index": table_start_index },
                "columnIndices": [col_idx],
                "tableColumnProperties": { "widthType": "EVENLY_DISTRIBUTED" },
                "fields": "widthType",
            }
        }),
    }
}

/// Why: Auto-balancing spreads available width so text-heavy columns get more
/// room; three algorithms trade off content-fit vs simplicity.
/// What: Returns per-column PT widths for `data` (row-major). `equalize` binary-
/// searches a target line-count; `sqrt`/`proportional` weight by char counts.
/// Widths are clamped to `min_col_width` and rescaled to fit `available_width`.
/// Test: `balance_widths_*` below.
pub(crate) fn balance_column_widths(
    data: &[Vec<String>],
    available_width: f64,
    font_size: f64,
    min_col_width: f64,
    algorithm: &str,
) -> Vec<f64> {
    if data.is_empty() || data[0].is_empty() {
        return Vec::new();
    }
    let num_cols = data.iter().map(|r| r.len()).max().unwrap_or(0);
    if num_cols == 0 {
        return Vec::new();
    }
    let char_width = font_size * 0.55;

    let mut max_chars = vec![0usize; num_cols];
    let mut max_word_len = vec![0usize; num_cols];
    for (c, mc) in max_chars.iter_mut().enumerate() {
        let mut col_max = 0usize;
        let mut col_word_max = 0usize;
        for row in data {
            if let Some(cell) = row.get(c) {
                col_max = col_max.max(cell.chars().count());
                if let Some(w) = cell.split_whitespace().map(|w| w.chars().count()).max() {
                    col_word_max = col_word_max.max(w);
                }
            }
        }
        *mc = col_max.max(1);
        max_word_len[c] = col_word_max.max(1);
    }

    let min_widths: Vec<f64> = max_word_len
        .iter()
        .map(|w| (*w as f64 * char_width).max(min_col_width))
        .collect();

    let mut widths: Vec<f64> = match algorithm {
        "equalize" => equalize_widths(&max_chars, &min_widths, char_width, available_width),
        "sqrt" => {
            let weights: Vec<f64> = max_chars
                .iter()
                .map(|mc| (*mc as f64).sqrt().max(1.0))
                .collect();
            let total: f64 = weights.iter().sum();
            weights
                .iter()
                .map(|w| (available_width * w / total).max(min_col_width))
                .collect()
        }
        _ => {
            let weights: Vec<f64> = max_chars.iter().map(|mc| (*mc as f64).max(1.0)).collect();
            let total: f64 = weights.iter().sum();
            weights
                .iter()
                .map(|w| (available_width * w / total).max(min_col_width))
                .collect()
        }
    };

    let total: f64 = widths.iter().sum();
    if total > available_width {
        let factor = available_width / total;
        for w in &mut widths {
            *w *= factor;
        }
    }
    widths
}

/// Equalize line counts via binary search on the target line count.
///
/// Why: Balancing by line count keeps rows visually even without a font metric
/// engine.
/// What: Finds the largest `T` such that `sum(ceil(chars/T)*char_width)` clamped
/// to `min_widths` fits `available_width`, then distributes leftover space.
/// Test: covered via `balance_widths_equalize` below.
pub(crate) fn equalize_widths(
    max_chars: &[usize],
    min_widths: &[f64],
    char_width: f64,
    available_width: f64,
) -> Vec<f64> {
    let num_cols = max_chars.len();
    let overall_max = *max_chars.iter().max().unwrap_or(&1);
    let mut best = vec![available_width / num_cols as f64; num_cols];

    let (mut lo, mut hi) = (1i64, overall_max as i64);
    while lo <= hi {
        let t = (lo + hi) / 2;
        let clamped: Vec<f64> = max_chars
            .iter()
            .zip(min_widths)
            .map(|(mc, mw)| {
                let lines = ((*mc as f64) / (t as f64)).ceil();
                (lines * char_width).max(*mw)
            })
            .collect();
        if clamped.iter().sum::<f64>() <= available_width {
            best = clamped;
            lo = t + 1;
        } else {
            hi = t - 1;
        }
    }

    let leftover = available_width - best.iter().sum::<f64>();
    if leftover > 0.5 {
        let mut benefiting: Vec<usize> = (0..num_cols)
            .filter(|&i| best[i] < max_chars[i] as f64 * char_width)
            .collect();
        if benefiting.is_empty() {
            benefiting = (0..num_cols).collect();
        }
        let share = leftover / benefiting.len() as f64;
        for i in benefiting {
            best[i] += share;
        }
    }
    best
}

fn f64_arg(args: &Value, key: &str, default: f64) -> f64 {
    args.get(key).and_then(|v| v.as_f64()).unwrap_or(default)
}

/// Why: Apply padding/border/background/alignment to a rectangular selection of
/// cells; `-1` targets all rows/columns.
/// What: Expands the target set and posts one `updateTableCellStyle` per cell.
/// Test: `build_cell_style_request` is unit-tested; the call is live-only.
pub async fn format_table_cells(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "document_id")?;
    let table_start_index = args
        .get("table_start_index")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing table_start_index"))?;
    let row_index = args
        .get("row_index")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing row_index"))?;
    let column_index = args
        .get("column_index")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing column_index"))?;
    let num_rows = args.get("num_rows").and_then(|v| v.as_i64()).unwrap_or(0);
    let num_columns = args
        .get("num_columns")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let padding = args.get("padding");
    let border = args.get("border");
    let background_color = args.get("background_color");
    let content_alignment = args.get("content_alignment").and_then(|v| v.as_str());

    let targets: Vec<(i64, i64)> = match (row_index, column_index) {
        (-1, -1) => (0..num_rows)
            .flat_map(|r| (0..num_columns).map(move |c| (r, c)))
            .collect(),
        (-1, c) => (0..num_rows).map(|r| (r, c)).collect(),
        (r, -1) => (0..num_columns).map(|c| (r, c)).collect(),
        (r, c) => vec![(r, c)],
    };

    let requests: Vec<Value> = targets
        .iter()
        .filter_map(|&(r, c)| {
            build_cell_style_request(
                table_start_index,
                r,
                c,
                padding,
                border,
                background_color,
                content_alignment,
            )
        })
        .collect();

    if requests.is_empty() {
        return Ok(json!({ "status": "no_formatting_applied", "document_id": id }));
    }
    let url = format!("{DOCS_API_BASE}/documents/{id}:batchUpdate");
    client
        .post(&url, json!({ "requests": requests }), account)
        .await?;
    Ok(json!({
        "status": "formatted",
        "document_id": id,
        "table_start_index": table_start_index,
        "cells_updated": requests.len(),
    }))
}

/// Why: Explicit or auto-balanced column widths control table layout.
/// What: Either balances from `data` or applies the supplied `column_widths`
/// (null = evenly distributed) as `updateTableColumnProperties` requests.
/// Test: `balance_column_widths` / `build_column_width_request` are unit-tested;
/// the call is live-only.
pub async fn set_table_column_widths(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "document_id")?;
    let table_start_index = args
        .get("table_start_index")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow!("missing table_start_index"))?;
    let auto_balance = args
        .get("auto_balance")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let algorithm = args
        .get("algorithm")
        .and_then(|v| v.as_str())
        .unwrap_or("equalize");

    // widths: each entry is Some(w) for a fixed width or None for evenly distributed.
    let widths: Vec<Option<f64>> = if auto_balance {
        let data = parse_2d_strings(args.get("data"));
        if data.is_empty() {
            return Ok(
                json!({ "status": "error", "message": "data is required when auto_balance=true" }),
            );
        }
        balance_column_widths(
            &data,
            f64_arg(&args, "available_width", DEFAULT_AVAILABLE_WIDTH),
            f64_arg(&args, "font_size", DEFAULT_FONT_SIZE),
            f64_arg(&args, "min_col_width", DEFAULT_MIN_COL_WIDTH),
            algorithm,
        )
        .into_iter()
        .map(Some)
        .collect()
    } else {
        args.get("column_widths")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().map(|v| v.as_f64()).collect())
            .unwrap_or_default()
    };

    if widths.is_empty() {
        return Ok(json!({ "status": "no_widths_applied", "document_id": id }));
    }
    let requests: Vec<Value> = widths
        .iter()
        .enumerate()
        .map(|(i, w)| build_column_width_request(table_start_index, i as i64, *w))
        .collect();
    let url = format!("{DOCS_API_BASE}/documents/{id}:batchUpdate");
    client
        .post(&url, json!({ "requests": requests }), account)
        .await?;
    Ok(json!({
        "status": "applied",
        "document_id": id,
        "table_start_index": table_start_index,
        "columns_updated": requests.len(),
        "auto_balance": auto_balance,
        "algorithm": if auto_balance { Value::from(algorithm) } else { Value::Null },
    }))
}

/// Parse a JSON 2-D array of strings into `Vec<Vec<String>>` (best-effort).
pub(crate) fn parse_2d_strings(value: Option<&Value>) -> Vec<Vec<String>> {
    value
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row.as_array()
                        .map(|cells| {
                            cells
                                .iter()
                                .map(|c| c.as_str().unwrap_or("").to_string())
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_style_request_padding_and_fields() {
        let padding = json!({ "top": 4, "left": 6 });
        let r = build_cell_style_request(10, 1, 2, Some(&padding), None, None, None).unwrap();
        let inner = &r["updateTableCellStyle"];
        let fields = inner["fields"].as_str().unwrap();
        assert!(fields.contains("paddingTop"));
        assert!(fields.contains("paddingLeft"));
        assert_eq!(inner["tableCellStyle"]["paddingTop"]["magnitude"], 4.0);
        assert_eq!(
            inner["tableRange"]["tableCellLocation"]["tableStartLocation"]["index"],
            10
        );
    }

    #[test]
    fn cell_style_request_border_default_sides() {
        let border = json!({ "color": { "red": 0.4 }, "width": 1.0 });
        let r = build_cell_style_request(0, 0, 0, None, Some(&border), None, None).unwrap();
        let fields = r["updateTableCellStyle"]["fields"].as_str().unwrap();
        for side in ["borderTop", "borderBottom", "borderLeft", "borderRight"] {
            assert!(fields.contains(side), "missing {side}");
        }
        assert_eq!(
            r["updateTableCellStyle"]["tableCellStyle"]["borderTop"]["dashStyle"],
            "SOLID"
        );
    }

    #[test]
    fn cell_style_request_border_explicit_sides() {
        let border = json!({ "sides": ["top"], "width": 2.0 });
        let r = build_cell_style_request(0, 0, 0, None, Some(&border), None, None).unwrap();
        let fields = r["updateTableCellStyle"]["fields"].as_str().unwrap();
        assert_eq!(fields, "borderTop");
    }

    #[test]
    fn cell_style_request_background_and_alignment() {
        let bg = json!({ "red": 0.9, "green": 0.9, "blue": 0.9 });
        let r = build_cell_style_request(0, 0, 0, None, None, Some(&bg), Some("MIDDLE")).unwrap();
        let fields = r["updateTableCellStyle"]["fields"].as_str().unwrap();
        assert!(fields.contains("backgroundColor"));
        assert!(fields.contains("contentAlignment"));
        assert_eq!(
            r["updateTableCellStyle"]["tableCellStyle"]["contentAlignment"],
            "MIDDLE"
        );
    }

    #[test]
    fn cell_style_request_none_when_empty() {
        assert!(build_cell_style_request(0, 0, 0, None, None, None, None).is_none());
    }

    #[test]
    fn column_width_request_fixed() {
        let r = build_column_width_request(5, 1, Some(120.0));
        let props = &r["updateTableColumnProperties"]["tableColumnProperties"];
        assert_eq!(props["widthType"], "FIXED_WIDTH");
        assert_eq!(props["width"]["magnitude"], 120.0);
        assert_eq!(r["updateTableColumnProperties"]["columnIndices"][0], 1);
    }

    #[test]
    fn column_width_request_evenly_distributed_for_null() {
        let r = build_column_width_request(5, 0, None);
        assert_eq!(
            r["updateTableColumnProperties"]["tableColumnProperties"]["widthType"],
            "EVENLY_DISTRIBUTED"
        );
        let r2 = build_column_width_request(5, 0, Some(0.0));
        assert_eq!(
            r2["updateTableColumnProperties"]["tableColumnProperties"]["widthType"],
            "EVENLY_DISTRIBUTED"
        );
    }

    #[test]
    fn balance_widths_equalize_sums_within_available() {
        let data = vec![
            vec![
                "short".to_string(),
                "a much longer cell of content".to_string(),
            ],
            vec![
                "x".to_string(),
                "another long body of text here".to_string(),
            ],
        ];
        let widths = balance_column_widths(&data, 468.0, 11.0, 60.0, "equalize");
        assert_eq!(widths.len(), 2);
        let total: f64 = widths.iter().sum();
        assert!(total <= 468.0 + 0.01, "total {total} exceeds available");
        assert!(widths.iter().all(|w| *w > 0.0));
    }

    #[test]
    fn balance_widths_proportional_wider_for_longer_column() {
        let data = vec![vec!["a".to_string(), "aaaaaaaaaaaaaaaaaaaa".to_string()]];
        let widths = balance_column_widths(&data, 468.0, 11.0, 20.0, "proportional");
        assert!(widths[1] > widths[0], "longer column should be wider");
    }

    #[test]
    fn balance_widths_empty_data() {
        assert!(balance_column_widths(&[], 468.0, 11.0, 60.0, "equalize").is_empty());
    }

    #[test]
    fn parse_2d_strings_reads_grid() {
        let v = json!([["a", "b"], ["c"]]);
        let grid = parse_2d_strings(Some(&v));
        assert_eq!(grid, vec![vec!["a", "b"], vec!["c"]]);
    }
}
