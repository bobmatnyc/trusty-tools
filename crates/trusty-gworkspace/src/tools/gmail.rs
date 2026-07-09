//! Gmail tool definitions.
//!
//! Why: Groups Gmail message, attachment, label, filter, and settings tools.
//! What: Appends the Gmail tool group to the shared registry vector.
//! Test: Covered via `tool_list_response()` in `tools::tests`.

use super::schema::{account_schema, action_enum, tool};
use serde_json::{Value, json};

/// Append the Gmail tool group to the registry.
///
/// Why: Keeps Gmail-related tools colocated.
/// What: Pushes the Gmail search/read/compose/label/filter/settings tools.
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn append(tools: &mut Vec<Value>) {
    tools.push(tool(
        "search_gmail_messages",
        "Search Gmail messages using Gmail query syntax (e.g. 'from:foo subject:bar').",
        json!({
            "account": account_schema(),
            "query": { "type": "string" },
            "max_results": { "type": "integer" },
        }),
        &[],
    ));
    tools.push(tool(
        "get_gmail_message_content",
        "Fetch the full content of a Gmail message by ID.",
        json!({
            "account": account_schema(),
            "message_id": { "type": "string" },
        }),
        &["message_id"],
    ));
    tools.push(tool(
        "download_gmail_attachment",
        "Download a Gmail attachment by message + attachment ID, optionally writing to disk.",
        json!({
            "account": account_schema(),
            "message_id": { "type": "string" },
            "attachment_id": { "type": "string" },
            "save_path": { "type": "string", "description": "If set, decoded bytes are written here." },
            "return_content": { "type": "boolean", "description": "If true, returns the base64 body inline." },
        }),
        &["message_id", "attachment_id"],
    ));
    tools.push(tool(
        "list_message_attachments",
        "Enumerate attachments on a Gmail message.",
        json!({
            "account": account_schema(),
            "message_id": { "type": "string" },
        }),
        &["message_id"],
    ));
    tools.push(tool(
        "compose_email",
        "Send, draft, or send-an-existing-draft email via Gmail.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["send", "draft", "send_draft"]),
            "to": { "type": "string" },
            "cc": { "type": "string" },
            "bcc": { "type": "string" },
            "subject": { "type": "string" },
            "body": { "type": "string" },
            "html": { "type": "boolean" },
            "draft_id": { "type": "string", "description": "Required when action=send_draft." },
        }),
        &[],
    ));
    tools.push(tool(
        "modify_gmail_messages",
        "Batch-add or remove labels across a set of Gmail messages.",
        json!({
            "account": account_schema(),
            "message_ids": { "type": "array", "items": { "type": "string" } },
            "add_label_ids": { "type": "array", "items": { "type": "string" } },
            "remove_label_ids": { "type": "array", "items": { "type": "string" } },
        }),
        &["message_ids"],
    ));
    tools.push(tool(
        "format_email_content",
        "Convert markdown-flavoured text to a simple HTML body suitable for compose_email.",
        json!({
            "body": { "type": "string" },
            "mode": { "type": "string", "enum": ["auto", "passthrough"] },
        }),
        &["body"],
    ));
    tools.push(tool(
        "manage_gmail_labels",
        "CRUD Gmail labels.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["list", "create", "update", "delete"]),
            "label_id": { "type": "string" },
            "name": { "type": "string" },
            "label_list_visibility": { "type": "string" },
            "message_list_visibility": { "type": "string" },
            "updates": { "type": "object" },
        }),
        &["action"],
    ));
    tools.push(tool(
        "manage_gmail_filters",
        "List, create, or delete Gmail filters.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["list", "create", "delete"]),
            "filter": { "type": "object", "description": "Filter resource (create)." },
            "filter_id": { "type": "string" },
        }),
        &["action"],
    ));
    tools.push(tool(
        "manage_gmail_settings",
        "Get or update Gmail account settings (vacation, autoForwarding, imap, pop, language).",
        json!({
            "account": account_schema(),
            "setting": { "type": "string", "enum": ["vacation", "auto_forwarding", "imap", "pop", "language"] },
            "action": action_enum(&["get", "update"]),
            "value": { "type": "object" },
        }),
        &["setting"],
    ));
}
