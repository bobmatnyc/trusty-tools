//! Block-node rendering and shared translation state for
//! [`super::to_canvas_markdown`].
//!
//! Why: block-level constructs (headings, paragraphs, quotes, code blocks,
//! thematic breaks) need one dispatch point distinct from
//! [`super::inline`]'s, and every render function down the tree
//! ([`super::lists`], [`super::table`], [`super::footnotes`]) needs to share
//! one mutable [`RenderCtx`] — the running warnings list and the footnote
//! numbering table — without threading half a dozen separate parameters.
//! What: [`RenderCtx`] holds that shared state; [`render_document`] is the
//! top-level entry point ([`super::to_canvas_markdown`]'s only call into this
//! module) — it pre-collects footnote definitions, renders the body, then
//! appends the flattened footnotes section; [`render_blocks`] concatenates a
//! container's block children; [`render_block`] dispatches a single block
//! `NodeValue`.
//! Test: unit tests below cover heading downgrade, blockquote, code block,
//! and thematic-break rendering directly; the full pipeline (including list,
//! table, and footnote interplay) is covered in `super::tests`.

use std::collections::HashMap;

use comrak::nodes::{AstNode, NodeCodeBlock, NodeValue};

use super::{
    footnotes, inline, lists, longest_backtick_run, table, truncate_for_warning, TranslationError,
};

/// Mutable state threaded through every render call.
///
/// Why: warnings and footnote numbering are cross-cutting — a heading buried
/// three lists deep can still trigger a downgrade warning, and a footnote
/// reference must resolve to the same number everywhere it recurs. Passing a
/// single `&mut RenderCtx` down the recursive walk is simpler and cheaper
/// than returning warnings up through every call site and merging them.
/// What: `warnings` accumulates in document order via [`RenderCtx::warn`];
/// `footnote_defs` holds each definition's rendered body content, keyed by
/// name, populated by [`footnotes::collect_definitions`] before the body walk
/// starts; `footnote_numbers`/`footnote_order` assign sequential numbers to
/// footnotes in first-reference order via [`RenderCtx::footnote_number`].
/// Test: `footnote_number_assigns_sequentially_and_warns_once`.
#[derive(Default)]
pub(super) struct RenderCtx {
    pub warnings: Vec<String>,
    pub footnote_defs: HashMap<String, String>,
    pub footnote_order: Vec<String>,
    pub footnote_numbers: HashMap<String, usize>,
}

impl RenderCtx {
    /// Record a lossy-downgrade note.
    pub(super) fn warn(&mut self, msg: impl Into<String>) {
        self.warnings.push(msg.into());
    }

    /// Resolve `name` to its sequential footnote number, assigning one (and
    /// recording a warning) on first reference.
    ///
    /// Why: Slack canvas markdown has no footnote syntax at all, so every
    /// reference is flattened to a plain `[n]` marker; the number must stay
    /// stable across repeated references to the same footnote within one
    /// document.
    /// What: idempotent per `name` — a second call returns the same number
    /// without a second warning.
    /// Test: `footnote_number_assigns_sequentially_and_warns_once`.
    pub(super) fn footnote_number(&mut self, name: &str) -> usize {
        if let Some(&n) = self.footnote_numbers.get(name) {
            return n;
        }
        let n = self.footnote_numbers.len() + 1;
        self.footnote_numbers.insert(name.to_string(), n);
        self.footnote_order.push(name.to_string());
        self.warn(format!(
            "footnote '{}' flattened to an inline [{n}] marker plus a trailing \
             Footnotes section (Slack canvas markdown has no footnote syntax)",
            truncate_for_warning(name)
        ));
        n
    }
}

/// Translate a parsed document root into Slack canvas markdown.
///
/// Why: the sole entry point [`super::to_canvas_markdown`] calls — orders the
/// three passes footnote handling requires (collect definitions, render body,
/// append the flattened section) so a reference can resolve regardless of
/// whether its definition appears before or after it in the source.
/// What: returns the concatenated block-level rendering plus a trailing
/// footnotes section (empty when no footnote was referenced). Propagates
/// [`TranslationError`] from a table whose cell count exceeds the cap.
/// Test: `super::tests::translates_every_supported_construct`,
/// `super::tests::flattens_footnotes`.
pub(super) fn render_document<'a>(
    root: &'a AstNode<'a>,
    ctx: &mut RenderCtx,
) -> Result<String, TranslationError> {
    footnotes::collect_definitions(root, ctx)?;
    let mut body = render_blocks(root, ctx)?;
    body.push_str(&footnotes::render_footnotes_section(ctx));
    Ok(body)
}

