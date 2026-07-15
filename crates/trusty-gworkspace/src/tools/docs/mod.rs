//! Google Docs tool definitions.
//!
//! Why: Groups the document create/read/edit/format/table tools.
//! What: Appends the Docs tool group to the shared registry vector.
//! Test: Covered via `tool_list_response()` in `tools::tests`.

mod extra;

use super::schema::{account_schema, action_enum, tool};
use serde_json::{Value, json};

/// Append the Docs tool group to the registry.
///
/// Why: Keeps Docs-related tools colocated.
/// What: Pushes the document create/edit/format/table tools, plus the extended
/// tabs/lists/images/tables/templates parity tools (`extra` submodule).
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn append(tools: &mut Vec<Value>) {
    tools.push(tool(
        "create_document",
        "Create a new empty Google Doc with the given title.",
        json!({ "account": account_schema(), "title": { "type": "string" } }),
        &[],
    ));
    tools.push(tool(
        "append_to_document",
        "Append text to the end of a Google Doc.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "text": { "type": "string" },
        }),
        &["document_id", "text"],
    ));
    tools.push(tool(
        "get_document",
        "Fetch the full Google Doc JSON.",
        json!({ "account": account_schema(), "document_id": { "type": "string" } }),
        &["document_id"],
    ));
    tools.push(tool(
        "get_document_structure",
        "Return the structural outline of a Google Doc (headings, paragraphs, tables) without inline runs.",
        json!({ "account": account_schema(), "document_id": { "type": "string" } }),
        &["document_id"],
    ));
    tools.push(tool(
        "replace_text_in_document",
        "Replace every occurrence of `find` with `replace` in a Google Doc.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "find": { "type": "string" },
            "replace": { "type": "string" },
        }),
        &["document_id", "find", "replace"],
    ));
    tools.push(tool(
        "insert_text_in_document",
        "Insert text at a specific index in a Google Doc.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "text": { "type": "string" },
            "index": { "type": "integer" },
        }),
        &["document_id", "text"],
    ));
    tools.push(tool(
        "delete_range_in_document",
        "Delete a content range from a Google Doc.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "start_index": { "type": "integer" },
            "end_index": { "type": "integer" },
        }),
        &["document_id", "start_index", "end_index"],
    ));
    tools.push(tool(
        "manage_document_comments",
        "List/create/reply/resolve/delete comments on a Google Doc.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["list", "create", "reply", "resolve", "delete"]),
            "document_id": { "type": "string" },
            "comment_id": { "type": "string" },
            "content": { "type": "string" },
        }),
        &["action", "document_id"],
    ));
    tools.push(tool(
        "format_document_range",
        "Apply bold/italic/underline/font size/named style to a range in a Google Doc.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "start_index": { "type": "integer" },
            "end_index": { "type": "integer" },
            "bold": { "type": "boolean" },
            "italic": { "type": "boolean" },
            "underline": { "type": "boolean" },
            "font_size": { "type": "number" },
            "named_style": { "type": "string", "description": "e.g. HEADING_1, NORMAL_TEXT" },
        }),
        &["document_id", "start_index", "end_index"],
    ));
    tools.push(tool(
        "set_document_style",
        "Update document-level style properties (page size, margins, etc.).",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "style": { "type": "object" },
            "fields": { "type": "string", "description": "Field mask, defaults to '*'." },
        }),
        &["document_id"],
    ));
    tools.push(tool(
        "insert_table_in_document",
        "Insert a table at the given index in a Google Doc.",
        json!({
            "account": account_schema(),
            "document_id": { "type": "string" },
            "rows": { "type": "integer" },
            "columns": { "type": "integer" },
            "index": { "type": "integer" },
        }),
        &["document_id"],
    ));
    tools.push(tool(
        "find_tables_in_document",
        "Enumerate tables in a Google Doc.",
        json!({ "account": account_schema(), "document_id": { "type": "string" } }),
        &["document_id"],
    ));
    tools.push(tool(
        "manage_table_structure",
        "Insert or delete a row or column in a Google Doc table.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["insert_row", "insert_column", "delete_row", "delete_column"]),
            "document_id": { "type": "string" },
            "table_start_index": { "type": "integer" },
            "row": { "type": "integer" },
            "column": { "type": "integer" },
            "below": { "type": "boolean" },
            "right": { "type": "boolean" },
        }),
        &["action", "document_id", "table_start_index"],
    ));

    extra::append(tools);
}
