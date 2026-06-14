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

use crate::chat::ToolDef;
use crate::ChatMessage;
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
#[derive(Debug, Serialize)]
pub(super) struct ChatRequestWire<'a> {
    pub(super) model: &'a str,
    pub(super) messages: &'a [ChatMessage],
    pub(super) stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tools: Option<Vec<OpenAiToolWire<'a>>>,
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
