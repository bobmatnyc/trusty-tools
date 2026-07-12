//! [`ChatResponse`] — the normalized inference response.
//!
//! Why: callers want one typed value from every adapter regardless of provider
//! wire quirks. This mirrors tcode's response hierarchy (so #2406 migrates
//! cleanly) and adds a typed [`StopReason`] view over the wire `finish_reason`
//! string.
//! What: [`ChatResponse`] wraps the `choices` array and the [`Usage`] block, with
//! convenience accessors ([`ChatResponse::first_text`],
//! [`ChatResponse::first_tool_calls`], [`ChatResponse::stop_reason`],
//! [`ChatResponse::usage`]). [`AssistantMessage`]/[`ChatChoice`] are the nested
//! wire shapes; [`StopReason`] is the typed stop-condition.
//! Test: inline `tests` — `deserialises_text_fixture`, `deserialises_tool_call`,
//! `stop_reason_maps_variants`, `usage_flows_through`, `first_text_empty_choices`.

use serde::{Deserialize, Serialize};

use super::tool::ToolCall;
use super::usage::{Usage, UsageBlock};

/// Why the model stopped generating, as a typed view over the wire string.
///
/// Why: callers branch on the stop condition (continue the tool loop vs. surface
/// the final text vs. flag a truncation); matching an enum is safer than
/// comparing raw strings at every site.
/// What: the four standard OpenAI/OpenRouter finish reasons plus an `Other`
/// catch-all preserving any unrecognised wire value.
/// Test: `stop_reason_maps_variants`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The model reached a natural stopping point (`"stop"`).
    Stop,
    /// The model emitted tool calls and is awaiting results (`"tool_calls"`).
    ToolCalls,
    /// Generation hit the token limit (`"length"`).
    Length,
    /// The content filter halted generation (`"content_filter"`).
    ContentFilter,
    /// Any other provider-specific reason, preserved verbatim.
    Other(String),
}

impl StopReason {
    /// Parse a wire `finish_reason` string into a [`StopReason`].
    ///
    /// Why: keeps the string→enum mapping in one place so every accessor agrees.
    /// What: maps the four known values; anything else becomes `Other`.
    /// Test: `stop_reason_maps_variants`.
    pub fn from_wire(reason: &str) -> Self {
        match reason {
            "stop" => Self::Stop,
            "tool_calls" => Self::ToolCalls,
            "length" => Self::Length,
            "content_filter" => Self::ContentFilter,
            other => Self::Other(other.to_string()),
        }
    }
}

/// The assistant message within a choice.
///
/// Why: an explicit response-side struct (rather than reusing [`super::ChatMessage`])
/// keeps request and response paths cleanly separated and lets the full response
/// — including tool-call arguments — be dumped verbatim for debug capture.
/// What: `content` is the text (or `None` for tool-only turns); `tool_calls`
/// carries any function invocations.
/// Test: `deserialises_tool_call`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    /// Text content; `None` when the turn is solely tool calls.
    pub content: Option<String>,
    /// Tool calls emitted this turn.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

/// One choice in the model response.
///
/// Why: the API returns a `choices` array (allowing `n > 1`); we always request
/// one but the struct must match the wire schema.
/// What: `message` holds the assistant turn; `finish_reason` carries the raw
/// stop condition (viewed typed via [`ChatResponse::stop_reason`]).
/// Test: `deserialises_text_fixture`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoice {
    /// The assistant message produced for this choice.
    pub message: AssistantMessage,
    /// Raw wire stop condition, e.g. `"stop"` or `"tool_calls"`.
    pub finish_reason: Option<String>,
}

/// The top-level response from a chat/completions call.
///
/// Why: wraps the `choices` array and `usage` so callers receive one typed value
/// from [`super::super::adapter::InferenceAdapter::chat`].
/// What: `id` is the provider response id; `model` is the RESOLVED model slug the
/// provider actually served (may differ from the request under routing/fallback);
/// `choices` holds the generated turns; `usage` is the normalized [`Usage`]
/// (parsed from the wire [`UsageBlock`]).
/// Test: `deserialises_text_fixture`, `usage_flows_through`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    /// Opaque provider response identifier.
    pub id: String,
    /// Resolved model slug the provider actually used. Empty when the response
    /// omits it — use [`Self::resolved_model`] rather than this field directly.
    #[serde(default)]
    pub model: String,
    /// Generated choices (one per `n`; we always request `n = 1`).
    pub choices: Vec<ChatChoice>,
    /// Token + cost accounting for this call.
    #[serde(default)]
    pub usage: UsageBlock,
}

impl ChatResponse {
    /// The first choice's text content, if any.
    ///
    /// Why: most calls want the assistant's text; this avoids repetitive
    /// indexing at every call site.
    /// What: `choices[0].message.content`, or `None` when absent.
    /// Test: `deserialises_text_fixture`, `first_text_empty_choices`.
    pub fn first_text(&self) -> Option<String> {
        self.choices.first()?.message.content.clone()
    }

    /// The first choice's tool calls.
    ///
    /// Why: tool-call workflows need the emitted calls without nested indexing.
    /// What: `choices[0].message.tool_calls`, or an empty slice.
    /// Test: `deserialises_tool_call`.
    pub fn first_tool_calls(&self) -> &[ToolCall] {
        self.choices
            .first()
            .map(|c| c.message.tool_calls.as_slice())
            .unwrap_or(&[])
    }

