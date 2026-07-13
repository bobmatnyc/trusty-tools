//! Anthropic Messages-API response translation (issue #2408, epic #2400 Wave 2).
//!
//! Why: an Anthropic `/v1/messages` response does NOT match the OpenAI response
//! the shared core parses — text/tool calls arrive as a `content` block array
//! (not `choices[].message`), the stop condition is `stop_reason`
//! (`end_turn`/`tool_use`/`max_tokens`), and — the trap the #2403 review flagged —
//! its `usage` uses `input_tokens`/`output_tokens`, which deserialise to ZERO
//! through the OpenAI-shaped [`UsageBlock`] (`prompt_tokens`/`completion_tokens`).
//! So the usage must be HAND-constructed, not serde-mapped through the OpenAI
//! shape. This module parses the native response and normalises it into the same
//! [`ChatResponse`] every other adapter returns, so callers stay uniform.
//! What: [`parse`] deserialises the Anthropic body and converts it — text blocks
//! concatenated into `content`, `tool_use` blocks into OpenAI-style [`ToolCall`]s,
//! `stop_reason` mapped to the neutral `finish_reason`, and usage hand-built into
//! a [`UsageBlock`] with the Anthropic token names mapped correctly.
//! Test: inline `tests` — `parses_text_and_native_usage`, `parses_tool_use_blocks`,
//! `maps_stop_reasons`, `ignores_unknown_block_types`, `parse_error_is_deserialise`.

use serde::Deserialize;
use serde_json::Value;

use crate::inference::error::InferenceError;
use crate::inference::types::{
    AssistantMessage, ChatChoice, ChatResponse, FunctionCall, ToolCall, UsageBlock,
};

/// Parse an Anthropic `/v1/messages` response body into the neutral
/// [`ChatResponse`].
///
/// Why: the adapter's `chat` must return the identical [`ChatResponse`] every
/// other provider yields, so downstream `first_text`/`first_tool_calls`/`usage`
/// accessors work unchanged regardless of backend.
/// What: deserialises the Anthropic envelope (surfacing a parse failure as
/// [`InferenceError::Deserialise`] with the raw body, never the API key) and
/// converts it via [`AnthropicResponse::into_chat_response`].
/// Test: `parses_text_and_native_usage`, `parse_error_is_deserialise`.
pub fn parse(text: &str) -> Result<ChatResponse, InferenceError> {
    let raw: AnthropicResponse =
        serde_json::from_str(text).map_err(|e| InferenceError::Deserialise {
            message: e.to_string(),
            body: text.to_string(),
        })?;
    Ok(raw.into_chat_response())
}

/// The top-level Anthropic Messages response envelope.
///
/// Why: a dedicated deserialisation target keeps the Anthropic wire-field names
/// (`content` blocks, `stop_reason`) isolated from the neutral response type.
/// What: `id`/`model` identity, the `content` block array, the `stop_reason`
/// string, and the native `usage` block.
/// Test: `parses_text_and_native_usage`.
#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    id: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: AnthropicUsage,
}

/// One Anthropic content block: text, a tool call, or an ignored other kind.
///
/// Why: Anthropic returns heterogeneous, internally-tagged content blocks;
/// modelling only the two we consume (plus a catch-all) keeps unknown future
/// block types — `thinking`, `redacted_thinking` — from failing the parse.
/// What: `Text` carries generated prose; `ToolUse` carries a function
/// invocation; `Other` absorbs any unrecognised `type`.
/// Test: `parses_tool_use_blocks`, `ignores_unknown_block_types`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    /// A generated text span.
    Text {
        /// The text content.
        #[serde(default)]
        text: String,
    },
    /// A model-emitted tool call.
    ToolUse {
        /// Opaque tool-call id (echoed as the `tool_result` `tool_use_id`).
        id: String,
        /// The invoked function name.
        name: String,
        /// The function arguments as a JSON object.
        #[serde(default)]
        input: Value,
    },
    /// Any other block type (e.g. `thinking`), ignored.
    #[serde(other)]
    Other,
}

