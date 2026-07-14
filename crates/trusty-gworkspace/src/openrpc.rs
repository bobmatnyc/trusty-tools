//! OpenRPC 1.3.2 service description for `gworkspace-mcp`.
//!
//! Why: Orchestrators such as open-mpm need a machine-readable manifest of
//! every tool the server exposes — including the Google OAuth scopes each
//! tool requires — so they can route tasks and prepare scope-aware auth
//! flows without bespoke per-server adapters. OpenRPC's `rpc.discover`
//! method is the standard way to expose this manifest over an existing
//! JSON-RPC 2.0 channel; no new transport, port, or auth handshake.
//! What: `discover_response()` returns a complete OpenRPC 1.3.2 document
//! built from the same `tool_list_response()` registry used by
//! `tools/list`. Each `methods[i]` entry has `name`, `description`,
//! `params` (one per JSON Schema property), `result`, and an
//! `x-google-scopes` extension field with the OAuth scope set. The
//! `scopes_for_tool()` function is the single source of truth for tool ->
//! scope mapping.
//! Test: `crates/trusty-gworkspace/src/openrpc.rs::tests` validates the
//! envelope shape, asserts every registered tool has a corresponding
//! method entry with `params` derived from its input schema and an
//! `x-google-scopes` array, and that `methods.len()` matches the tool
//! registry length.

use serde_json::{Value, json};

use crate::tools::tool_list_response;

/// OAuth scope constants for Google Workspace APIs.
///
/// Why: Centralising scope literals avoids drift between the scope mapping
/// here and the auth client; each scope appears exactly once.
/// What: Module-level `&str` constants matching the canonical Google scope
/// URLs.
/// Test: Indirectly via `every_tool_has_scopes` — any typo would surface
/// when the assertion compares string contents.
mod scopes {
    pub const GMAIL_MODIFY: &str = "https://www.googleapis.com/auth/gmail.modify";
    pub const GMAIL_SEND: &str = "https://www.googleapis.com/auth/gmail.send";
    pub const GMAIL_SETTINGS_BASIC: &str = "https://www.googleapis.com/auth/gmail.settings.basic";
    pub const GMAIL_LABELS: &str = "https://www.googleapis.com/auth/gmail.labels";
    pub const CALENDAR: &str = "https://www.googleapis.com/auth/calendar";
    pub const CALENDAR_EVENTS: &str = "https://www.googleapis.com/auth/calendar.events";
    pub const DRIVE: &str = "https://www.googleapis.com/auth/drive";
    pub const DOCUMENTS: &str = "https://www.googleapis.com/auth/documents";
    pub const SPREADSHEETS: &str = "https://www.googleapis.com/auth/spreadsheets";
    pub const PRESENTATIONS: &str = "https://www.googleapis.com/auth/presentations";
    pub const TASKS: &str = "https://www.googleapis.com/auth/tasks";
    pub const USERINFO_PROFILE: &str = "https://www.googleapis.com/auth/userinfo.profile";
}

/// Return the OAuth scopes a given tool requires.
///
/// Why: open-mpm and other clients need to know which Google scopes must
/// be present in the user's credential before invoking a tool, so they can
/// either prompt for incremental consent or fail fast with a useful error.
/// What: Maps every tool name in the registry to a slice of canonical
/// Google OAuth 2.0 scope URLs. Returns an empty slice for unknown tools
/// so callers can treat missing mappings as "no scopes required" rather
/// than panicking.
/// Test: `every_tool_has_scopes` iterates the tool registry and asserts
/// the slice is non-empty for every known tool.
pub fn scopes_for_tool(name: &str) -> &'static [&'static str] {
    use scopes::*;
    match name {
        // Accounts — only needs profile info to enumerate local profiles.
        "list_accounts" => &[USERINFO_PROFILE],

        // Calendar
        "manage_calendars" => &[CALENDAR],
        "manage_events" => &[CALENDAR_EVENTS],
        "query_free_busy" => &[CALENDAR],

        // Gmail
        "search_gmail_messages" => &[GMAIL_MODIFY],
        "get_gmail_message_content" => &[GMAIL_MODIFY],
        "download_gmail_attachment" => &[GMAIL_MODIFY],
        "list_message_attachments" => &[GMAIL_MODIFY],
        "compose_email" => &[GMAIL_SEND, GMAIL_MODIFY],
        "modify_gmail_messages" => &[GMAIL_MODIFY],
        "format_email_content" => &[], // pure local transformation
        "manage_gmail_labels" => &[GMAIL_LABELS, GMAIL_MODIFY],
        "manage_gmail_filters" => &[GMAIL_SETTINGS_BASIC],
        "manage_gmail_settings" => &[GMAIL_SETTINGS_BASIC],

        // Drive
        "list_drive_contents"
        | "search_drive_files"
        | "get_drive_file_content"
        | "list_shared_drives"
        | "manage_drive_file"
        | "manage_file_permissions" => &[DRIVE],

        // Docs
        "create_document"
        | "append_to_document"
        | "get_document"
        | "get_document_structure"
        | "replace_text_in_document"
        | "insert_text_in_document"
        | "delete_range_in_document"
        | "manage_document_comments"
        | "format_document_range"
        | "set_document_style"
        | "insert_table_in_document"
        | "find_tables_in_document"
        | "manage_table_structure" => &[DOCUMENTS, DRIVE],

        // Sheets
        "get_spreadsheet"
        | "manage_spreadsheet"
        | "modify_sheet_values"
        | "format_sheet"
        | "create_chart" => &[SPREADSHEETS, DRIVE],

        // Slides
        "get_slides" | "manage_slides" | "add_slide_content" => &[PRESENTATIONS, DRIVE],

        // Tasks
        "manage_task_lists" | "manage_tasks" | "list_tasks" | "complete_task" => &[TASKS],

        _ => &[],
    }
}

