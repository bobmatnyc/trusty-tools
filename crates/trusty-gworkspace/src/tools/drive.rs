//! Google Drive tool definitions.
//!
//! Why: Groups Drive listing, search, content, and permission tools.
//! What: Appends the Drive tool group to the shared registry vector.
//! Test: Covered via `tool_list_response()` in `tools::tests`.

use super::schema::{account_schema, action_enum, tool};
use serde_json::{Value, json};

/// Append the Drive tool group to the registry.
///
/// Why: Keeps Drive-related tools colocated.
/// What: Pushes the Drive list/search/content/permission tools.
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn append(tools: &mut Vec<Value>) {
    tools.push(tool(
        "list_drive_contents",
        "List the contents of a Drive folder (defaults to root).",
        json!({
            "account": account_schema(),
            "folder_id": { "type": "string" },
            "max_results": { "type": "integer" },
        }),
        &[],
    ));
    tools.push(tool(
        "search_drive_files",
        "Search Drive using v3 query syntax.",
        json!({
            "account": account_schema(),
            "query": { "type": "string" },
            "max_results": { "type": "integer" },
        }),
        &["query"],
    ));
    tools.push(tool(
        "get_drive_file_content",
        "Fetch a Drive file's content (auto-exports Google native docs). Text is returned inline; binary files are base64-encoded, or written to disk when save_path is set.",
        json!({
            "account": account_schema(),
            "file_id": { "type": "string" },
            "export_mime_type": { "type": "string", "description": "Override export MIME for Google native files." },
            "save_path": { "type": "string", "description": "If set, the raw bytes are written to this local path (required for large binaries)." },
            "output_format": {
                "type": "string",
                "enum": ["auto", "raw"],
                "description": "auto: text inline / binary base64 (default). raw: always treat as bytes (base64 or save_path).",
            },
        }),
        &["file_id"],
    ));
    tools.push(tool(
        "list_shared_drives",
        "List shared drives the account has access to.",
        json!({ "account": account_schema() }),
        &[],
    ));
    tools.push(tool(
        "manage_drive_file",
        "Create folders, rename/move/copy/trash/delete, or upload files in Drive.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["create_folder", "rename", "trash", "delete", "copy", "move", "upload"]),
            "file_id": { "type": "string" },
            "name": { "type": "string", "description": "New/target file or folder name (required for create_folder, rename, upload)." },
            "parent_id": { "type": "string", "description": "Parent folder id (target for move/upload, optional for create_folder)." },
            "local_path": { "type": "string", "description": "upload: read bytes from this local file." },
            "content": { "type": "string", "description": "upload: inline text content (alternative to local_path)." },
            "mime_type": { "type": "string", "description": "upload: MIME type of the content (guessed from local_path extension, else text/plain)." },
        }),
        &["action"],
    ));
    tools.push(tool(
        "manage_file_permissions",
        "List, create, update, or delete sharing permissions on a Drive file.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["list", "create", "update", "delete"]),
            "file_id": { "type": "string" },
            "permission_id": { "type": "string" },
            "role": { "type": "string", "description": "reader|commenter|writer|owner|organizer" },
            "type": { "type": "string", "description": "user|group|domain|anyone" },
            "email_address": { "type": "string" },
            "domain": { "type": "string" },
            "send_notification_email": { "type": "string", "enum": ["true", "false"] },
        }),
        &["action", "file_id"],
    ));
}
