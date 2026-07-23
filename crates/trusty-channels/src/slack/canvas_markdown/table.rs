//! Table rendering + Slack's 300-cell-per-table hard cap for
//! [`super::to_canvas_markdown`].
//!
//! Why: Slack canvas markdown accepts GFM-style pipe tables, but documents a
//! hard 300-cell (`rows * columns`, header included) limit per table — a
//! table over that cap cannot be represented at all, so this is the one
//! construct in the translator that can fail the whole conversion rather
//! than degrade with a warning (see the module doc on
//! [`super::to_canvas_markdown`]).
//! What: [`render_table`] walks the table's row/cell children directly
//! (rather than trusting [`comrak::nodes::NodeTable::num_rows`]'s exact
//! semantics, which the AST doesn't document precisely enough to rely on) to
//! get an unambiguous cell count, checks it against
//! [`super::MAX_TABLE_CELLS`] before emitting anything, and renders a
//! standard GFM pipe table (header row, a `---` delimiter row, then the
//! remaining rows).
//! Test: `renders_simple_table`, `table_over_cap_is_a_hard_error` (also
//! covered end-to-end in `super::tests`).

use comrak::nodes::{AstNode, NodeTable};

use super::blocks::RenderCtx;
use super::{TranslationError, MAX_TABLE_CELLS};
use crate::slack::canvas_markdown::inline;

/// Render a `Table` node, or refuse it outright when it exceeds Slack's
/// 300-cell cap.
///
/// Why: a silently truncated table drops caller data invisibly; refusing the
/// whole translation up front is the only choice that never surprises the
/// caller (see the module doc on [`super::to_canvas_markdown`]).
/// What: counts actual row/cell children (not `table.num_rows`, kept as an
/// unused-but-documented parameter for callers matching by struct shape);
/// returns [`TranslationError::TableTooLarge`] before rendering a single
/// character when `rows * columns > `[`MAX_TABLE_CELLS`]. Otherwise emits a
/// standard GFM pipe table.
/// Test: `renders_simple_table`, `table_over_cap_is_a_hard_error`.
pub(super) fn render_table<'a>(
    node: &'a AstNode<'a>,
    _table: &NodeTable,
    ctx: &mut RenderCtx,
) -> Result<String, TranslationError> {
    let rows: Vec<Vec<String>> = node
        .children()
        .map(|row| {
            row.children()
                .map(|cell| inline::render_inlines(cell, ctx).trim().to_string())
                .collect()
        })
        .collect();

    let row_count = rows.len();
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let cells = row_count * columns;
    if cells > MAX_TABLE_CELLS {
        return Err(TranslationError::TableTooLarge {
            cells,
            rows: row_count,
            columns,
            cap: MAX_TABLE_CELLS,
        });
    }

    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        out.push_str("| ");
        out.push_str(&row.join(" | "));
        out.push_str(" |\n");
        if i == 0 {
            out.push_str("| ");
            out.push_str(&vec!["---"; columns].join(" | "));
            out.push_str(" |\n");
        }
    }
    out.push('\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::to_canvas_markdown;
    use super::super::TranslationError;
    use super::MAX_TABLE_CELLS;

    #[test]
    fn renders_simple_table() {
        let out = to_canvas_markdown("| a | b |\n| --- | --- |\n| 1 | 2 |\n").unwrap();
        assert!(out.markdown.contains("| a | b |"));
        assert!(out.markdown.contains("| --- | --- |"));
        assert!(out.markdown.contains("| 1 | 2 |"));
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn table_over_cap_is_a_hard_error() {
        let mut input = String::from("| a | b |\n| --- | --- |\n");
        for i in 0..151 {
            input.push_str(&format!("| r{i}c0 | r{i}c1 |\n"));
        }
        let err = to_canvas_markdown(&input).expect_err("152 rows x 2 cols = 304 > 300");
        let TranslationError::TableTooLarge {
            cells,
            rows,
            columns,
            cap,
        } = err;
        assert_eq!(rows, 152);
        assert_eq!(columns, 2);
        assert_eq!(cells, 304);
        assert_eq!(cap, MAX_TABLE_CELLS);
    }

    #[test]
    fn table_exactly_at_cap_succeeds() {
        // 150 rows x 2 columns = 300 cells, exactly at the cap.
        let mut input = String::from("| a | b |\n| --- | --- |\n");
        for i in 0..149 {
            input.push_str(&format!("| r{i}c0 | r{i}c1 |\n"));
        }
        let out = to_canvas_markdown(&input).expect("exactly-at-cap table must succeed");
        assert!(out.markdown.contains("| a | b |"));
    }
}
