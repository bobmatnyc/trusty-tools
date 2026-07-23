//! CommonMark → Slack canvas-markdown translator (issue #3744 slice 2).
//!
//! Why: `slack_canvas_push` (in [`super::handlers::canvas`]) accepts CommonMark
//! from the caller, but Slack's `canvases.edit`/`canvases.create` document
//! content is a narrower markdown dialect — no h4-h6, no HTML, no footnotes,
//! and a hard 300-cell table cap. Feeding raw CommonMark straight through
//! would silently produce a canvas that doesn't render the way the caller
//! expects. Centralising the conversion in one pure, unit-testable module lets
//! the handler stay a thin caller and every construct gets exhaustive coverage
//! independent of any live Slack call.
//! What: [`to_canvas_markdown`] parses `input` as CommonMark (via `comrak`,
//! with the `table`, `strikethrough`, `tasklist`, and `footnotes` extensions
//! enabled — the four non-core constructs this dialect needs) and walks the
//! resulting AST, re-emitting Slack canvas markdown. Constructs Slack supports
//! natively (bold, italic, strikethrough, inline code, fenced code blocks,
//! h1-h3, bulleted/ordered lists, dividers, quote blocks, task-list
//! checkboxes, links) round-trip losslessly. Constructs it does not support
//! are downgraded or dropped with a warning recorded on
//! [`TranslationResult::warnings`] — see [`blocks`] and [`footnotes`] for the
//! specific rules (h4+ → h3, HTML blocks/inline stripped, footnotes flattened
//! to inline `[n]` markers plus a trailing "Footnotes" section, non-mention
//! images dropped). Slack's own mention syntax (`![](@U…)`, `![](#C…)`) is
//! recognised and passed through byte-for-byte — this module never invents a
//! `@name` → `@U…` mapping layer, since that requires a live user/channel
//! lookup the translator doesn't have. A table whose cell count exceeds
//! [`MAX_TABLE_CELLS`] is a hard [`TranslationError`], never a silent
//! truncation, since a truncated table silently drops caller data.
//! Test: unit tests below cover full-document round trips for every
//! supported/downgraded construct; [`blocks`], [`inline`], [`lists`],
//! [`table`], and [`footnotes`] each carry their own construct-level tests.

mod blocks;
mod footnotes;
mod inline;
mod lists;
mod table;

use comrak::nodes::AstNode;
use comrak::{parse_document, Arena, Options};

/// Slack's documented hard cap on cells (rows × columns) in a single canvas
/// table. Exceeding it is a hard translation error (see the module doc) —
/// never a silent truncation.
pub const MAX_TABLE_CELLS: usize = 300;

/// The result of a successful CommonMark → Slack canvas-markdown translation.
///
/// Why: lossy downgrades (heading clamping, HTML stripping, footnote
/// flattening, non-mention image drops) must stay visible to the caller
/// rather than disappearing silently — `slack_canvas_push` surfaces
/// `warnings` back through the tool response.
/// What: `markdown` is the translated document; `warnings` is empty when the
/// translation was fully lossless.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TranslationResult {
    /// The translated Slack canvas markdown.
    pub markdown: String,
    /// Human-readable notes on every lossy downgrade applied, in document
    /// order.
    pub warnings: Vec<String>,
}

/// A translation failure severe enough to refuse the conversion outright.
///
/// Why: unlike a downgrade (recorded as a warning, translation still
/// succeeds), a table over Slack's hard cell cap cannot be represented at
/// all without dropping caller data — refusing beats truncating silently.
/// What: currently the sole variant is the 300-cell table cap; kept as an
/// enum (rather than a bare error string) so a future hard constraint has
/// somewhere to go without a breaking signature change.
/// Test: `table::table_over_cap_is_a_hard_error`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TranslationError {
    /// A table's `rows * columns` cell count exceeds [`MAX_TABLE_CELLS`].
    #[error(
        "table has {cells} cells ({rows} rows x {columns} columns), exceeding Slack canvas's \
         {cap}-cell-per-table limit; split it into multiple smaller tables"
    )]
    TableTooLarge {
        /// Total cell count (`rows * columns`).
        cells: usize,
        /// Row count, including the header row.
        rows: usize,
        /// Column count.
        columns: usize,
        /// The enforced cap ([`MAX_TABLE_CELLS`]).
        cap: usize,
    },
}

