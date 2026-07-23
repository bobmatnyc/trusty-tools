//! JSON-Schema builders for the six `slack_*canvas*` MCP tools.
//!
//! Why: split out of [`super::tools`] (issue #3744 slice 2) — adding
//! `slack_canvas_push`'s schema pushed `tools.rs`'s single `vec![...]`
//! literal past the workspace's 500-SLOC production-file cap. Canvas tools
//! are also the one family with real internal cross-references (`slack_update_canvas`'s
//! description points readers at `slack_canvas_lookup_sections`), so grouping
//! them in one file — mirroring `handlers::canvas`'s own split from the rest
//! of `handlers` — keeps that context together rather than splitting
//! arbitrarily by line count.
//! What: [`canvas_tools`] returns the six canvas tool definitions in
//! `tool_list_response`'s exact `{name, description, inputSchema}` shape
//! (built through [`super::tools::tool`], the same private-to-`slack`
//! constructor `tools.rs` itself uses) as a `Vec<Value>` that
//! [`super::tools::tool_list_response`] appends to its own list. This file
//! has no `tool_list_response`/`is_known_tool` of its own — `tools.rs` stays
//! the single source of truth for the combined surface and its tests.
//! Test: `super::tools::canvas_split_tests::canvas_tools_are_present_after_the_file_split`
//! asserts all six names survive the split into the combined response; the
//! shared `tool_list_has_expected_count` / `every_tool_name_is_unique` /
//! `known_tools_match_registry` tests in `tools.rs` cover these schemas the
//! same as every other tool's.

use serde_json::{json, Value};

use super::tools::tool;

/// Build the six canvas tool schemas, in the same order they previously
/// appeared inline in `tools.rs`.
pub(super) fn canvas_tools() -> Vec<Value> {
    vec![
        tool(
            "slack_create_canvas",
            "Create a standalone canvas, optionally tabbed into a channel and/or \
             seeded with markdown content. Requires scope canvases:write.",
            json!({
                "title": { "type": "string", "description": "Optional canvas title." },
                "markdown": {
                    "type": "string",
                    "description": "Optional initial markdown content.",
                },
                "channel_id": {
                    "type": "string",
                    "description": "Optional channel ID to tab the new canvas into.",
                },
            }),
            &[],
        ),
        tool(
            "slack_update_canvas",
            "Replace a canvas's entire document content. Requires scope canvases:write.",
            json!({
                "canvas_id": { "type": "string", "description": "Canvas ID (e.g. F0123ABCD)." },
                "markdown": {
                    "type": "string",
                    "description": "New markdown content, replacing the canvas in full.",
                },
                "operation_id": {
                    "type": "string",
                    "description": "Optional idempotency key for the edit.",
                },
            }),
            &["canvas_id", "markdown"],
        ),
        tool(
            "slack_read_canvas",
            "Read a canvas's content. Slack has no documented full-content-read API, so \
             this downloads the canvas's private file export (HTML, not the original \
             markdown). Requires scopes canvases:read and files:read.",
            json!({
                "canvas_id": { "type": "string", "description": "Canvas ID (e.g. F0123ABCD)." },
            }),
            &["canvas_id"],
        ),
        tool(
            "slack_canvas_create",
            "Create a standalone canvas, optionally tabbed into a channel and/or \
             seeded with markdown content. Thin wrapper over canvases.create — no \
             markdown-to-canvas translation beyond Slack's own markdown ingestion. \
             `channel_id` is optional per Slack's API, but free-tier (non-Business+) \
             workspaces reject a non-tabbed canvas (error \
             free_teams_cannot_create_non_tabbed_canvases), so pass `channel_id` on \
             those teams. Requires scope canvases:write; other Slack errors surfaced \
             as-is include canvas_creation_failed, canvas_disabled_user_team, and \
             missing_scope.",
            json!({
                "title": { "type": "string", "description": "Optional canvas title." },
                "markdown": {
                    "type": "string",
                    "description": "Required initial markdown content, sent to Slack as \
                        document_content: {type: \"markdown\", markdown}.",
                },
                "channel_id": {
                    "type": "string",
                    "description": "Optional channel ID to tab the new canvas into. \
                        Effectively required on free-tier Slack teams — see the tool \
                        description.",
                },
            }),
            &["markdown"],
        ),
        tool(
            "slack_canvas_lookup_sections",
            "Look up section ids/anchors within an existing canvas via \
             canvases.sections.lookup, filtered by section type and/or contained \
             text. Slack has no full-canvas-content-read API — this returns only \
             section_ids, never document content or any other raw response field; \
             pair a returned id with slack_update_canvas / a section-targeted \
             canvases.edit call to act on it. Slack's criteria reliably accepts \
             only h1/h2/h3/any_header as section_types. Requires scope canvases:read.",
            json!({
                "canvas_id": { "type": "string", "description": "Canvas ID (e.g. F0123ABCD)." },
                "section_types": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional filter on section heading type. Slack \
                        reliably accepts only \"h1\", \"h2\", \"h3\", and \
                        \"any_header\" here.",
                },
                "contains_text": {
                    "type": "string",
                    "description": "Optional filter: only return sections whose \
                        content contains this text.",
                },
            }),
            &["canvas_id"],
        ),
        tool(
            "slack_canvas_push",
            "Translate CommonMark to Slack canvas markdown and push it onto an existing \
             canvas. mode=\"append\" issues a single insert_at_end edit. \
             mode=\"replace_all\" is NOT atomic: it looks up the canvas's existing \
             header-delimited (h1/h2/h3) sections, deletes each one with its own \
             sequential canvases.edit call, then inserts the new content — a failure \
             partway through leaves the canvas with some old sections removed and the \
             new content not yet inserted. A canvas with no header-delimited sections \
             (empty, or all-non-header content) cannot have its existing content \
             cleared this way; replace_all falls back to appending in that case and \
             says so in the response's warnings. A canvas_editing_locked response \
             (another client mid-edit) is retried a bounded number of times \
             automatically. Tables are capped at 300 cells (rows x columns); an \
             over-cap table is refused, never silently truncated. Slack's own mention \
             syntax (![](@U...), ![](#C...)) passes through untranslated if already \
             present in the input — this tool never invents a name-to-id mapping. \
             Requires scope canvases:write (canvases:read too for replace_all's lookup).",
            json!({
                "canvas_id": { "type": "string", "description": "Canvas ID (e.g. F0123ABCD)." },
                "markdown": {
                    "type": "string",
                    "description": "CommonMark content to translate and push. Supported: \
                        bold/italic/strikethrough/inline code/code blocks, h1-h3 (h4+ are \
                        downgraded to h3), bulleted/ordered lists, dividers, quote blocks, \
                        task-list checkboxes, tables (300-cell cap), links. HTML and \
                        footnotes are downgraded with a warning returned in the response.",
                },
                "mode": {
                    "type": "string",
                    "enum": ["replace_all", "append"],
                    "description": "\"append\" adds to the end of the canvas. \
                        \"replace_all\" clears existing header-delimited sections first \
                        (not atomic — see the tool description).",
                },
            }),
            &["canvas_id", "markdown", "mode"],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_exactly_the_six_canvas_tools() {
        let tools = canvas_tools();
        assert_eq!(tools.len(), 6);
        for t in &tools {
            assert!(t["name"].as_str().unwrap().contains("canvas"));
        }
    }
}