/// Render every block child of `node`, concatenated in document order.
///
/// Why: shared by [`render_document`] (the document root) and every
/// container block (blockquote, list item) that holds block children.
pub(super) fn render_blocks<'a>(
    node: &'a AstNode<'a>,
    ctx: &mut RenderCtx,
) -> Result<String, TranslationError> {
    let mut out = String::new();
    for child in node.children() {
        out.push_str(&render_block(child, ctx)?);
    }
    Ok(out)
}

/// Dispatch a single block `NodeValue` to its Slack-canvas-markdown
/// rendering.
///
/// Why: one match arm per construct keeps the downgrade rules (h4+ → h3,
/// HTML stripped) greppable in one place, mirroring
/// [`inline::render_inline`]'s inline-level dispatch.
/// What: `FootnoteDefinition` renders to nothing here — its content was
/// already captured by [`footnotes::collect_definitions`] before this walk
/// started, and it is emitted once, at the end, by
/// [`footnotes::render_footnotes_section`]. Any block construct with no
/// explicit arm (only reachable via a comrak extension this translator never
/// enables — see the module doc on [`super::to_canvas_markdown`]) falls back
/// to rendering its children rather than panicking.
/// Test: `heading_h1_through_h3_pass_through`, `heading_h4_plus_downgrades`,
/// `blockquote_prefixes_every_line`, `code_block_preserves_info_string`,
/// `thematic_break_renders_divider`.
pub(super) fn render_block<'a>(
    node: &'a AstNode<'a>,
    ctx: &mut RenderCtx,
) -> Result<String, TranslationError> {
    let value = node.data.borrow().value.clone();
    match value {
        NodeValue::Document => render_blocks(node, ctx),
        NodeValue::FrontMatter(_) => Ok(String::new()),
        NodeValue::Paragraph => {
            let text = inline::render_inlines(node, ctx);
            Ok(format!("{text}\n\n"))
        }
        NodeValue::Heading(h) => {
            let mut level = h.level;
            if level > 3 {
                ctx.warn(format!(
                    "heading level h{level} downgraded to h3 (Slack canvas markdown \
                     supports only h1-h3)"
                ));
                level = 3;
            }
            let hashes = "#".repeat(level as usize);
            let text = inline::render_inlines(node, ctx);
            Ok(format!("{hashes} {text}\n\n"))
        }
        NodeValue::ThematicBreak => Ok("---\n\n".to_string()),
        NodeValue::BlockQuote => render_blockquote(node, ctx),
        NodeValue::CodeBlock(cb) => Ok(render_code_block(&cb)),
        NodeValue::HtmlBlock(_) => {
            ctx.warn("HTML block stripped (Slack canvas markdown does not support raw HTML)");
            Ok(String::new())
        }
        NodeValue::List(list) => lists::render_list(node, &list, ctx),
        NodeValue::Table(t) => table::render_table(node, &t, ctx),
        NodeValue::FootnoteDefinition(_) => Ok(String::new()),
        _ => render_blocks(node, ctx),
    }
}

/// Render a block quote: recursively render its block children, then prefix
/// every resulting line with `> ` (a bare `>` for blank lines, avoiding
/// trailing whitespace).
fn render_blockquote<'a>(
    node: &'a AstNode<'a>,
    ctx: &mut RenderCtx,
) -> Result<String, TranslationError> {
    let inner = render_blocks(node, ctx)?;
    let mut out = String::new();
    for line in inner.trim_end().lines() {
        if line.is_empty() {
            out.push('>');
        } else {
            out.push_str("> ");
            out.push_str(line);
        }
        out.push('\n');
    }
    out.push('\n');
    Ok(out)
}

