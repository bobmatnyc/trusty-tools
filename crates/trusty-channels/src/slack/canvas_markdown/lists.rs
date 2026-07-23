//! List (bulleted, ordered, task-checklist) rendering for
//! [`super::to_canvas_markdown`].
//!
//! Why: lists are the one construct that needs both a per-item marker
//! (`-`, `1.`, `- [ ]`, `- [x]`) and indentation of any nested block content
//! (a sub-list, a second paragraph within one item) — distinct enough from
//! every other block to warrant its own module rather than inlining into
//! [`super::blocks`].
//! What: [`render_list`] assigns each item's marker (bullet vs sequential
//! ordinal, per [`comrak::nodes::NodeList::list_type`]) and detects a task
//! item's checkbox state independent of the list's own `is_task_list` flag
//! (Slack canvas markdown checklists round-trip as `- [ ]`/`- [x]`, exactly
//! CommonMark's own task-list syntax, so no translation beyond re-emission is
//! needed); [`render_item`] renders one item's block content and indents
//! every line after the first by two spaces so nested structure survives.
//! Test: unit tests below cover bulleted, ordered, task-list, and nested-list
//! rendering; multi-paragraph items are covered in `super::tests`.

use comrak::nodes::{AstNode, ListType, NodeList, NodeValue};

use super::blocks::{render_blocks, RenderCtx};
use super::TranslationError;

/// Render a `List` node: dispatch each item to its marker, in document
/// order.
///
/// Why: the marker depends on both the list's type (bullet vs ordered) and,
/// independent of that, whether the *item itself* is a task item — CommonMark
/// represents a task-list item as `NodeValue::TaskItem` in place of
/// `NodeValue::Item`, so it is read per-item rather than assumed uniform from
/// the list's `is_task_list` flag.
/// What: for an ordered list, the ordinal starts at `list.start` and
/// increments only for plain `Item` children (a task item is always rendered
/// with a bullet, matching GFM's task-list grammar). Ends with a trailing
/// blank line so the list is separated from whatever follows.
/// Test: `renders_bulleted_list`, `renders_ordered_list_from_custom_start`,
/// `renders_task_list_checkboxes`.
pub(super) fn render_list<'a>(
    node: &'a AstNode<'a>,
    list: &NodeList,
    ctx: &mut RenderCtx,
) -> Result<String, TranslationError> {
    let mut out = String::new();
    let mut ordinal = list.start;
    for item in node.children() {
        let item_value = item.data.borrow().value.clone();
        let (marker, checkbox) = match item_value {
            NodeValue::TaskItem(task) => {
                let box_str = if task.symbol.is_some() { "[x]" } else { "[ ]" };
                ("-".to_string(), Some(box_str))
            }
            _ => {
                let marker = match list.list_type {
                    ListType::Bullet => "-".to_string(),
                    ListType::Ordered => {
                        let m = format!("{ordinal}.");
                        ordinal += 1;
                        m
                    }
                };
                (marker, None)
            }
        };
        out.push_str(&render_item(item, &marker, checkbox, ctx)?);
    }
    out.push('\n');
    Ok(out)
}

/// Render one list item: its first line carries `marker` (and `checkbox`
/// when present); every subsequent line — including a nested sub-list's own
/// markers — is indented two spaces so the structure survives re-parsing.
fn render_item<'a>(
    node: &'a AstNode<'a>,
    marker: &str,
    checkbox: Option<&str>,
    ctx: &mut RenderCtx,
) -> Result<String, TranslationError> {
    let content = render_blocks(node, ctx)?;
    let content = content.trim_end();
    let prefix = match checkbox {
        Some(box_str) => format!("{marker} {box_str} "),
        None => format!("{marker} "),
    };

    let mut lines = content.lines();
    let mut out = String::new();
    match lines.next() {
        Some(first) => {
            out.push_str(&prefix);
            out.push_str(first);
        }
        None => out.push_str(prefix.trim_end()),
    }
    out.push('\n');
    for line in lines {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::to_canvas_markdown;

    #[test]
    fn renders_bulleted_list() {
        let out = to_canvas_markdown("- one\n- two\n").unwrap();
        assert!(out.markdown.contains("- one"));
        assert!(out.markdown.contains("- two"));
    }

    #[test]
    fn renders_ordered_list_from_custom_start() {
        let out = to_canvas_markdown("5. five\n6. six\n").unwrap();
        assert!(out.markdown.contains("5. five"));
        assert!(out.markdown.contains("6. six"));
    }

    #[test]
    fn renders_task_list_checkboxes() {
        let out = to_canvas_markdown("- [ ] todo\n- [x] done\n").unwrap();
        assert!(out.markdown.contains("- [ ] todo"));
        assert!(out.markdown.contains("- [x] done"));
        assert!(out.warnings.is_empty(), "warnings: {:?}", out.warnings);
    }

    #[test]
    fn renders_nested_list_indented() {
        let out = to_canvas_markdown("- parent\n  - child\n").unwrap();
        assert!(out.markdown.contains("- parent"));
        assert!(out.markdown.contains("  - child"));
    }

    #[test]
    fn mixed_task_and_plain_items_in_one_list() {
        // A task-list item's marker never advances an ordered counter or
        // switches to numeric — GFM task lists are always bullet lists.
        let out = to_canvas_markdown("- [ ] todo\n- plain\n").unwrap();
        assert!(out.markdown.contains("- [ ] todo"));
        assert!(out.markdown.contains("- plain"));
    }
}