    /// The typed stop reason from the first choice.
    ///
    /// Why: callers decide whether to continue the tool loop or surface the
    /// response; the typed enum is safer than string comparison.
    /// What: parses `choices[0].finish_reason` via [`StopReason::from_wire`];
    /// `None` when there is no choice or no finish reason.
    /// Test: `stop_reason_maps_variants`.
    pub fn stop_reason(&self) -> Option<StopReason> {
        self.choices
            .first()?
            .finish_reason
            .as_deref()
            .map(StopReason::from_wire)
    }

    /// The normalized [`Usage`] for this call.
    ///
    /// Why: callers speak [`Usage`], not the wire [`UsageBlock`].
    /// What: clones and converts `self.usage` via [`UsageBlock::into_usage`].
    /// Test: `usage_flows_through`.
    pub fn usage(&self) -> Usage {
        self.usage.clone().into_usage()
    }

    /// The model slug to attribute this turn to: the resolved model the provider
    /// reports, falling back to the requested slug when the response omits one.
    ///
    /// Why: recorders/pricing must attribute what the provider actually ran, not
    /// merely what was asked — a routing/fallback slug can resolve differently.
    /// What: `&self.model` when non-empty, else `requested`.
    /// Test: `resolved_model_prefers_response_then_requested`.
    pub fn resolved_model<'a>(&'a self, requested: &'a str) -> &'a str {
        if self.model.is_empty() {
            requested
        } else {
            &self.model
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the struct layout must match the real wire format.
    /// Test: itself.
    #[test]
    fn deserialises_text_fixture() {
        let fixture = r#"{
          "id": "gen-abc",
          "choices": [{"message": {"role": "assistant", "content": "Hello", "tool_calls": []},
                       "finish_reason": "stop"}],
          "usage": {"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14}
        }"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        assert_eq!(resp.id, "gen-abc");
        assert_eq!(resp.first_text().as_deref(), Some("Hello"));
        assert_eq!(resp.stop_reason(), Some(StopReason::Stop));
        assert!(resp.first_tool_calls().is_empty());
    }

    /// Why: tool-call workflows depend on parsing `tool_calls`.
    /// Test: itself.
    #[test]
    fn deserialises_tool_call() {
        let fixture = r#"{
          "id": "gen-xyz",
          "choices": [{"message": {"role": "assistant", "content": null,
              "tool_calls": [{"id": "call_1", "type": "function",
                  "function": {"name": "get_weather", "arguments": "{\"loc\":\"SEA\"}"}}]},
              "finish_reason": "tool_calls"}],
          "usage": {}
        }"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        assert!(resp.first_text().is_none());
        let calls = resp.first_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "get_weather");
        assert_eq!(resp.stop_reason(), Some(StopReason::ToolCalls));
    }

    /// Why: the string→enum mapping must cover every known reason plus `Other`.
    /// Test: itself.
    #[test]
    fn stop_reason_maps_variants() {
        assert_eq!(StopReason::from_wire("stop"), StopReason::Stop);
        assert_eq!(StopReason::from_wire("tool_calls"), StopReason::ToolCalls);
        assert_eq!(StopReason::from_wire("length"), StopReason::Length);
        assert_eq!(
            StopReason::from_wire("content_filter"),
            StopReason::ContentFilter
        );
        assert_eq!(
            StopReason::from_wire("weird"),
            StopReason::Other("weird".into())
        );
    }

    /// Why: usage must flow through the response accessor into `Usage`.
    /// Test: itself.
    #[test]
    fn usage_flows_through() {
        let fixture = r#"{
          "id": "z",
          "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
          "usage": {"prompt_tokens": 8, "completion_tokens": 3,
                    "cache_read_input_tokens": 5, "cache_creation_input_tokens": 2}
        }"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        let u = resp.usage();
        assert_eq!(u.prompt_tokens, 8);
        assert_eq!(u.cache_read_tokens, 5);
        assert_eq!(u.cache_creation_tokens, 2);
    }

    /// Why: callers must handle an empty choices list without panicking.
    /// Test: itself.
    #[test]
    fn first_text_empty_choices() {
        let resp: ChatResponse =
            serde_json::from_str(r#"{"id":"x","choices":[],"usage":{}}"#).expect("deserialise");
        assert!(resp.first_text().is_none());
        assert!(resp.stop_reason().is_none());
        assert!(resp.first_tool_calls().is_empty());
    }

    /// Why: recorders must prefer the resolved model, falling back to requested.
    /// Test: itself.
    #[test]
    fn resolved_model_prefers_response_then_requested() {
        let with: ChatResponse =
            serde_json::from_str(r#"{"id":"x","model":"served/model","choices":[],"usage":{}}"#)
                .expect("deserialise");
        assert_eq!(with.resolved_model("asked/model"), "served/model");

        let without: ChatResponse =
            serde_json::from_str(r#"{"id":"x","choices":[],"usage":{}}"#).expect("deserialise");
        assert_eq!(without.resolved_model("asked/model"), "asked/model");
    }
}