/// Render a fenced/indented code block, preserving its info-string language
/// tag (Slack canvas markdown fences use the same triple-backtick syntax as
/// CommonMark).
///
/// Why: a hardcoded 3-backtick fence breaks as soon as the literal content
/// itself contains a run of 3+ consecutive backticks (e.g. a code block
/// documenting *how to write* a fenced code block) — the closing fence would
/// match early, truncating the block on reparse (the code-critic-flagged bug
/// this fixes).
/// What: the fence length is `max(3, longest_backtick_run(literal) + 1)`,
/// applied to both the opening and closing fence so they stay symmetric —
/// CommonMark requires the closing fence be at least as long as the opening
/// one, and using the same computed length for both is the simplest way to
/// guarantee that.
/// Test: `code_block_fence_widens_past_content_backtick_run_round_trips`.
fn render_code_block(cb: &NodeCodeBlock) -> String {
    let lang = cb.info.split_whitespace().next().unwrap_or("");
    let literal = if cb.literal.ends_with('\n') {
        cb.literal.clone()
    } else {
        format!("{}\n", cb.literal)
    };
    let fence_len = (longest_backtick_run(&literal) + 1).max(3);
    let fence = "`".repeat(fence_len);
    format!("{fence}{lang}\n{literal}{fence}\n\n")
}

#[cfg(test)]
mod tests {
    use comrak::nodes::NodeValue;

    use super::super::test_support::{find_first, reparse};
    use super::super::to_canvas_markdown;
    use super::*;

    #[test]
    fn heading_h1_through_h3_pass_through() {
        for (md, expected) in [("# a", "# a"), ("## a", "## a"), ("### a", "### a")] {
            let out = to_canvas_markdown(md).unwrap();
            assert_eq!(out.markdown.trim(), expected);
            assert!(out.warnings.is_empty());
        }
    }

    #[test]
    fn heading_h4_plus_downgrades() {
        for md in ["#### a", "##### a", "###### a"] {
            let out = to_canvas_markdown(md).unwrap();
            assert_eq!(out.markdown.trim(), "### a");
            assert_eq!(out.warnings.len(), 1);
        }
    }

    #[test]
    fn blockquote_prefixes_every_line() {
        let out = to_canvas_markdown("> line one\n>\n> line two\n").unwrap();
        assert!(out.markdown.contains("> line one"));
        assert!(out.markdown.contains("> line two"));
    }

    #[test]
    fn code_block_preserves_info_string() {
        let out = to_canvas_markdown("```python\nprint(1)\n```\n").unwrap();
        assert!(out.markdown.contains("```python"));
        assert!(out.markdown.contains("print(1)"));
    }

    #[test]
    fn thematic_break_renders_divider() {
        let out = to_canvas_markdown("a\n\n---\n\nb\n").unwrap();
        assert!(out.markdown.contains("---"));
    }

    #[test]
    fn footnote_number_assigns_sequentially_and_warns_once() {
        let mut ctx = RenderCtx::default();
        assert_eq!(ctx.footnote_number("a"), 1);
        assert_eq!(ctx.footnote_number("b"), 2);
        assert_eq!(ctx.footnote_number("a"), 1);
        assert_eq!(ctx.warnings.len(), 2, "only the first reference warns");
    }

    #[test]
    fn code_block_fence_widens_past_content_backtick_run_round_trips() {
        // Source uses a 4-backtick fence around content that itself
        // contains a 3-backtick run — a hardcoded 3-backtick output fence
        // would close early on reparse, truncating the block.
        let input = "````\nhas ``` three backticks inside\n````\n";
        let out = to_canvas_markdown(input).expect("should translate");
        assert!(out.warnings.is_empty(), "warnings: {:?}", out.warnings);
        assert!(
            out.markdown.contains("````"),
            "fence must widen to at least 4 backticks: {}",
            out.markdown
        );

        let arena = comrak::Arena::new();
        let root = reparse(&arena, &out.markdown);
        let code_block = find_first(root, |v| matches!(v, NodeValue::CodeBlock(_)))
            .expect("reparsed output must contain a code block");
        let NodeValue::CodeBlock(cb) = &code_block.data.borrow().value else {
            unreachable!()
        };
        assert_eq!(
            cb.literal, "has ``` three backticks inside\n",
            "the 3-backtick run must survive as block content, not truncate it"
        );
    }
}
