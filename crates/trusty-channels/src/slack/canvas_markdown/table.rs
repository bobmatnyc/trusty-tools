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
/// standard GFM pipe table with every cell run through [`escape_cell`] so a
/// literal `|` or `\` inside a cell can never be misread as a column
/// delimiter on reparse (the code-critic-flagged bug this fixes — see
/// `pipe_and_backslash_in_cell_round_trip_through_reparse`).
/// Test: `renders_simple_table`, `table_over_cap_is_a_hard_error`,
/// `pipe_and_backslash_in_cell_round_trip_through_reparse`.
pub(super) fn render_table<'a>(
    node: &'a AstNode<'a>,
    _table: &NodeTable,
    ctx: &mut RenderCtx,
) -> Result<String, TranslationError> {
    let rows: Vec<Vec<String>> = node
        .children()
        .map(|row| {
            row.children()
                .map(|cell| escape_cell(inline::render_inlines(cell, ctx).trim()))
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

/// Escape a rendered cell's one remaining unprotected Markdown table
/// delimiter: `|`.
///
/// Why: a bare `|` inside a cell splits it into two cells on reparse — the
/// exact "silent truncation" [`render_table`]'s own doc promises never to
/// do. `cell` here is the *already-rendered* markdown from
/// [`inline::render_inlines`], which has itself already run every `Text`
/// node through `inline::escape_text` — every real backslash character in
/// `cell` is therefore already part of a complete, self-contained escape
/// pair (`\\`, `` \` ``, `\*`, or `\_`; see `escape_text`'s doc). Escaping
/// backslash *again* here would double it (`\\` → `\\\\`), corrupting the
/// content on reparse instead of preserving it — caught by
/// `pipe_and_backslash_in_cell_round_trip_through_reparse` when this
/// function first escaped both characters. Inserting a fresh `\` directly
/// before an untouched `|` is always safe regardless of what precedes it,
/// because a preceding escape pair from `escape_text` is already complete
/// and never bleeds into the next character.
/// Test: `pipe_and_backslash_in_cell_round_trip_through_reparse`,
/// `escape_cell_escapes_only_the_pipe`.
fn escape_cell(cell: &str) -> String {
    cell.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use comrak::nodes::NodeValue;

    use super::super::test_support::{find_first, reparse};
    use super::super::to_canvas_markdown;
    use super::super::TranslationError;
    use super::{escape_cell, MAX_TABLE_CELLS};

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

    #[test]
    fn pipe_and_backslash_in_cell_round_trip_through_reparse() {
        // Source cell content is `a|b\c` (an escaped pipe and a literal
        // backslash), written in GFM table source as `a\|b\\c`.
        let input = "| col1 | col2 |\n| --- | --- |\n| a\\|b\\\\c | plain |\n";
        let out = to_canvas_markdown(input).expect("table should translate");
        assert!(out.warnings.is_empty(), "warnings: {:?}", out.warnings);

        // Substring checks alone would pass even if the cell were split in
        // two — the escaped delimiter must actually survive a reparse.
        let arena = comrak::Arena::new();
        let root = reparse(&arena, &out.markdown);
        let table = find_first(root, |v| matches!(v, NodeValue::Table(_)))
            .expect("reparsed output must contain a table");

        let rows: Vec<_> = table.children().collect();
        assert_eq!(rows.len(), 2, "header + one data row, not split further");
        let data_row_cells: Vec<_> = rows[1].children().collect();
        assert_eq!(
            data_row_cells.len(),
            2,
            "the escaped pipe must not split the cell into three"
        );

        // Read the reparsed cell's decoded text directly off the AST's
        // `Text` node(s) — NOT through `inline::render_inlines`, which
        // would re-apply `escape_text` and mask exactly the property this
        // test exists to check (the semantic content, post-decode).
        let mut cell_text = String::new();
        for child in data_row_cells[0].children() {
            if let NodeValue::Text(t) = &child.data.borrow().value {
                cell_text.push_str(t);
            }
        }
        assert_eq!(
            cell_text, "a|b\\c",
            "the pipe and backslash must survive as data"
        );
    }

    #[test]
    fn escape_cell_escapes_only_the_pipe() {
        // Backslash is already escaped upstream by `inline::escape_text`;
        // `escape_cell` must not touch it a second time (see its doc).
        assert_eq!(escape_cell("a|b"), "a\\|b");
        assert_eq!(
            escape_cell("a\\\\b"),
            "a\\\\b",
            "backslash passes through untouched"
        );
        assert_eq!(escape_cell("plain"), "plain");
        assert_eq!(escape_cell("a||b"), "a\\|\\|b");
    }
}