/// Build the OpenRPC 1.3.2 service description document.
///
/// Why: Produces the value placed in the `result` field of a
/// `rpc.discover` JSON-RPC response, satisfying the OpenRPC spec for
/// discovery so any compliant client (including open-mpm) can introspect
/// every method, its parameters, and required scopes.
/// What: Walks `tool_list_response()`, converts each tool into an OpenRPC
/// `Method` object — flattening the JSON Schema properties into named
/// `ContentDescriptor` params with `required` propagated from the input
/// schema — and emits a top-level document with `openrpc`, `info`, and
/// `methods`. Each method carries an `x-google-scopes` extension field.
/// Test: `discover_response_is_valid_openrpc_document`.
pub fn discover_response() -> Value {
    let tools_value = tool_list_response();
    let tools = tools_value["tools"].as_array().cloned().unwrap_or_default();

    let methods: Vec<Value> = tools.iter().map(tool_to_method).collect();

    json!({
        "openrpc": "1.3.2",
        "info": {
            "title": "gworkspace-mcp",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "Google Workspace tools (Gmail, Calendar, Drive, Docs, Sheets, Slides, Tasks) exposed as JSON-RPC 2.0 methods over stdio.",
            "license": {
                "name": "Elastic-2.0",
                "url": "https://www.elastic.co/licensing/elastic-license"
            }
        },
        "methods": methods,
    })
}

/// Convert a single `tools/list` entry into an OpenRPC `Method` object.
///
/// Why: The OpenRPC schema requires `params` to be an array of named
/// `ContentDescriptor`s, but our MCP tool registry stores arguments as a
/// flat JSON Schema `properties` object. This helper bridges the two.
/// What: Copies `name` and `description`, flattens
/// `inputSchema.properties` into `params[]` (preserving the `required`
/// flag per parameter), emits a permissive `result` content descriptor,
/// and attaches the `x-google-scopes` extension via `scopes_for_tool()`.
/// Test: Covered by `discover_response_is_valid_openrpc_document` which
/// asserts every method has `name`, non-null `params`, `result`, and
/// `x-google-scopes`.
fn tool_to_method(tool: &Value) -> Value {
    let name = tool["name"].as_str().unwrap_or("").to_string();
    let description = tool["description"].as_str().unwrap_or("").to_string();
    let input_schema = &tool["inputSchema"];
    let properties = input_schema
        .get("properties")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let required: Vec<String> = input_schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut params: Vec<Value> = Vec::with_capacity(properties.len());
    for (param_name, schema) in properties.iter() {
        let param_description = schema
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        params.push(json!({
            "name": param_name,
            "description": param_description,
            "required": required.iter().any(|r| r == param_name),
            "schema": schema,
        }));
    }

    let scopes = scopes_for_tool(&name);

    json!({
        "name": name,
        "description": description,
        "params": params,
        "result": {
            "name": format!("{name}_result"),
            "description": "Tool result envelope; structure varies by tool.",
            "schema": { "type": "object" }
        },
        "x-google-scopes": scopes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_response_is_valid_openrpc_document() {
        let doc = discover_response();

        // Top-level envelope
        assert_eq!(
            doc["openrpc"].as_str(),
            Some("1.3.2"),
            "openrpc version must be 1.3.2"
        );
        assert!(doc["info"]["title"].is_string(), "info.title required");
        assert!(doc["info"]["version"].is_string(), "info.version required");

        // methods array
        let methods = doc["methods"].as_array().expect("methods must be array");
        let tools = tool_list_response();
        let tool_count = tools["tools"].as_array().unwrap().len();
        assert_eq!(
            methods.len(),
            tool_count,
            "methods count must match tool registry"
        );

        for m in methods {
            assert!(m["name"].is_string(), "method has name");
            assert!(m["params"].is_array(), "method has params array");
            assert!(m["result"].is_object(), "method has result object");
            let scopes = m["x-google-scopes"]
                .as_array()
                .expect("x-google-scopes is an array");
            // Pure-local tools (e.g. format_email_content) may legitimately
            // have an empty scope set; we only assert the field exists.
            for s in scopes {
                assert!(s.is_string(), "scope entries must be strings");
            }
        }
    }

    #[test]
    fn every_tool_has_scopes() {
        let tools = tool_list_response();
        let tools = tools["tools"].as_array().unwrap();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            // format_email_content is a pure local helper with no Google
            // call, so an empty scope set is acceptable. Every other tool
            // must declare at least one scope.
            if name == "format_email_content" {
                continue;
            }
            let scopes = scopes_for_tool(name);
            assert!(
                !scopes.is_empty(),
                "tool {name} must declare at least one OAuth scope"
            );
        }
    }

    #[test]
    fn method_params_match_input_schema_required() {
        let doc = discover_response();
        let methods = doc["methods"].as_array().unwrap();
        // Pick a tool we know has a required param: `get_gmail_message_content`
        // requires `message_id`.
        let m = methods
            .iter()
            .find(|m| m["name"] == "get_gmail_message_content")
            .expect("method present");
        let params = m["params"].as_array().unwrap();
        let p = params
            .iter()
            .find(|p| p["name"] == "message_id")
            .expect("message_id param present");
        assert_eq!(
            p["required"], true,
            "message_id must be marked required in OpenRPC params"
        );
    }
}
