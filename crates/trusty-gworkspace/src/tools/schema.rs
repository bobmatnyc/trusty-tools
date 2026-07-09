//! Shared JSON Schema builders for MCP tool definitions.
//!
//! Why: Every tool group repeats the same `account` string schema, the same
//! `action` enum shape, and the same `{name, description, inputSchema}` outer
//! envelope. Centralising the builders keeps each group module focused on the
//! per-tool argument shapes rather than boilerplate.
//! What: Small free functions returning `serde_json::Value` fragments used by
//! the per-service modules under `tools/`.
//! Test: Exercised transitively by `tools::tests` via `tool_list_response()`.

use serde_json::{Value, json};

/// Standard `account` string-schema fragment shared by every tool.
///
/// Why: The Google account profile selector is identical across all tools.
/// What: Returns the `{type, description}` schema for the `account` field.
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn account_schema() -> Value {
    json!({
        "type": "string",
        "description": "The Google account profile to use. Defaults to the default profile.",
    })
}

/// Build an `action` string-enum schema fragment.
///
/// Why: Most `manage_*` tools expose a fixed set of operations via an `action`
/// enum; this keeps that shape consistent.
/// What: Returns a string schema whose `enum` is the supplied action list.
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn action_enum(actions: &[&str]) -> Value {
    json!({
        "type": "string",
        "description": "Operation to perform.",
        "enum": actions,
    })
}

/// Assemble a single MCP tool definition.
///
/// Why: The `tools/list` response requires a uniform `{name, description,
/// inputSchema}` object per tool; centralising avoids drift.
/// What: Wraps the supplied `properties` and `required` list into an
/// object-typed input schema alongside the name and description.
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        }
    })
}
