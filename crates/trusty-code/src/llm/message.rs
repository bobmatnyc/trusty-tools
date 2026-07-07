//! `ChatMessage` wire type and role constructors.
//!
//! Why: The conversation-message type is used by both requests (history) and
//! tool-result injection; isolating it here keeps the request and response
//! modules free of message construction boilerplate.
//! What: Defines `ChatMessage` with `role`, `content`, `tool_calls`,
//! `tool_call_id`, `name`, and (#2156) `cache_control` fields, plus convenience
//! constructors for each standard role.
//! Test: `chat_message_constructors`, `chat_message_tool_role_round_trip`,
//! `chat_message_serialises_all_roles`,
//! `chat_message_cache_control_serialises_as_content_block`,
//! `chat_message_without_cache_control_serialises_plain_string`.

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

use super::request::{CacheControl, ToolCall};

/// A single message in the chat conversation.
///
/// Why: OpenRouter's `/chat/completions` endpoint uses the standard OpenAI
/// message schema; `role` distinguishes speaker identity and `content` carries
/// the text payload (or `null` for tool-call-only turns).
/// What: `role` is one of `"system"`, `"user"`, `"assistant"`, `"tool"`;
/// `content` is the text or `None` for assistant turns that only emit tool
/// calls; `tool_calls` carries the outbound calls for assistant turns;
/// `tool_call_id` and `name` are populated for `tool` role messages;
/// `cache_control` (#2156) is a prompt-cache breakpoint applied at
/// serialisation time — see [`Self::serialize`] for the wire-shape switch it
/// drives.
/// Test: `chat_message_serialises_all_roles`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
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

    /// Prompt-cache breakpoint (#2156). Never read back from the wire (this
    /// type is request-only — see the module doc), so it is dropped on
    /// deserialise via `#[serde(skip)]` rather than round-tripped.
    /// `agent_loop::build_request` sets this on the system-role message when
    /// the run is cache-eligible; see [`Self::serialize`] for how it changes
    /// `content`'s wire shape.
    #[serde(skip)]
    pub cache_control: Option<CacheControl>,
}

impl Serialize for ChatMessage {
    /// Serialise `ChatMessage`, converting `content` into a one-element
    /// cache-annotated content-block array when `cache_control` is set
    /// (#2156), and a bare string (today's shape) otherwise.
    ///
    /// Why: OpenRouter's Claude `cache_control` passthrough only honours the
    /// breakpoint when `content` is shaped as
    /// `[{"type":"text","text":...,"cache_control":{"type":"ephemeral"}}]` —
    /// a plain string `content` value is never inspected for a cache marker
    /// (verified against OpenRouter's prompt-caching docs and `block/goose`'s
    /// production OpenRouter provider). Hand-writing this impl — rather than
    /// deriving — keeps `content: Option<String>` the type every other call
    /// site (`Transcript`, tests) already depends on; only the wire encoding
    /// branches on `cache_control`.
    /// What: `role` always serialises. `content` serialises as the cached
    /// block array when both `content` and `cache_control` are `Some`, as a
    /// bare string when only `content` is `Some`, and is omitted entirely
    /// when `content` is `None` — matching the previous
    /// `skip_serializing_if = "Option::is_none"` behaviour byte-for-byte when
    /// `cache_control` is `None`. `tool_calls`, `tool_call_id`, and `name`
    /// serialise unchanged; `cache_control` itself never appears as its own
    /// JSON key.
    /// Test: `chat_message_cache_control_serialises_as_content_block`,
    /// `chat_message_without_cache_control_serialises_plain_string`,
    /// `chat_message_serialises_all_roles`.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("ChatMessage", 5)?;
        state.serialize_field("role", &self.role)?;

        match (&self.content, &self.cache_control) {
            (Some(text), Some(cache_control)) => {
                let block = [CachedTextBlock {
                    kind: "text",
                    text: text.as_str(),
                    cache_control,
                }];
                state.serialize_field("content", &block)?;
            }
            (Some(text), None) => state.serialize_field("content", text)?,
            (None, _) => state.skip_field("content")?,
        }

        match &self.tool_calls {
            Some(calls) => state.serialize_field("tool_calls", calls)?,
            None => state.skip_field("tool_calls")?,
        }
        match &self.tool_call_id {
            Some(id) => state.serialize_field("tool_call_id", id)?,
            None => state.skip_field("tool_call_id")?,
        }
        match &self.name {
            Some(name) => state.serialize_field("name", name)?,
            None => state.skip_field("name")?,
        }

        state.end()
    }
}

/// Wire shape for one Anthropic-ephemeral cached text content block (#2156).
///
/// Why: `[{"type":"text","text":...,"cache_control":{"type":"ephemeral"}}]`
/// is exactly OpenRouter's documented Claude `cache_control` passthrough
/// shape for message content; this private struct is the single place that
/// shape is expressed instead of building it ad hoc inside
/// `ChatMessage::serialize`.
/// What: `kind` is always `"text"`; `text`/`cache_control` borrow from the
/// owning `ChatMessage` to avoid a clone during serialisation.
/// Test: exercised indirectly via
/// `chat_message_cache_control_serialises_as_content_block`.
#[derive(Serialize)]
struct CachedTextBlock<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
    cache_control: &'a CacheControl,
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
            cache_control: None,
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
            cache_control: None,
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
            cache_control: None,
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
            cache_control: None,
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

    /// A message with `cache_control` set serialises `content` as a
    /// one-element cache-annotated block array (#2156).
    ///
    /// Why: This is the exact shape OpenRouter's Claude passthrough requires
    /// to honour the breakpoint; a plain-string `content` would be silently
    /// ignored by Anthropic's caching layer.
    /// What: Build a system message, set `cache_control`, serialise, assert
    /// `content` is `[{"type":"text","text":...,"cache_control":
    /// {"type":"ephemeral"}}]`.
    /// Test: this test.
    #[test]
    fn chat_message_cache_control_serialises_as_content_block() {
        let mut msg = ChatMessage::system("you are helpful");
        msg.cache_control = Some(CacheControl::ephemeral());

        let v: serde_json::Value = serde_json::to_value(&msg).expect("serialise");
        assert_eq!(
            v["content"],
            serde_json::json!([{
                "type": "text",
                "text": "you are helpful",
                "cache_control": {"type": "ephemeral"}
            }])
        );
        assert_eq!(v["role"], "system");
    }

    /// A message WITHOUT `cache_control` serialises `content` as a bare
    /// string — byte-identical to pre-#2156 output (#2156 regression guard).
    ///
    /// Why: Every non-caching request (Parity mode, non-Anthropic models)
    /// must be unaffected by this change; this pins that the default
    /// (`cache_control: None`) path never emits the block-array shape.
    /// What: Build a system message via the ordinary constructor (leaving
    /// `cache_control` at its default `None`), serialise, assert `content`
    /// is the plain string.
    /// Test: this test.
    #[test]
    fn chat_message_without_cache_control_serialises_plain_string() {
        let msg = ChatMessage::system("you are helpful");
        assert!(msg.cache_control.is_none());

        let v: serde_json::Value = serde_json::to_value(&msg).expect("serialise");
        assert_eq!(v["content"], serde_json::json!("you are helpful"));
    }
}