/// The Anthropic-native `usage` block.
///
/// Why: Anthropic names its token counters `input_tokens`/`output_tokens` — NOT
/// the OpenAI `prompt_tokens`/`completion_tokens` the shared [`UsageBlock`]
/// deserialises. Parsing into this dedicated struct, then hand-mapping, is what
/// prevents the tokens silently deserialising to zero.
/// What: the four token counters Anthropic reports (cache fields share the
/// Anthropic-native names the [`UsageBlock`] already uses).
/// Test: `parses_text_and_native_usage`.
#[derive(Debug, Default, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
}

impl AnthropicResponse {
    /// Convert the parsed Anthropic response into the neutral [`ChatResponse`].
    ///
    /// Why: normalising here (rather than at each call site) keeps the block/
    /// stop-reason/usage remapping in one place and guarantees the returned shape
    /// is byte-for-byte the shape callers already handle.
    /// What: concatenates `Text` blocks into `content` (`None` when empty), maps
    /// each `ToolUse` into an OpenAI-style [`ToolCall`] (arguments re-serialised
    /// from the `input` object), maps `stop_reason` via [`map_stop_reason`], and
    /// HAND-builds the [`UsageBlock`] so `input_tokens`/`output_tokens` land in
    /// `prompt_tokens`/`completion_tokens` instead of deserialising to zero.
    /// Test: `parses_text_and_native_usage`, `parses_tool_use_blocks`.
    fn into_chat_response(self) -> ChatResponse {
        let mut text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        for block in self.content {
            match block {
                ContentBlock::Text { text: t } => text.push_str(&t),
                ContentBlock::ToolUse { id, name, input } => tool_calls.push(ToolCall {
                    id,
                    kind: "function".into(),
                    function: FunctionCall {
                        name,
                        arguments: serde_json::to_string(&input)
                            .unwrap_or_else(|_| "{}".to_string()),
                    },
                }),
                ContentBlock::Other => {}
            }
        }
        let content = if text.is_empty() { None } else { Some(text) };
        let finish_reason = self.stop_reason.as_deref().map(map_stop_reason);

        // Hand-construct the usage block: Anthropic's input/output token names do
        // not deserialise through the OpenAI-shaped `UsageBlock`, so map them
        // explicitly (cache fields already share the Anthropic-native names).
        let usage = UsageBlock {
            prompt_tokens: self.usage.input_tokens,
            completion_tokens: self.usage.output_tokens,
            total_tokens: self
                .usage
                .input_tokens
                .saturating_add(self.usage.output_tokens),
            cache_read_input_tokens: self.usage.cache_read_input_tokens,
            cache_creation_input_tokens: self.usage.cache_creation_input_tokens,
            prompt_tokens_details: None,
            cost: None,
        };

        ChatResponse {
            id: self.id,
            model: self.model,
            choices: vec![ChatChoice {
                message: AssistantMessage {
                    content,
                    tool_calls,
                },
                finish_reason,
            }],
            usage,
        }
    }
}

