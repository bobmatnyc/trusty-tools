//! Inline-node rendering for [`super::to_canvas_markdown`].
//!
//! Why: inline content (the children of a paragraph, heading, table cell, or
//! list item) needs its own recursive walk distinct from
//! [`super::blocks`]'s block-level one — inlines nest arbitrarily (bold inside
//! a link, code inside emphasis, …) and never introduce a new line by
//! themselves.
//! What: [`render_inlines`] walks a node's children and concatenates each
//! child's rendering; [`render_inline`] dispatches a single inline
//! `NodeValue`. Text content is lightly escaped (backslash, `*`, `_`,
//! backtick) so re-emitted text can't be misread as markup it never was.
//! Slack's own mention syntax (`![](@U…)`, `![](#C…)`) passes through
//! byte-for-byte; any other image is unsupported by canvas markdown and is
//! dropped with a warning rather than guessed at.
//! Test: unit tests below cover each inline construct; mention passthrough
//! and image-drop are also covered end-to-end in `super::tests`.

use comrak::nodes::{AstNode, NodeValue};

use super::blocks::RenderCtx;
use super::{longest_backtick_run, truncate_for_warning};

/// Render every child of `node` as inline content, concatenated in order.
///
/// Why: shared by every block that holds inline children (paragraph, heading,
/// table cell, list item text run).
/// What: dispatches each child through [`render_inline`]; never introduces a
/// block-level newline.
/// Test: exercised via `super::tests::translates_every_supported_construct`.
pub(super) fn render_inlines<'a>(node: &'a AstNode<'a>, ctx: &mut RenderCtx) -> String {
    let mut out = String::new();
    for child in node.children() {
        render_inline(child, ctx, &mut out);
    }
    out
}

/// Render a single inline node into `out`.
///
/// Why: one dispatch point keeps every inline construct's Slack-markdown
/// encoding in one place, matching [`super::blocks::render_block`]'s
/// block-level dispatch shape.
/// What: emphasis/strong/strikethrough/code map to their Slack-markdown
/// delimiters; links keep CommonMark's `[text](url)` shape unchanged (Slack
/// canvas accepts it as-is); a footnote reference resolves through
/// [`RenderCtx::footnote_number`] to an inline `[n]` marker; an image either
/// passes through (Slack mention syntax) or is dropped with a warning; HTML
/// inline is stripped with a warning; any construct with no clean mapping
/// falls back to rendering its children (containers) or is silently skipped
/// (leaves) — this only matters for extensions this translator never enables
/// (see the module doc on [`super::to_canvas_markdown`]), so it is a defensive
/// default, not an expected path.
/// Test: `emphasis_and_strong_render`, `strikethrough_renders_single_tilde`,
/// `inline_code_renders_backticks`, `links_pass_through_unchanged`,
/// `escapes_markup_characters_in_text`.
pub(super) fn render_inline<'a>(node: &'a AstNode<'a>, ctx: &mut RenderCtx, out: &mut String) {
    let value = node.data.borrow().value.clone();
    match value {
        NodeValue::Text(text) => out.push_str(&escape_text(&text)),
        NodeValue::Code(code) => out.push_str(&code_span(&code.literal)),
        NodeValue::Emph => wrap(node, ctx, out, "_", "_"),
        NodeValue::Strong => wrap(node, ctx, out, "**", "**"),
        NodeValue::Strikethrough => wrap(node, ctx, out, "~", "~"),
        NodeValue::SoftBreak => out.push('\n'),
        NodeValue::LineBreak => out.push('\n'),
        NodeValue::Link(link) => {
            out.push('[');
            out.push_str(&render_inlines(node, ctx));
            out.push_str("](");
            out.push_str(&link.url);
            out.push(')');
        }
        NodeValue::Image(link) => render_image(&link.url, out, ctx),
        NodeValue::HtmlInline(_) => {
            ctx.warn("inline HTML stripped (Slack canvas markdown does not support raw HTML)");
        }
        NodeValue::FootnoteReference(fnref) => {
            let n = ctx.footnote_number(&fnref.name);
            out.push('[');
            out.push_str(&n.to_string());
            out.push(']');
        }
        // Defensive default: any inline construct not explicitly listed above
        // requires a comrak extension this translator never enables (see the
        // module doc), so it should be unreachable from real input — render
        // children (if any) rather than panic, so a future extension flip
        // degrades gracefully instead of crashing the tool call.
        _ => out.push_str(&render_inlines(node, ctx)),
    }
}

