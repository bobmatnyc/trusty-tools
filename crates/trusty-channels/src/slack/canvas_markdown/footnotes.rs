//! Footnote flattening for [`super::to_canvas_markdown`].
//!
//! Why: Slack canvas markdown has no footnote syntax at all — CommonMark's
//! `[^name]` reference / `[^name]: text` definition pair has nowhere to go.
//! Dropping footnotes outright would silently lose caller content; this
//! module instead "flattens" them: each reference becomes an inline `[n]`
//! marker (assigned by [`super::blocks::RenderCtx::footnote_number`] as
//! references are encountered) and every referenced definition is collected
//! up front, then re-emitted as a trailing ordered list once the body is
//! done.
//! What: [`collect_definitions`] pre-scans the document's direct children —
//! CommonMark requires a footnote definition to be a top-level block — and
//! renders each one's content into [`RenderCtx::footnote_defs`], keyed by
//! name, *before* the body walk starts (a definition may appear before or
//! after its first reference in the source, so numbering can't be assigned
//! here). [`render_footnotes_section`] runs after the body walk, once every
//! reference has been numbered, and emits a `---` divider, a bold
//! "Footnotes" label, and one ordered-list line per referenced footnote in
//! first-reference order.
//! Test: `collects_definition_content`, `section_is_empty_when_unreferenced`,
//! `super::tests::flattens_footnotes`.

use comrak::nodes::{AstNode, NodeValue};

use super::blocks::{render_blocks, RenderCtx};
use super::TranslationError;

/// Pre-scan `root`'s direct children for footnote definitions and render
/// each one's content into `ctx.footnote_defs`.
///
/// Why: must run before the body walk so a reference to a footnote defined
/// later in the document still resolves.
/// What: only direct children of the document root are considered — per
/// CommonMark's footnote extension grammar, a definition is always a
/// top-level block, never nested inside a list item or blockquote.
pub(super) fn collect_definitions<'a>(
    root: &'a AstNode<'a>,
    ctx: &mut RenderCtx,
) -> Result<(), TranslationError> {
    for child in root.children() {
        let value = child.data.borrow().value.clone();
        if let NodeValue::FootnoteDefinition(def) = value {
            let content = render_blocks(child, ctx)?;
            ctx.footnote_defs
                .insert(def.name, content.trim().to_string());
        }
    }
    Ok(())
}

/// Build the trailing "Footnotes" section from every footnote referenced
/// during the body walk, in first-reference order.
///
/// Why: called once, after the body is fully rendered, so every reference
/// has already been numbered via [`RenderCtx::footnote_number`].
/// What: returns an empty string when no footnote was referenced (the common
/// case); otherwise a `---` divider, a bold label, and one `N. <content>`
/// line per referenced footnote. A referenced-but-undefined footnote (a
/// dangling `[^name]` with no matching `[^name]: ...`) renders with empty
/// content rather than panicking — the `[n]` marker in the body still makes
/// the gap visible.
pub(super) fn render_footnotes_section(ctx: &RenderCtx) -> String {
    if ctx.footnote_order.is_empty() {
        return String::new();
    }
    let mut out = String::from("---\n\n**Footnotes**\n\n");
    for name in &ctx.footnote_order {
        let n = ctx.footnote_numbers.get(name).copied().unwrap_or(0);
        let content = ctx
            .footnote_defs
            .get(name)
            .map(String::as_str)
            .unwrap_or("");
        out.push_str(&format!("{n}. {content}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::to_canvas_markdown;

    #[test]
    fn collects_definition_content() {
        let out = to_canvas_markdown("See[^x].\n\n[^x]: The definition text.\n").unwrap();
        assert!(out.markdown.contains("See[1]."));
        assert!(out.markdown.contains("1. The definition text."));
    }

    #[test]
    fn section_is_empty_when_unreferenced() {
        // A document with no footnote reference at all must not grow a
        // Footnotes section from an unrelated body.
        let out = to_canvas_markdown("plain paragraph, no footnotes\n").unwrap();
        assert!(!out.markdown.contains("Footnotes"));
    }

    #[test]
    fn repeated_reference_to_same_footnote_reuses_number() {
        let out =
            to_canvas_markdown("first[^x] and second[^x].\n\n[^x]: shared definition.\n").unwrap();
        assert!(out.markdown.contains("first[1]"));
        assert!(out.markdown.contains("second[1]"));
        // Exactly one Footnotes entry, not two.
        assert_eq!(out.markdown.matches("shared definition.").count(), 1);
    }
}
