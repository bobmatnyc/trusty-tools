//! Response-side wire types for the OpenRouter / OpenAI chat-completions API.
//!
//! Why: Keeping `AssistantMessage`, `ChatChoice`, and `ChatResponse` in their
//! own module separates the deserialisation surface from request construction
//! and makes response-handling logic easy to find and test.
//! What: Defines the response hierarchy returned by `POST /v1/chat/completions`
//! and helper methods on `ChatResponse` for accessing common fields.
//! Test: `chat_response_deserialises_fixture`, `tool_call_deserialises_from_fixture`,
//! `chat_response_first_text_content`, `chat_response_first_tool_calls`,
//! `chat_response_usage_into_token_usage`.

use serde::{Deserialize, Serialize};

use crate::perf::TokenUsage;

use super::request::ToolCall;
use super::usage::UsageBlock;

/// The assistant message within a choice.
///
/// Why: Mirrors `ChatMessage` but uses explicit struct rather than re-using the
/// request type to keep request/response paths cleanly separated and avoid
/// accidental mis-serialisation. `Serialize` (#2264) lets the debug-capture
/// transcript (`llm::debug_capture`) dump the FULL response — including
/// tool-call arguments, not just names — verbatim to JSONL.
/// What: `content` is the text (or `None` for tool-only turns); `tool_calls`
/// carries any function invocations.
/// Test: `assistant_message_text_and_tool_calls`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    /// Text content; `None` when the turn consists solely of tool calls.
    pub content: Option<String>,

    /// Tool calls emitted this turn.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

/// One choice in the model response.
///
/// Why: The API returns a `choices` array (allowing `n > 1`); we always request
/// a single choice but the struct must match the wire schema.
/// What: `message` holds the assistant turn (text and/or tool calls);
/// `finish_reason` carries the stop condition.
/// Test: `chat_response_deserialises_fixture`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoice {
    /// The assistant message produced for this choice.
    pub message: AssistantMessage,

    /// Why the model stopped: `"stop"`, `"tool_calls"`, `"length"`, `"content_filter"`.
    pub finish_reason: Option<String>,
}

/// The top-level response from `POST /v1/chat/completions`.
///
/// Why: Wraps the `choices` array and `usage` block so callers receive a single
/// typed value from `LlmClient::chat`.
/// What: `id` is the response ID; `choices` contains the generated turns;
/// `usage` carries token accounting. `model` (#1475 bug 2) is the RESOLVED
/// model slug the provider actually served the request with — OpenRouter
/// (and the underlying OpenAI-compatible API) echoes this at the top level
/// of every response, which can differ from the request's `model` field
/// when routing/fallback resolves to a concrete backing model (e.g. an
/// `:auto` or multi-model routing slug). `#[serde(default)]` so fixtures
/// and mocks that omit it still deserialise — see
/// [`Self::resolved_model`] for the fallback this enables.
/// Test: `chat_response_deserialises_fixture`,
/// `resolved_model_prefers_response_model_over_requested`,
/// `resolved_model_falls_back_to_requested_when_absent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    /// Opaque response identifier from the provider.
    pub id: String,

    /// The resolved model slug the provider actually used (#1475 bug 2).
    /// Empty when the response omits it (older fixtures, some mocks) —
    /// callers should use [`Self::resolved_model`] rather than this field
    /// directly.
    #[serde(default)]
    pub model: String,

    /// Generated choices (one per `n`; we always request `n = 1`).
    pub choices: Vec<ChatChoice>,

    /// Token accounting for this request.
    #[serde(default)]
    pub usage: UsageBlock,
}

impl ChatResponse {
    /// Extract the first choice's text content, if any.
    ///
    /// Why: The vast majority of calls want the assistant's text response; this
    /// convenience method avoids repetitive indexing at every call site.
    /// What: Returns `choices[0].message.content.clone()` or `None` when the
    /// response has no choices or the content is absent.
    /// Test: `chat_response_first_text_content`.
    pub fn first_text(&self) -> Option<String> {
        self.choices.first()?.message.content.clone()
    }

    /// Extract the first choice's tool calls.
    ///
    /// Why: Tool-call workflows need quick access to the emitted calls without
    /// indexing into nested fields each time.
    /// What: Returns a reference to `choices[0].message.tool_calls`, or an empty
    /// slice when there are no choices.
    /// Test: `chat_response_first_tool_calls`.
    pub fn first_tool_calls(&self) -> &[ToolCall] {
        self.choices
            .first()
            .map(|c| c.message.tool_calls.as_slice())
            .unwrap_or(&[])
    }

    /// Extract the finish reason from the first choice.
    ///
    /// Why: Callers need the stop condition to decide whether to continue the
    /// tool-call loop or surface the response.
    /// What: Returns `choices[0].finish_reason.as_deref()` or `None`.
    /// Test: `chat_response_deserialises_fixture`.
    pub fn finish_reason(&self) -> Option<&str> {
        self.choices.first()?.finish_reason.as_deref()
    }

    /// Convert the response's `usage` block into `TokenUsage`.
    ///
    /// Why: `PerfCollector::record_phase` accepts `&TokenUsage`; this method
    /// keeps the conversion at one place.
    /// What: Delegates to `UsageBlock::into_token_usage`.
    /// Test: `chat_response_usage_into_token_usage`.
    pub fn token_usage(self) -> TokenUsage {
        self.usage.into_token_usage()
    }

