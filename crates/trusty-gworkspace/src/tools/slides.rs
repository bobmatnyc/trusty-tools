//! Google Slides tool definitions.
//!
//! Why: Groups presentation fetch/search, slide management, and typed content
//! authoring so the MCP tool schemas stay next to their siblings.
//! What: Appends the Slides tool group to the shared registry vector, mirroring
//! the action/type depth of the Python upstream.
//! Test: Covered via `tool_list_response()` in `tools::tests`.

use crate::api::services::slides::core::VALID_LAYOUTS;

use super::schema::{account_schema, action_enum, tool};
use serde_json::{Value, json};

/// Append the Slides tool group to the registry.
///
/// Why: Keeps Slides-related tools colocated.
/// What: Pushes the presentation get/manage and content tools with their full
/// action/type parameter sets.
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn append(tools: &mut Vec<Value>) {
    tools.push(tool(
        "get_slides",
        "Read Google Slides: list presentations, fetch a whole deck, a single \
         slide by index, or all text.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["list", "get_presentation", "get_slide", "get_text"]),
            "presentation_id": {
                "type": "string",
                "description": "Presentation id (required for all actions except `list`).",
            },
            "slide_index": {
                "type": "integer",
                "description": "Zero-based slide index for the `get_slide` action.",
            },
            "query": {
                "type": "string",
                "description": "Name substring to filter by for the `list` action.",
            },
            "max_results": {
                "type": "integer",
                "description": "Max presentations to return for `list` (default 20).",
            },
        }),
        &[],
    ));
    tools.push(tool(
        "manage_slides",
        "Create a presentation, create/delete a slide, or replace an element's text.",
        json!({
            "account": account_schema(),
            "action": action_enum(&[
                "create_presentation",
                "create_slide",
                "delete_slide",
                "update_text",
            ]),
            "presentation_id": { "type": "string" },
            "slide_id": { "type": "string" },
            "object_id": {
                "type": "string",
                "description": "Target element id for `update_text`.",
            },
            "title": { "type": "string" },
            "text": {
                "type": "string",
                "description": "Replacement text for `update_text`.",
            },
            "insertion_index": {
                "type": "integer",
                "description": "Zero-based position for a new slide (`create_slide`).",
            },
            "layout": {
                "type": "string",
                "description": "Predefined slide layout for `create_slide` (default BLANK).",
                "enum": VALID_LAYOUTS,
            },
        }),
        &["action"],
    ));
    tools.push(tool(
        "add_slide_content",
        "Add content to a slide: a plain or formatted text box, an image from a \
         URL, or a new bulleted-list slide.",
        json!({
            "account": account_schema(),
            "type": {
                "type": "string",
                "description": "Content kind to add (default text_box).",
                "enum": ["text_box", "formatted_text_box", "image", "bulleted_list"],
            },
            "presentation_id": { "type": "string" },
            "slide_id": {
                "type": "string",
                "description": "Target slide id (text_box/formatted_text_box/image).",
            },
            "text": {
                "type": "string",
                "description": "Text for a text box, or newline-delimited bullet items.",
            },
            "font_size": {
                "type": "number",
                "description": "Point size for `formatted_text_box`.",
            },
            "bold": { "type": "boolean" },
            "italic": { "type": "boolean" },
            "font_color": {
                "type": "string",
                "description": "Hex color `#RRGGBB` for `formatted_text_box`.",
            },
            "image_url": {
                "type": "string",
                "description": "Publicly fetchable image URL for `type=image`.",
            },
            "items": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Bullet lines for `type=bulleted_list`.",
            },
            "layout": {
                "type": "string",
                "description": "Layout for the new slide created by `bulleted_list` (default BLANK).",
                "enum": VALID_LAYOUTS,
            },
        }),
        &["presentation_id"],
    ));
}