/// Render `node`'s children wrapped in `open`/`close` delimiters.
fn wrap<'a>(node: &'a AstNode<'a>, ctx: &mut RenderCtx, out: &mut String, open: &str, close: &str) {
    out.push_str(open);
    out.push_str(&render_inlines(node, ctx));
    out.push_str(close);
}

/// Render an `Image` node: Slack mention syntax passes through untouched;
/// anything else is dropped with a warning.
///
/// Why: `![](@U…)`/`![](#C…)` is Slack's own canvas markdown for a live
/// user/channel mention — a power user may hand-write it, and this translator
/// must never rewrite or invent a mapping for it (see the module doc). A
/// regular image has no canvas-markdown representation at all.
/// Test: `super::tests::passes_through_slack_mention_images`,
/// `super::tests::drops_non_mention_images_with_warning`.
fn render_image(url: &str, out: &mut String, ctx: &mut RenderCtx) {
    if is_slack_mention(url) {
        out.push_str("![](");
        out.push_str(url);
        out.push(')');
    } else {
        ctx.warn(format!(
            "image dropped (Slack canvas markdown has no embedded-image syntax): {}",
            truncate_for_warning(url)
        ));
    }
}

/// Render an inline code span with a delimiter guaranteed longer than any
/// backtick run already inside `literal`, applying CommonMark's single-space
/// padding rule when needed.
///
/// Why: a fixed single-backtick delimiter breaks as soon as `literal`
/// contains a backtick — the span reparses as a *shorter* code span plus
/// stray literal backticks, silently corrupting the content (the
/// code-critic-flagged bug this fixes). CommonMark's own rule for a code
/// span delimiter is "one backtick longer than the longest backtick run in
/// the content"; additionally, if the content starts or ends with a
/// backtick (or is empty), a single space is required on that side so the
/// delimiter isn't misread as extending into the content.
/// What: `fence` = `"`".repeat(longest_backtick_run(literal) + 1)` (minimum
/// one backtick when `literal` has none). Padding is added on both sides
/// together whenever `literal` is empty or starts/ends with a backtick —
/// matching how a CommonMark parser would need to see it to parse back to
/// the same content.
/// Test: `code_span_backtick_collision_round_trips_through_reparse`,
/// `code_span_padding_round_trips_through_reparse`,
/// `code_span_handles_empty_literal`.
fn code_span(literal: &str) -> String {
    let fence = "`".repeat(longest_backtick_run(literal) + 1);
    let needs_padding = literal.is_empty() || literal.starts_with('`') || literal.ends_with('`');
    if needs_padding {
        format!("{fence} {literal} {fence}")
    } else {
        format!("{fence}{literal}{fence}")
    }
}