/// Translate a CommonMark document into Slack canvas markdown.
///
/// Why: the single entry point [`super::handlers::canvas::canvas_push`] calls
/// before ever touching the network — pure and synchronous, so it is fully
/// unit-testable without a mock Slack server.
/// What: parses `input` with `table`/`strikethrough`/`tasklist`/`footnotes`
/// extensions enabled, walks the resulting AST via [`blocks::render_document`],
/// and returns the rendered markdown plus any downgrade warnings. Returns
/// [`TranslationError`] only for the hard table-cap case; every other
/// unsupported construct downgrades with a warning instead of failing.
/// Test: `translates_every_supported_construct`, `downgrades_h4_to_h3`,
/// `strips_html_with_warning`, `flattens_footnotes`,
/// `passes_through_slack_mention_images`, `drops_non_mention_images`,
/// `table_over_cap_is_a_hard_error`.
pub fn to_canvas_markdown(input: &str) -> Result<TranslationResult, TranslationError> {
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    let root: &AstNode = parse_document(&arena, input, &options);

    let mut ctx = blocks::RenderCtx::default();
    let markdown = blocks::render_document(root, &mut ctx)?;
    Ok(TranslationResult {
        markdown,
        warnings: ctx.warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translate(input: &str) -> TranslationResult {
        to_canvas_markdown(input).expect("translation should succeed")
    }

    #[test]
    fn translates_every_supported_construct() {
        let input = "\
# Title

## Subtitle

### Sub-subtitle

**bold** and _italic_ and ~strike~ and `code` and [a link](https://example.com)

- one
- two
  - nested

1. first
2. second

- [ ] todo
- [x] done

> a quote

---

```rust
fn main() {}
```
";
        let out = translate(input);
        assert!(out.warnings.is_empty(), "warnings: {:?}", out.warnings);
        assert!(out.markdown.contains("# Title"));
        assert!(out.markdown.contains("## Subtitle"));
        assert!(out.markdown.contains("### Sub-subtitle"));
        assert!(out.markdown.contains("**bold**"));
        assert!(out.markdown.contains("_italic_"));
        assert!(out.markdown.contains("~strike~"));
        assert!(out.markdown.contains("`code`"));
        assert!(out.markdown.contains("[a link](https://example.com)"));
        assert!(out.markdown.contains("- one"));
        assert!(out.markdown.contains("  - nested"));
        assert!(out.markdown.contains("1. first"));
        assert!(out.markdown.contains("2. second"));
        assert!(out.markdown.contains("- [ ] todo"));
        assert!(out.markdown.contains("- [x] done"));
        assert!(out.markdown.contains("> a quote"));
        assert!(out.markdown.contains("---"));
        assert!(out.markdown.contains("```rust"));
        assert!(out.markdown.contains("fn main() {}"));
    }

    #[test]
    fn downgrades_h4_to_h3() {
        let out = translate("#### Deep heading\n");
        assert_eq!(out.markdown.trim(), "### Deep heading");
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("h4"), "{}", out.warnings[0]);
    }

    #[test]
    fn downgrades_h6_to_h3() {
        let out = translate("###### Deepest heading\n");
        assert_eq!(out.markdown.trim(), "### Deepest heading");
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("h6"), "{}", out.warnings[0]);
    }

    #[test]
    fn strips_html_with_warning() {
        let out = translate("<div>raw html</div>\n\nplain paragraph\n");
        assert!(!out.markdown.contains("<div>"));
        assert!(out.markdown.contains("plain paragraph"));
        assert!(out.warnings.iter().any(|w| w.contains("HTML")));
    }

    #[test]
    fn strips_inline_html_with_warning() {
        let out = translate("text with <b>inline html</b> in it\n");
        assert!(!out.markdown.contains("<b>"));
        assert!(out.warnings.iter().any(|w| w.contains("HTML")));
    }

    #[test]
    fn flattens_footnotes() {
        let out = translate("A claim[^1].\n\n[^1]: The supporting detail.\n");
        assert!(out.markdown.contains("A claim[1]."));
        assert!(out.markdown.contains("Footnotes"));
        assert!(out.markdown.contains("The supporting detail."));
        assert!(out.warnings.iter().any(|w| w.contains("footnote")));
    }

    #[test]
    fn passes_through_slack_mention_images() {
        let out = translate("Hello ![](@U0123ABCD) in ![](#C0123ABCD)\n");
        assert!(out.markdown.contains("![](@U0123ABCD)"));
        assert!(out.markdown.contains("![](#C0123ABCD)"));
        assert!(out.warnings.is_empty(), "warnings: {:?}", out.warnings);
    }

    #[test]
    fn drops_non_mention_images_with_warning() {
        let out = translate("![a photo](https://example.com/pic.png)\n");
        assert!(!out.markdown.contains("pic.png"));
        assert!(out.warnings.iter().any(|w| w.contains("image")));
    }

    #[test]
    fn table_over_cap_is_a_hard_error() {
        let mut input = String::from("| a | b |\n| --- | --- |\n");
        for i in 0..151 {
            input.push_str(&format!("| r{i}c0 | r{i}c1 |\n"));
        }
        let err = to_canvas_markdown(&input).expect_err("over-cap table must be refused");
        let TranslationError::TableTooLarge { cells, cap, .. } = err;
        assert!(cells > cap);
        assert_eq!(cap, MAX_TABLE_CELLS);
    }

    #[test]
    fn empty_input_translates_to_empty_output() {
        let out = translate("");
        assert_eq!(out.markdown.trim(), "");
        assert!(out.warnings.is_empty());
    }
}
