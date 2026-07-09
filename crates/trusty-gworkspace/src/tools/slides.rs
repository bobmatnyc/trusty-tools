//! Google Slides tool definitions.
//!
//! Why: Groups presentation fetch, slide management, and content tools.
//! What: Appends the Slides tool group to the shared registry vector.
//! Test: Covered via `tool_list_response()` in `tools::tests`.

use super::schema::{account_schema, action_enum, tool};
use serde_json::{Value, json};

/// Append the Slides tool group to the registry.
///
/// Why: Keeps Slides-related tools colocated.
/// What: Pushes the presentation get/manage and content tools.
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn append(tools: &mut Vec<Value>) {
    tools.push(tool(
        "get_slides",
        "Fetch a Google Slides presentation JSON.",
        json!({ "account": account_schema(), "presentation_id": { "type": "string" } }),
        &["presentation_id"],
    ));
    tools.push(tool(
        "manage_slides",
        "Create a presentation or create/delete slides within one.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["create_presentation", "create_slide", "delete_slide"]),
            "presentation_id": { "type": "string" },
            "slide_id": { "type": "string" },
            "title": { "type": "string" },
            "layout": { "type": "string" },
        }),
        &["action"],
    ));
    tools.push(tool(
        "add_slide_content",
        "Add a text box with the given text to a slide.",
        json!({
            "account": account_schema(),
            "presentation_id": { "type": "string" },
            "slide_id": { "type": "string" },
            "text": { "type": "string" },
        }),
        &["presentation_id", "slide_id", "text"],
    ));
}