/// Whether `url` looks like Slack's canvas mention syntax target: `@U…` (a
/// user id) or `#C…` (a channel id).
///
/// Why: shared by [`render_image`] and its tests; kept a plain prefix/shape
/// check (not a full Slack-id-format validator) since a malformed
/// caller-supplied mention should still pass through untouched rather than be
/// silently reinterpreted as a regular image.
pub(super) fn is_slack_mention(url: &str) -> bool {
    let Some(rest) = url.strip_prefix('@').or_else(|| url.strip_prefix('#')) else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Escape the four characters that would otherwise be re-read as markup:
/// backslash, `*`, `_`, and backtick.
///
/// Why: `comrak` decodes CommonMark backslash-escapes before handing text
/// nodes to us, so a literal `*` in the source arrives as plain `*` — without
/// re-escaping it here, re-emitting it verbatim would turn caller-authored
/// literal text into accidental emphasis/code markup in the translated
/// output.
/// Test: `escapes_markup_characters_in_text`.
fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '*' | '_' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use comrak::nodes::NodeValue;

    use super::super::test_support::{find_first, reparse};
    use super::super::to_canvas_markdown;
    use super::*;

    #[test]
    fn emphasis_and_strong_render() {
        let out = to_canvas_markdown("*em* and **strong**\n").unwrap();
        assert!(out.markdown.contains("_em_"));
        assert!(out.markdown.contains("**strong**"));
    }

    #[test]
    fn strikethrough_renders_single_tilde() {
        let out = to_canvas_markdown("~~gone~~\n").unwrap();
        assert!(out.markdown.contains("~gone~"));
        assert!(!out.markdown.contains("~~"));
    }

    #[test]
    fn inline_code_renders_backticks() {
        let out = to_canvas_markdown("`x = 1`\n").unwrap();
        assert!(out.markdown.contains("`x = 1`"));
    }

    #[test]
    fn links_pass_through_unchanged() {
        let out = to_canvas_markdown("[docs](https://example.com/a?b=1)\n").unwrap();
        assert!(out.markdown.contains("[docs](https://example.com/a?b=1)"));
    }

    #[test]
    fn escapes_markup_characters_in_text() {
        let out = to_canvas_markdown("literal \\*star\\* and \\`tick\\`\n").unwrap();
        assert!(out.markdown.contains("\\*star\\*"));
        assert!(out.markdown.contains("\\`tick\\`"));
    }

    #[test]
    fn is_slack_mention_matches_user_and_channel_ids_only() {
        assert!(is_slack_mention("@U0123ABCD"));
        assert!(is_slack_mention("#C0123ABCD"));
        assert!(!is_slack_mention("https://example.com/pic.png"));
        assert!(!is_slack_mention("@"));
        assert!(!is_slack_mention("#"));
        assert!(!is_slack_mention("@not-alnum!"));
    }

    #[test]
    fn code_span_handles_empty_literal() {
        // Delimiter sizing degrades gracefully to a single backtick with
        // padding when there is no content at all (never reachable from a
        // real CommonMark ` ` span, but must not panic or under-index).
        assert_eq!(code_span(""), "`  `");
    }

    #[test]
    fn code_span_sizes_delimiter_by_longest_run_plus_one() {
        assert_eq!(code_span("plain"), "`plain`");
        assert_eq!(code_span("a`b"), "``a`b``");
        assert_eq!(code_span("a``b"), "```a``b```");
    }

    #[test]
    fn code_span_pads_when_content_starts_or_ends_with_backtick() {
        assert_eq!(code_span("`x"), "`` `x ``");
        assert_eq!(code_span("x`"), "`` x` ``");
        assert_eq!(code_span("`x`"), "`` `x` ``");
    }

    #[test]
    fn code_span_backtick_collision_round_trips_through_reparse() {
        // Source: a 2-backtick-delimited span containing a single backtick
        // — valid CommonMark, and exactly the shape a naive single-backtick
        // re-emission would corrupt on reparse.
        let out = to_canvas_markdown("``a`b``\n").expect("should translate");
        assert!(out.warnings.is_empty(), "warnings: {:?}", out.warnings);

        let arena = comrak::Arena::new();
        let root = reparse(&arena, &out.markdown);
        let code = find_first(root, |v| matches!(v, NodeValue::Code(_)))
            .expect("reparsed output must contain a code span");
        let NodeValue::Code(code) = &code.data.borrow().value else {
            unreachable!()
        };
        assert_eq!(code.literal, "a`b", "the backtick must survive intact");
    }

    #[test]
    fn code_span_padding_round_trips_through_reparse() {
        // Source: content that both starts and ends with a backtick after
        // CommonMark's own leading/trailing-space-stripping rule resolves
        // (`` `x` `` with padding spaces around a 1-backtick-delimited
        // inner span).
        let out = to_canvas_markdown("`` `x` ``\n").expect("should translate");
        assert!(out.warnings.is_empty(), "warnings: {:?}", out.warnings);

        let arena = comrak::Arena::new();
        let root = reparse(&arena, &out.markdown);
        let code = find_first(root, |v| matches!(v, NodeValue::Code(_)))
            .expect("reparsed output must contain a code span");
        let NodeValue::Code(code) = &code.data.borrow().value else {
            unreachable!()
        };
        assert_eq!(
            code.literal, "`x`",
            "leading/trailing backticks must survive"
        );
    }
}
