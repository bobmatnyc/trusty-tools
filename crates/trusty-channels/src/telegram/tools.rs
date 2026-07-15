//! MCP `tools/list` response — JSON Schema for every planned Telegram tool.
//!
//! Why: Claude Code (and any MCP client) needs a machine-readable contract
//! describing each tool's arguments so the model can fill them correctly. This
//! is the single source of truth for the Telegram MCP surface; the dispatcher
//! in `server` routes on the same names. Every schema here is authoritative even
//! though the handlers are still stubs (live calls deferred, see issue #2641).
//! What: `tool_list_response()` returns `{"tools": [{name, description,
//! inputSchema}, ...]}`; `is_known_tool()` reports whether a name is part of the
//! planned surface so the dispatcher can distinguish "not yet implemented" from
//! "unknown tool". The reduced Telegram surface is send/info-only by design: a
//! Telegram *bot* cannot read or search prior chat history via the Bot API — it
//! can only send messages and query chat/bot metadata. Each tool's description
//! restates that limitation so callers see it without reading source.
//! Test: Unit tests below assert the tool count, required-field shape, name
//! uniqueness, the history-limitation note on every description, and
//! `is_known_tool` agreement with the registry.

use serde_json::{json, Value};

/// Shared clause restating the Bot API history/search limitation. Appended to
/// every tool description so an MCP client sees the constraint inline.
const HISTORY_LIMIT_NOTE: &str = " Note: a Telegram bot cannot read or search \
prior chat history via the Bot API — it can only send messages and query \
chat/bot metadata.";

/// The authoritative list of planned Telegram tool names.
///
/// Why: Both `tool_list_response` (indirectly) and `is_known_tool` must agree
/// on the exact surface; a single constant array prevents drift.
/// What: The three send/info-only chat-as-tools operations planned for the
/// native Telegram MCP (reduced scope, issue #2641).
/// Test: `known_tools_match_registry` cross-checks this against the built list.
pub const TOOL_NAMES: &[&str] = &[
    "telegram_send_message",
    "telegram_get_bot_info",
    "telegram_get_chat",
];

/// Report whether `name` is a planned Telegram tool.
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

/// A `chat_id` string-schema fragment shared by several tools.
fn chat_id_prop() -> Value {
    json!({
        "type": "string",
        "description": "Target chat: a numeric chat ID (e.g. 123456789) or an \
                        @channelusername.",
    })
}

/// Build the full `tools/list` response.
///
/// Why: One function = one source of truth for the MCP contract.
/// What: Returns the three planned Telegram tools with minimal-but-valid
/// schemas; each description ends with the history-limitation note.
/// Test: `tool_list_has_expected_count` asserts exactly three tools.
pub fn tool_list_response() -> Value {
    let tools = vec![
        tool(
            "telegram_send_message",
            &format!(
                "Send a text message to a Telegram chat (the bot must already be \
                 a member/admin of the target chat).{HISTORY_LIMIT_NOTE}"
            ),
            json!({
                "chat_id": chat_id_prop(),
                "text": { "type": "string", "description": "Message text to send." },
                "parse_mode": {
                    "type": "string",
                    "description": "Optional text formatting mode (e.g. MarkdownV2 or HTML).",
                },
                "reply_to_message_id": {
                    "type": "integer",
                    "description": "Optional message ID to reply to within the chat.",
                },
            }),
            &["chat_id", "text"],
        ),
        tool(
            "telegram_get_bot_info",
            &format!(
                "Fetch the authenticated bot's own identity/metadata (Bot API \
                 getMe).{HISTORY_LIMIT_NOTE}"
            ),
            json!({}),
            &[],
        ),
        tool(
            "telegram_get_chat",
            &format!(
                "Fetch metadata for a chat the bot can see (title, type, member \
                 counts; Bot API getChat).{HISTORY_LIMIT_NOTE}"
            ),
            json!({
                "chat_id": chat_id_prop(),
            }),
            &["chat_id"],
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
    fn every_description_documents_history_limit() {
        // The Bot-API history/search limitation must be visible on every tool
        // description (issue #2641 acceptance criterion).
        let v = tool_list_response();
        for t in v["tools"].as_array().unwrap() {
            let desc = t["description"].as_str().unwrap();
            assert!(
                desc.contains("cannot read or search"),
                "tool {} must document the history limitation",
                t["name"]
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
        assert!(!is_known_tool("telegram_not_a_tool"));
    }
}