/// Map an Anthropic `stop_reason` to the neutral OpenAI-style `finish_reason`.
///
/// Why: [`crate::inference::ChatResponse::stop_reason`] parses the wire string
/// through [`crate::inference::StopReason::from_wire`], which speaks the OpenAI
/// vocabulary; translating here lets Anthropic responses drive the same typed
/// stop-condition branching as every other provider.
/// What: `end_turn`/`stop_sequence`→`stop`, `max_tokens`→`length`,
/// `tool_use`→`tool_calls`; any other value is preserved verbatim.
/// Test: `maps_stop_reasons`.
fn map_stop_reason(reason: &str) -> String {
    match reason {
        "end_turn" | "stop_sequence" => "stop".to_string(),
        "max_tokens" => "length".to_string(),
        "tool_use" => "tool_calls".to_string(),
        other => other.to_string(),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::types::StopReason;

    /// Why: text must extract into `content`, and — the critical trap — the
    /// Anthropic `input_tokens`/`output_tokens` must map to
    /// `prompt_tokens`/`completion_tokens` (NOT deserialise to zero), with cache
    /// counters flowing through.
    /// Test: itself.
    #[test]
    fn parses_text_and_native_usage() {
        let body = r#"{
          "id": "msg_1",
          "model": "claude-sonnet-4-5",
          "content": [{"type": "text", "text": "pong"}],
          "stop_reason": "end_turn",
          "usage": {
            "input_tokens": 12,
            "output_tokens": 3,
            "cache_read_input_tokens": 5,
            "cache_creation_input_tokens": 2
          }
        }"#;
        let resp = parse(body).expect("parse");
        assert_eq!(resp.id, "msg_1");
        assert_eq!(resp.first_text().as_deref(), Some("pong"));
        assert_eq!(resp.stop_reason(), Some(StopReason::Stop));
        let usage = resp.usage();
        assert_eq!(
            usage.prompt_tokens, 12,
            "input_tokens must not deserialise to 0"
        );
        assert_eq!(usage.completion_tokens, 3);
        assert_eq!(usage.cache_read_tokens, 5);
        assert_eq!(usage.cache_creation_tokens, 2);
        assert_eq!(usage.total_tokens(), 15);
    }

    /// Why: `tool_use` blocks must become OpenAI-style tool calls with the
    /// arguments re-serialised from the `input` object, and a `tool_use` stop
    /// reason maps to the tool-loop `finish_reason`.
    /// Test: itself.
    #[test]
    fn parses_tool_use_blocks() {
        let body = r#"{
          "id": "msg_2",
          "model": "claude-sonnet-4-5",
          "content": [
            {"type": "text", "text": "let me check"},
            {"type": "tool_use", "id": "toolu_9", "name": "get_weather",
             "input": {"loc": "SEA"}}
          ],
          "stop_reason": "tool_use",
          "usage": {"input_tokens": 20, "output_tokens": 8}
        }"#;
        let resp = parse(body).expect("parse");
        assert_eq!(resp.first_text().as_deref(), Some("let me check"));
        assert_eq!(resp.stop_reason(), Some(StopReason::ToolCalls));
        let calls = resp.first_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_9");
        assert_eq!(calls[0].kind, "function");
        assert_eq!(calls[0].function.name, "get_weather");
        let args: Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["loc"], "SEA");
    }

    /// Why: every known Anthropic stop reason must translate to its neutral
    /// equivalent, and an unknown one preserved.
    /// Test: itself.
    #[test]
    fn maps_stop_reasons() {
        assert_eq!(map_stop_reason("end_turn"), "stop");
        assert_eq!(map_stop_reason("stop_sequence"), "stop");
        assert_eq!(map_stop_reason("max_tokens"), "length");
        assert_eq!(map_stop_reason("tool_use"), "tool_calls");
        assert_eq!(map_stop_reason("refusal"), "refusal");
    }

    /// Why: unknown content-block types (`thinking`) must be ignored, not fail
    /// the parse, so a future block kind never breaks the adapter.
    /// Test: itself.
    #[test]
    fn ignores_unknown_block_types() {
        let body = r#"{
          "id": "msg_3",
          "content": [
            {"type": "thinking", "thinking": "hmm"},
            {"type": "text", "text": "answer"}
          ],
          "stop_reason": "end_turn",
          "usage": {"input_tokens": 1, "output_tokens": 1}
        }"#;
        let resp = parse(body).expect("parse");
        assert_eq!(resp.first_text().as_deref(), Some("answer"));
    }

    /// Why: a malformed body must surface as a classified `Deserialise` error
    /// carrying the raw body, never a panic.
    /// Test: itself.
    #[test]
    fn parse_error_is_deserialise() {
        let Err(err) = parse("{not json") else {
            panic!("expected a Deserialise error");
        };
        assert!(matches!(err, InferenceError::Deserialise { .. }));
    }
}
