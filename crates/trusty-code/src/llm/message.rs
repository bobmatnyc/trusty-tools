//! `ChatMessage` wire type and role constructors.
//!
//! Why: The conversation-message type is used by both requests (history) and
//! tool-result injection; isolating it here keeps the request and response
//! modules free of message construction boilerplate.
//! What: Defines `ChatMessage` with `role`, `content`, `tool_calls`,
//! `tool_call_id`, and `name` fields, plus convenience constructors for each
//! standard role.
//! Test: `chat_message_constructors`, `chat_message_tool_role_round_trip`,
//! `chat_message_serialises_all_roles`.

use serde::{Deserialize, Serialize};

use super::request::ToolCall;

/// A single message in the chat conversation.
///
/// Why: OpenRouter's `/chat/completions` endpoint uses the standard OpenAI
/// message schema; `role` distinguishes speaker identity and `content` carries
/// the text payload (or `null` for tool-call-only turns).
/// What: `role` is one of `"system"`, `"user"`, `"assistant"`, `"tool"`;
/// `content` is the text or `None` for assistant turns that only emit tool
/// calls; `tool_calls` carries the outbound calls for assistant turns;
/// `tool_call_id` and `name` are populated for `tool` role messages.
/// Test: `chat_message_serialises_all_roles`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    /// Conversation role: `"system"`, `"user"`, `"assistant"`, or `"tool"`.
    pub role: String,

    /// Text content; `None` on assistant turns that only emit tool calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,

    /// Tool calls emitted by the assistant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,

    /// For `tool` role messages: the ID of the tool call this message responds to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,

    /// For `tool` role messages: the name of the function that was called.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    /// Construct a `system` message.
    ///
    /// Why: Convenience constructor; avoids repeated `role.to_string()` boilerplate.
    /// What: Returns a `ChatMessage` with `role = "system"` and the provided content.
    /// Test: `chat_message_constructors`.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Construct a `user` message.
    ///
    /// Why: Convenience constructor for the most common message type.
    /// What: Returns a `ChatMessage` with `role = "user"` and the provided content.
    /// Test: `chat_message_constructors`.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Construct an `assistant` message with text content.
    ///
    /// Why: Convenience constructor for building conversation history from prior
    /// assistant responses.
    /// What: Returns a `ChatMessage` with `role = "assistant"` and the provided content.
    /// Test: `chat_message_constructors`.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Construct a `tool` result message.
    ///
    /// Why: After executing a tool call, the result must be fed back in a `tool`
    /// role message referencing the original `tool_call_id`.
    /// What: Returns a `ChatMessage` with `role = "tool"`, the call ID, function
    /// name, and result content.
    /// Test: `chat_message_tool_role_round_trip`.
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: Some(name.into()),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience constructors produce the right `role` strings.
    ///
    /// Why: Verify each static constructor sets the role correctly.
    /// What: Assert roles for `system`, `user`, `assistant`.
    /// Test: this test.
    #[test]
    fn chat_message_constructors() {
        assert_eq!(ChatMessage::system("s").role, "system");
        assert_eq!(ChatMessage::user("u").role, "user");
        assert_eq!(ChatMessage::assistant("a").role, "assistant");
    }

    /// `tool_result` constructor sets `tool_call_id` and `name`.
    ///
    /// Why: Tool result messages have extra required fields; the constructor must
    /// populate them correctly.
    /// What: Assert `tool_call_id` and `name` are `Some`.
    /// Test: this test.
    #[test]
    fn chat_message_tool_role_round_trip() {
        let msg = ChatMessage::tool_result("call_abc", "get_weather", r#"{"temp":72}"#);
        assert_eq!(msg.role, "tool");
        assert_eq!(msg.tool_call_id.as_deref(), Some("call_abc"));
        assert_eq!(msg.name.as_deref(), Some("get_weather"));
        assert_eq!(msg.content.as_deref(), Some(r#"{"temp":72}"#));
    }

    /// All role constructors serialise the `role` field correctly.
    ///
    /// Why: JSON consumers depend on the `role` string value; a typo in any
    /// constructor would silently break the API.
    /// What: Serialise each message variant, assert the `role` JSON field value.
    /// Test: this test.
    #[test]
    fn chat_message_serialises_all_roles() {
        for (msg, expected_role) in [
            (ChatMessage::system("sys"), "system"),
            (ChatMessage::user("usr"), "user"),
            (ChatMessage::assistant("ast"), "assistant"),
            (ChatMessage::tool_result("id", "fn", "result"), "tool"),
        ] {
            let v: serde_json::Value = serde_json::to_value(&msg).expect("serialise");
            assert_eq!(
                v["role"].as_str(),
                Some(expected_role),
                "role mismatch for expected={expected_role}"
            );
        }
    }
}