    /// The model slug to attribute this turn to for recording/pricing
    /// (#1475 bug 2): the RESOLVED model the provider reports, falling back
    /// to the originally `requested` slug when the response didn't carry
    /// one.
    ///
    /// Why: `run_task::recorder::RecordingLlmClient` and any future
    /// transcript recorder must record what the provider actually ran, not
    /// merely what was asked for — a routing/fallback slug (`:auto`, a
    /// multi-model alias) can resolve to a different concrete model.
    /// What: Returns `&self.model` when non-empty, else `requested`.
    /// Test: `resolved_model_prefers_response_model_over_requested`,
    /// `resolved_model_falls_back_to_requested_when_absent`.
    pub fn resolved_model<'a>(&'a self, requested: &'a str) -> &'a str {
        if self.model.is_empty() {
            requested
        } else {
            &self.model
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative API response fixture deserialises correctly.
    ///
    /// Why: Ensures the struct layout matches the real OpenRouter wire format.
    /// What: Parse a JSON string modelled on an actual response; assert all
    /// top-level fields are populated.
    /// Test: this test.
    #[test]
    fn chat_response_deserialises_fixture() {
        let fixture = r#"{
          "id": "gen-abc123",
          "choices": [
            {
              "message": {
                "role": "assistant",
                "content": "Hello, world!",
                "tool_calls": []
              },
              "finish_reason": "stop"
            }
          ],
          "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 4,
            "total_tokens": 14
          }
        }"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise fixture");
        assert_eq!(resp.id, "gen-abc123");
        assert_eq!(resp.first_text().as_deref(), Some("Hello, world!"));
        assert_eq!(resp.finish_reason(), Some("stop"));
        assert!(resp.first_tool_calls().is_empty());
    }

    /// A response with a tool call deserialises `tool_calls`.
    ///
    /// Why: Tool-calling workflow depends on correctly parsing `tool_calls` from
    /// the response.
    /// What: Parse a fixture with `tool_calls` populated; assert name and args.
    /// Test: this test.
    #[test]
    fn tool_call_deserialises_from_fixture() {
        let fixture = r#"{
          "id": "gen-xyz",
          "choices": [
            {
              "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [
                  {
                    "id": "call_001",
                    "type": "function",
                    "function": {
                      "name": "get_weather",
                      "arguments": "{\"location\":\"Seattle\"}"
                    }
                  }
                ]
              },
              "finish_reason": "tool_calls"
            }
          ],
          "usage": {
            "prompt_tokens": 25,
            "completion_tokens": 15,
            "total_tokens": 40
          }
        }"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        assert!(resp.first_text().is_none());
        let calls = resp.first_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_001");
        assert_eq!(calls[0].function.name, "get_weather");
        assert_eq!(resp.finish_reason(), Some("tool_calls"));
    }

    /// `ChatResponse::token_usage` delegates to `UsageBlock::into_token_usage`.
    ///
    /// Why: Callers use `response.token_usage()` rather than digging into
    /// `response.usage`; this test ensures the method works end-to-end.
    /// What: Build a full response fixture, call `token_usage()`, assert counts.
    /// Test: this test.
    #[test]
    fn chat_response_usage_into_token_usage() {
        let fixture = r#"{
          "id": "gen-zzz",
          "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
          "usage": {"prompt_tokens": 8, "completion_tokens": 3, "total_tokens": 11,
                    "cache_read_input_tokens": 5, "cache_creation_input_tokens": 2}
        }"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        let usage = resp.token_usage();
        assert_eq!(usage.prompt_tokens, 8);
        assert_eq!(usage.completion_tokens, 3);
        assert_eq!(usage.cache_read_tokens, 5);
        assert_eq!(usage.cache_creation_tokens, 2);
    }

    /// `ChatResponse::first_text` returns `None` for an empty choices list.
    ///
    /// Why: Callers must handle missing text gracefully; validate the `None`
    /// path without panicking.
    /// What: Deserialise a response with an empty `choices` array, assert
    /// `first_text()` is `None`.
    /// Test: this test.
    #[test]
    fn chat_response_first_text_content_empty_choices() {
        let fixture = r#"{"id":"x","choices":[],"usage":{}}"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        assert!(resp.first_text().is_none());
        assert!(resp.first_tool_calls().is_empty());
        assert!(resp.finish_reason().is_none());
    }

    /// `resolved_model` prefers the response's own `model` field when
    /// present, even if it differs from what was requested (#1475 bug 2).
    #[test]
    fn resolved_model_prefers_response_model_over_requested() {
        let fixture =
            r#"{"id":"x","model":"openai/gpt-4o-mini-2024-07-18","choices":[],"usage":{}}"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        assert_eq!(
            resp.resolved_model("openai/gpt-4o-mini"),
            "openai/gpt-4o-mini-2024-07-18"
        );
    }

    /// `resolved_model` falls back to the requested slug when the response
    /// omits `model` (older fixtures, some mocks) (#1475 bug 2).
    #[test]
    fn resolved_model_falls_back_to_requested_when_absent() {
        let fixture = r#"{"id":"x","choices":[],"usage":{}}"#;
        let resp: ChatResponse = serde_json::from_str(fixture).expect("deserialise");
        assert_eq!(
            resp.resolved_model("openai/gpt-4o-mini"),
            "openai/gpt-4o-mini"
        );
    }
}
