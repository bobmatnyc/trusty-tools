//! OpenAI-compatible wire types and serialisation helpers.
//!
//! Why: the request-body wire structs (`ChatRequestWire`, `OpenAiToolWire`,
//! `OpenAiFunctionWire`) and the `tools_wire` mapper are shared by both the
//! OpenRouter and Ollama providers. Centralising them here avoids drift
//! between implementations and makes the wire format easy to audit in
//! isolation.
//! What: `ChatRequestWire`, `OpenAiToolWire`, `OpenAiFunctionWire`, and
//! `tools_wire` which maps a `ToolDef` slice into the OpenAI `tools` array.
//! Test: `tool_def_serializes_as_function`, `empty_tools_serializes_to_none`
//! in the parent module's test suite.

use crate::ChatMessage;
use crate::chat::ToolDef;
use serde::Serialize;

/// OpenAI wire shape for a single tool definition.
#[derive(Debug, Serialize)]
pub(super) struct OpenAiToolWire<'a> {
    #[serde(rename = "type")]
    pub(super) kind: &'static str,
    pub(super) function: OpenAiFunctionWire<'a>,
}

/// OpenAI wire shape for the function part of a tool definition.
#[derive(Debug, Serialize)]
pub(super) struct OpenAiFunctionWire<'a> {
    pub(super) name: &'a str,
    pub(super) description: &'a str,
    pub(super) parameters: &'a serde_json::Value,
}

/// OpenAI-compatible streaming chat request body.
///
/// Why: #3758 — the sampling fields below were missing, so a streamed turn
/// silently ran on provider defaults while the blocking path for the SAME turn
/// sent the caller's configured temperature, token ceiling, and stop
/// sequences. Every one of them is `skip_serializing_if`-gated, so a caller
/// that supplies nothing produces byte-identical JSON to the pre-#3758 body.
/// What: `stream` is always sent; `tools`, `temperature`, `max_tokens`, and
/// `stop` are omitted when absent (an empty array is NOT sent — some servers
/// reject `"stop": []`, mirroring the empty-`tools` trap).
/// Test: `sampling_params_serialize_into_request_body`,
/// `default_sampling_omits_fields`.
#[derive(Debug, Serialize)]
pub(super) struct ChatRequestWire<'a> {
    pub(super) model: &'a str,
    pub(super) messages: &'a [ChatMessage],
    pub(super) stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tools: Option<Vec<OpenAiToolWire<'a>>>,
    // #3758: sampling parity with the blocking path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) temperature: Option<f32>,
    // #3758: sampling parity with the blocking path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) max_tokens: Option<u32>,
    // #3758: OpenAI spells this `stop`; the direct-Anthropic dialect calls the
    // same parameter `stop_sequences`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) stop: Option<&'a [String]>,
}

/// Map a `ToolDef` slice to the OpenAI `tools` array, or `None` when empty.
///
/// Why: both OpenRouter and Ollama providers share this mapping; centralising
/// it avoids drift between implementations.
/// What: returns `Some(Vec<OpenAiToolWire>)` when the slice is non-empty,
/// `None` otherwise (callers skip the field entirely via `skip_serializing_if`).
/// Test: `tool_def_serializes_as_function`, `empty_tools_serializes_to_none`.
pub(super) fn tools_wire(tools: &[ToolDef]) -> Option<Vec<OpenAiToolWire<'_>>> {
    if tools.is_empty() {
        None
    } else {
        Some(
            tools
                .iter()
                .map(|t| OpenAiToolWire {
                    kind: "function",
                    function: OpenAiFunctionWire {
                        name: &t.name,
                        description: &t.description,
                        parameters: &t.parameters,
                    },
                })
                .collect(),
        )
    }
}
