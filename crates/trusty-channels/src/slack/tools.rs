//! MCP `tools/list` response — JSON Schema for every planned Slack tool.
//!
//! Why: Claude Code (and any MCP client) needs a machine-readable contract
//! describing each tool's arguments so the model can fill them correctly. This
//! is the single source of truth for the Slack MCP surface; the dispatcher in
//! `server` routes on the same names. All nine tools now have live handlers
//! (issues #2639 + #2640, see ADR-0014).
//! What: `tool_list_response()` returns `{"tools": [{name, description,
//! inputSchema}, ...]}`; `is_known_tool()` reports whether a name is part of
//! the planned surface so the dispatcher can distinguish "not yet implemented"
//! from "unknown tool".
//! Test: Unit tests below assert the tool count, required-field shape, name
//! uniqueness, and `is_known_tool` agreement with the registry.

use serde_json::{json, Value};

/// The authoritative list of planned Slack tool names.
///
/// Why: Both `tool_list_response` (indirectly) and `is_known_tool` must agree
/// on the exact surface; a single constant array prevents drift.
/// What: The nine chat-as-tools operations planned for the native Slack MCP.
/// Test: `known_tools_match_registry` cross-checks this against the built list.
pub const TOOL_NAMES: &[&str] = &[
    "slack_send_message",
    "slack_read_channel",
    "slack_read_thread",
    "slack_list_channels",
    "slack_search_messages",
    "slack_search_channels",
    "slack_list_users",
    "slack_get_user",
    "slack_add_reaction",
];

/// Report whether `name` is a planned Slack tool.
///
/// Why: The dispatcher returns a distinct MCP error for an unknown tool vs a
/// known-but-unimplemented one; this predicate draws that line.
/// What: Linear membership check against `TOOL_NAMES`.
/// Test: `known_tools_match_registry`.
pub fn is_known_tool(name: &str) -> bool {
    TOOL_NAMES.contains(&name)
}

/// Assemble a single MCP tool definition.
///
/// Why: The `tools/list` response requires a uniform `{name, description,
/// inputSchema}` object per tool; centralising avoids drift.
/// What: Wraps `properties` and `required` into an object-typed input schema
/// alongside the name and description.
/// Test: Covered via `tool_list_response()` in `tests`.
fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
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

/// A `channel` id/name string-schema fragment shared by several tools.
fn channel_prop() -> Value {
    json!({
        "type": "string",
        "description": "Channel ID (e.g. C0123ABCD) or name (e.g. #general).",
    })
}

/// Build the full `tools/list` response.
///
/// Why: One function = one source of truth for the MCP contract.
/// What: Returns the nine planned Slack tools with minimal-but-valid schemas.
/// Test: `tool_list_has_expected_count` asserts exactly nine tools.
pub fn tool_list_response() -> Value {
    let tools = vec![
        tool(
            "slack_send_message",
            "Post a message to a Slack channel (or reply in a thread).",
            json!({
                "channel": channel_prop(),
                "text": { "type": "string", "description": "Message text to send." },
                "thread_ts": {
                    "type": "string",
                    "description": "Optional parent message ts to reply in-thread.",
                },
            }),
            &["channel", "text"],
        ),
        tool(
            "slack_read_channel",
            "Read recent messages from a Slack channel. Returns a single page \
             (no cursor pagination); a busy channel may truncate at `limit`.",
            json!({
                "channel": channel_prop(),
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of messages to return (default 50).",
                },
            }),
            &["channel"],
        ),
        tool(
            "slack_read_thread",
            "Read replies in a Slack thread. Returns a single page (no cursor \
             pagination); a very long thread may truncate.",
            json!({
                "channel": channel_prop(),
                "thread_ts": {
                    "type": "string",
                    "description": "The ts of the thread's parent message.",
                },
            }),
            &["channel", "thread_ts"],
        ),
        tool(
            "slack_list_channels",
            "List channels visible to the authenticated principal.",
            json!({
                "types": {
                    "type": "string",
                    "description": "Comma-separated channel types (e.g. public_channel,private_channel).",
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of channels to return.",
                },
            }),
            &[],
        ),
        tool(
            "slack_search_messages",
            "Search messages across the workspace. Requires a Slack user token \
             (SLACK_USER_TOKEN); a bot token cannot call search.",
            json!({
                "query": { "type": "string", "description": "Slack search query string." },
                "count": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default 20).",
                },
            }),
            &["query"],
        ),
        tool(
            "slack_search_channels",
            "Search channels by name or topic. Scans a single page of up to 200 \
             channels client-side (no cursor pagination); a large workspace may \
             truncate.",
            json!({
                "query": { "type": "string", "description": "Channel search query string." },
            }),
            &["query"],
        ),
        tool(
            "slack_list_users",
            "List users in the workspace.",
            json!({
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of users to return.",
                },
            }),
            &[],
        ),
        tool(
            "slack_get_user",
            "Fetch a single user's profile by ID.",
            json!({
                "user": { "type": "string", "description": "Slack user ID (e.g. U0123ABCD)." },
            }),
            &["user"],
        ),
        tool(
            "slack_add_reaction",
            "Add an emoji reaction to a message.",
            json!({
                "channel": channel_prop(),
                "timestamp": {
                    "type": "string",
                    "description": "The ts of the message to react to.",
                },
                "name": {
                    "type": "string",
                    "description": "Emoji name without colons (e.g. thumbsup).",
                },
            }),
            &["channel", "timestamp", "name"],
        ),
    ];
    json!({ "tools": tools })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn tool_list_has_expected_count() {
        let v = tool_list_response();
        let tools = v["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), TOOL_NAMES.len(), "expected all planned tools");
        for t in tools {
            assert!(t["name"].is_string(), "every tool has a name");
            assert!(t["description"].is_string(), "every tool has a description");
            assert_eq!(
                t["inputSchema"]["type"], "object",
                "every tool has an object inputSchema"
            );
            assert!(
                t["inputSchema"]["required"].is_array(),
                "every tool declares a required array"
            );
        }
    }

    #[test]
    fn every_tool_name_is_unique() {
        let v = tool_list_response();
        let mut seen = HashSet::new();
        for t in v["tools"].as_array().unwrap() {
            let name = t["name"].as_str().unwrap().to_string();
            assert!(seen.insert(name.clone()), "duplicate tool: {name}");
        }
    }

    #[test]
    fn known_tools_match_registry() {
        let v = tool_list_response();
        let built: HashSet<&str> = v["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        let declared: HashSet<&str> = TOOL_NAMES.iter().copied().collect();
        assert_eq!(built, declared, "TOOL_NAMES must match the built registry");
        for name in TOOL_NAMES {
            assert!(is_known_tool(name), "{name} must be a known tool");
        }
        assert!(!is_known_tool("slack_not_a_tool"));
    }
}
