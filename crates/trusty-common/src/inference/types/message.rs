//! [`ChatMessage`] — the conversation-message wire type.
//!
//! Why: the message type is used by both requests (history) and tool-result
//! injection; isolating it keeps the request/response modules free of message
//! construction boilerplate. It mirrors tcode's proven type — including the
//! `cache_control` content-block switch (#2156) — so #2406 migrates without a
//! wire change.
//! What: [`ChatMessage`] with `role`, `content`, `tool_calls`, `tool_call_id`,
//! `name`, and `cache_control`, plus a constructor per standard role. The
//! hand-written `Serialize` converts `content` into a cache-annotated content
//! block when `cache_control` is set.
//! Test: inline `tests` — `constructors_set_role`, `tool_result_fields`,
//! `serialises_all_roles`, `cache_control_serialises_as_block`,
//! `plain_content_when_no_cache_control`.

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

use super::tool::{CacheControl, ToolCall};

/// A single message in the chat conversation.
///
/// Why: the OpenAI/OpenRouter `/chat/completions` schema distinguishes speaker
/// identity by `role` and carries the text payload in `content` (or `null` for
/// tool-call-only assistant turns).
/// What: `role` is one of `"system"`/`"user"`/`"assistant"`/`"tool"`; `content`
/// is the text or `None`; `tool_calls` carries outbound calls on assistant
/// turns; `tool_call_id`/`name` are populated for `tool` messages;
/// `cache_control` is a prompt-cache breakpoint applied at serialisation time.
/// Test: `serialises_all_roles`, `cache_control_serialises_as_block`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChatMessage {
    /// Conversation role.
    pub role: String,
    /// Text content; `None` on assistant turns that only emit tool calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Tool calls emitted by the assistant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// For `tool` messages: the id of the tool call this responds to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// For `tool` messages: the name of the function that was called.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Prompt-cache breakpoint. Request-only, so dropped on deserialise via
    /// `#[serde(skip)]`; see [`Self::serialize`] for its wire effect.
    #[serde(skip)]
    pub cache_control: Option<CacheControl>,
}

impl Serialize for ChatMessage {
    /// Serialise, converting `content` into a one-element cache-annotated block
    /// array when `cache_control` is set, and a bare string otherwise.
    ///
    /// Why: the caching passthrough only honours the breakpoint when `content`
    /// is shaped as `[{"type":"text","text":...,"cache_control":{...}}]` — a
    /// plain string is never inspected. Hand-writing this keeps `content:
    /// Option<String>` for every caller while branching only the wire encoding.
    /// What: `content` serialises as the block array when both `content` and
    /// `cache_control` are `Some`, a bare string when only `content` is `Some`,
    /// and is omitted when `content` is `None`. Other fields serialise as their
    /// `Option::is_none`-skipped selves; `cache_control` never appears as its
    /// own key.
    /// Test: `cache_control_serialises_as_block`, `plain_content_when_no_cache_control`.
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

/// Wire shape for one Anthropic-ephemeral cached text content block.
///
/// Why: `[{"type":"text","text":...,"cache_control":{"type":"ephemeral"}}]` is
/// the documented content-caching shape; this private struct is the single place
/// it is expressed instead of building it ad hoc in [`ChatMessage::serialize`].
/// What: `kind` is always `"text"`; `text`/`cache_control` borrow from the owner
/// to avoid a clone during serialisation.
/// Test: exercised via `cache_control_serialises_as_block`.
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
    /// Why: convenience constructor avoiding `role.into()` boilerplate.
    /// What: `role = "system"`, provided `content`, all optionals `None`.
    /// Test: `constructors_set_role`.
    pub fn system(content: impl Into<String>) -> Self {
        Self::text("system", content)
    }

    /// Construct a `user` message.
    ///
    /// Why: convenience constructor for the most common message type.
    /// What: `role = "user"`, provided `content`.
    /// Test: `constructors_set_role`.
    pub fn user(content: impl Into<String>) -> Self {
        Self::text("user", content)
    }

    /// Construct an `assistant` message with text content.
    ///
    /// Why: convenience constructor for building history from prior responses.
    /// What: `role = "assistant"`, provided `content`.
    /// Test: `constructors_set_role`.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::text("assistant", content)
    }

    /// Construct a `tool` result message.
    ///
    /// Why: after executing a tool call, the result is fed back in a `tool`
    /// message referencing the original `tool_call_id`.
    /// What: `role = "tool"` with the call id, function name, and result content.
    /// Test: `tool_result_fields`.
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

    /// Private shared constructor for the plain-text roles.
    ///
    /// Why: `system`/`user`/`assistant` differ only in the role string; one
    /// helper removes the triplicated field boilerplate.
    /// What: builds a text-only message with all optional fields `None`.
    fn text(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            cache_control: None,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: each constructor must set the right `role` string.
    /// Test: itself.
    #[test]
    fn constructors_set_role() {
        assert_eq!(ChatMessage::system("s").role, "system");
        assert_eq!(ChatMessage::user("u").role, "user");
        assert_eq!(ChatMessage::assistant("a").role, "assistant");
    }

    /// Why: tool results carry extra required fields.
    /// Test: itself.
    #[test]
    fn tool_result_fields() {
        let m = ChatMessage::tool_result("call_abc", "get_weather", r#"{"t":72}"#);
        assert_eq!(m.role, "tool");
        assert_eq!(m.tool_call_id.as_deref(), Some("call_abc"));
        assert_eq!(m.name.as_deref(), Some("get_weather"));
    }

    /// Why: JSON consumers depend on the `role` value; a typo would silently
    /// break the API.
    /// Test: itself.
    #[test]
    fn serialises_all_roles() {
        for (m, role) in [
            (ChatMessage::system("x"), "system"),
            (ChatMessage::user("x"), "user"),
            (ChatMessage::assistant("x"), "assistant"),
            (ChatMessage::tool_result("i", "f", "r"), "tool"),
        ] {
            let v: serde_json::Value = serde_json::to_value(&m).expect("serialise");
            assert_eq!(v["role"].as_str(), Some(role));
        }
    }

    /// Why: a `cache_control`-bearing message must emit the block-array shape.
    /// Test: itself.
    #[test]
    fn cache_control_serialises_as_block() {
        let mut m = ChatMessage::system("you are helpful");
        m.cache_control = Some(CacheControl::ephemeral());
        let v: serde_json::Value = serde_json::to_value(&m).expect("serialise");
        assert_eq!(
            v["content"],
            serde_json::json!([{
                "type": "text",
                "text": "you are helpful",
                "cache_control": {"type": "ephemeral"}
            }])
        );
    }

    /// Why: the default (no cache_control) path must stay a bare string.
    /// Test: itself.
    #[test]
    fn plain_content_when_no_cache_control() {
        let m = ChatMessage::system("hi");
        let v: serde_json::Value = serde_json::to_value(&m).expect("serialise");
        assert_eq!(v["content"], serde_json::json!("hi"));
    }
}
