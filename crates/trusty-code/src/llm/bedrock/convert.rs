//! `ChatRequest`/`ChatResponse` <-> AWS Bedrock Converse API conversion.
//!
//! Why: `BedrockChatClient` (in `super::mod`) must speak the same
//! `ChatRequest`/`ChatResponse` wire-neutral types every other transport
//! uses, so `AgentLoop` and the rest of trusty-code never branch on the
//! backend. This module is the entire translation seam: tcode's
//! OpenAI-shaped messages/tools/tool_choice become Converse's
//! `Message`/`Tool`/`ToolChoice` request shapes, and a Converse response
//! becomes a `ChatResponse` indistinguishable (to callers) from an
//! OpenRouter one.
//! What: [`build_converse_messages`] walks `ChatRequest.messages`, splitting
//! `system`-role entries into Converse's separate system-prompt array and
//! merging consecutive same-role turns (Converse requires strict
//! user/assistant alternation; tcode's `tool`-role results are Converse
//! `ToolResult` blocks nested inside a `user`-role message).
//! [`build_tool_config`] maps `ChatRequest.tools` + `ChatRequest.tool_choice`
//! into a Converse `ToolConfiguration`, interpreting `tool_choice` generically
//! (OpenAI-shaped strings/objects, and the Converse-shaped JSON
//! `BedrockProvider::map_tool_choice` produces) so either input flows
//! correctly. [`converse_output_to_chat_response`] maps the Converse
//! response's content blocks and token usage back into `ChatResponse`.
//! Test: `bedrock::tests::*` (this module has no inline tests — see the
//! module-level SLOC-cap convention already used by
//! `trusty-review::llm::bedrock`).

use std::collections::HashMap;

use aws_sdk_bedrockruntime::operation::converse::ConverseOutput as ConverseResponse;
use aws_sdk_bedrockruntime::types::{
    AnyToolChoice, AutoToolChoice, ContentBlock, ConversationRole, Message, SpecificToolChoice,
    SystemContentBlock, Tool, ToolChoice as SdkToolChoice, ToolConfiguration, ToolInputSchema,
    ToolResultBlock, ToolResultContentBlock, ToolSpecification, ToolUseBlock,
};
use aws_smithy_types::{Document, Number};
use serde_json::Value;

use super::super::{
    AssistantMessage, ChatChoice, ChatMessage, ChatRequest, ChatResponse, FunctionCall, LlmError,
    ToolCall, ToolDefinition, UsageBlock,
};

/// Split `req.messages` into Converse's system-prompt array and the
/// alternating user/assistant message list.
///
/// Why: Converse rejects a message list that doesn't strictly alternate
/// `user`/`assistant` roles, and has no `system`-role message type — system
/// content is a separate top-level field. tcode's transcript can contain
/// several `system`-role entries (the seed system prompt, plus any
/// compaction-summary messages, #2070) and consecutive `tool`-role results
/// answering a multi-tool-call turn; both must be handled without violating
/// alternation.
/// What: `system`-role entries become `SystemContentBlock::Text` (appended in
/// order, wherever they occur — Converse's system array has no per-turn
/// position semantics). Every other message maps to Converse role `User`
/// (`user` and `tool` roles) or `Assistant` (`assistant` role); consecutive
/// messages of the same Converse role are merged into ONE `Message` with
/// multiple content blocks, which is exactly what happens for a run of
/// `tool`-role results answering one assistant turn's tool calls.
/// Test: `bedrock::tests::*`.
pub(super) fn build_converse_messages(
    req: &ChatRequest,
) -> Result<(Vec<SystemContentBlock>, Vec<Message>), LlmError> {
    let mut system_blocks = Vec::new();
    let mut messages = Vec::new();
    let mut current_role: Option<ConversationRole> = None;
    let mut current_blocks: Vec<ContentBlock> = Vec::new();

    for msg in &req.messages {
        if msg.role == "system" {
            if let Some(text) = &msg.content {
                system_blocks.push(SystemContentBlock::Text(text.clone()));
            }
            continue;
        }

        let role = if msg.role == "assistant" {
            ConversationRole::Assistant
        } else {
            ConversationRole::User
        };

        if current_role.as_ref() != Some(&role) {
            flush_message(&mut messages, &mut current_role, &mut current_blocks)?;
            current_role = Some(role);
        }

        append_content_blocks(msg, &mut current_blocks)?;
    }

    flush_message(&mut messages, &mut current_role, &mut current_blocks)?;
    Ok((system_blocks, messages))
}

/// Append one `ChatMessage`'s content as Converse `ContentBlock`s onto the
/// in-progress same-role batch.
///
/// Why: `user`, `tool`, and `assistant` messages each carry their payload in
/// a different `ChatMessage` shape (plain text; a tool result keyed by
/// `tool_call_id`; text plus zero or more tool calls) that must become the
/// matching Converse content-block variant.
/// What: `assistant` -> a `Text` block for non-empty `content`, plus one
/// `ToolUse` block per `tool_calls` entry (arguments parsed from the OpenAI
/// JSON-string shape into a `Document`). `tool` -> one `ToolResult` block
/// keyed by `tool_call_id`. Anything else (`user`) -> a `Text` block for
/// `content` when present.
/// Test: `bedrock::tests::*`.
fn append_content_blocks(
    msg: &ChatMessage,
    blocks: &mut Vec<ContentBlock>,
) -> Result<(), LlmError> {
    match msg.role.as_str() {
        "assistant" => {
            if let Some(text) = &msg.content
                && !text.is_empty()
            {
                blocks.push(ContentBlock::Text(text.clone()));
            }
            if let Some(calls) = &msg.tool_calls {
                for call in calls {
                    blocks.push(ContentBlock::ToolUse(tool_use_block(call)?));
                }
            }
        }
        "tool" => {
            let tool_use_id = msg.tool_call_id.clone().unwrap_or_default();
            let content_text = msg.content.clone().unwrap_or_default();
            let result = ToolResultBlock::builder()
                .tool_use_id(tool_use_id)
                .content(ToolResultContentBlock::Text(content_text))
                .build()
                .map_err(|e| LlmError::Bedrock(format!("build ToolResultBlock: {e}")))?;
            blocks.push(ContentBlock::ToolResult(result));
        }
        _ => {
            if let Some(text) = &msg.content {
                blocks.push(ContentBlock::Text(text.clone()));
            }
        }
    }
    Ok(())
}

/// Build one Converse `ToolUseBlock` from an assistant turn's `ToolCall`.
///
/// Why: Re-sending prior assistant tool calls as history requires the exact
/// `tool_use_id`/`name`/`input` triple Converse expects — `input` as a
/// `Document`, not the OpenAI JSON-string shape tcode stores it in.
/// What: Parses `call.function.arguments` as JSON (falling back to an empty
/// object on parse failure — a malformed prior call must not abort the whole
/// request) and converts it to a `Document` via [`json_to_document`].
/// Test: `bedrock::tests::*`.
fn tool_use_block(call: &ToolCall) -> Result<ToolUseBlock, LlmError> {
    let parsed: Value = serde_json::from_str(&call.function.arguments)
        .unwrap_or_else(|_| Value::Object(Default::default()));
    let input = json_to_document(&parsed).unwrap_or_else(|| Document::Object(HashMap::new()));
    ToolUseBlock::builder()
        .tool_use_id(&call.id)
        .name(&call.function.name)
        .input(input)
        .build()
        .map_err(|e| LlmError::Bedrock(format!("build ToolUseBlock: {e}")))
}

/// Flush the in-progress same-role content-block batch into `messages`.
///
/// Why: Shared by the per-message loop (on a role change) and the final
/// call after the loop ends, so the "merge consecutive same-role turns"
/// logic lives in exactly one place.
/// What: No-op when `blocks` is empty; otherwise builds one `Message` from
/// the accumulated blocks and role, pushes it, and clears both.
fn flush_message(
    messages: &mut Vec<Message>,
    current_role: &mut Option<ConversationRole>,
    blocks: &mut Vec<ContentBlock>,
) -> Result<(), LlmError> {
    if blocks.is_empty() {
        return Ok(());
    }
    let role = current_role.take().unwrap_or(ConversationRole::User);
    let message = Message::builder()
        .role(role)
        .set_content(Some(std::mem::take(blocks)))
        .build()
        .map_err(|e| LlmError::Bedrock(format!("build Message: {e}")))?;
    messages.push(message);
    Ok(())
}

/// The provider-neutral decision `interpret_tool_choice` resolves
/// `ChatRequest.tool_choice` to.
///
/// Why: `ChatRequest.tool_choice` is a generic `serde_json::Value` so every
/// transport can share the field; this enum is the Bedrock transport's own
/// intermediate representation before building the SDK's `ToolChoice`.
/// What: Mirrors Converse's three real choices plus `Suppress` (tcode's/
/// OpenAI's `"none"`, which Converse has no equivalent for — the transport
/// omits `toolConfig` entirely instead).
#[derive(Debug, PartialEq, Eq)]
enum ToolChoiceDecision {
    Auto,
    Any,
    Named(String),
    Suppress,
}

/// Interpret `ChatRequest.tool_choice` generically across both wire shapes
/// this transport may see.
///
/// Why: `agent_loop::build_request` currently sends the bare OpenAI string
/// `"auto"` for every provider (#1021 doesn't rewire that), while
/// [`super::super::super::provider::BedrockProvider::map_tool_choice`]
/// produces the Converse-shaped JSON (`{"auto":{}}`, `{"any":{}}`,
/// `{"tool":{"name":...}}`) for direct callers/tests. Interpreting both here
/// means the transport is correct today AND once `build_request` is wired to
/// call the provider's mapping.
/// What: `None` (or an unrecognised shape) defaults to `Auto` — the safe,
/// permissive default. `"auto"`/`"required"`/`"any"`/`"none"` strings map
/// per the OpenAI convention (`"required"` and Converse's `"any"` naming
/// both mean "must call some tool"). A JSON object naming a tool under
/// either `tool.name` (Converse shape) or `function.name` (OpenAI
/// function-selector shape) maps to [`ToolChoiceDecision::Named`].
/// Test: `bedrock::tests::*`.
fn interpret_tool_choice(value: Option<&Value>) -> ToolChoiceDecision {
    let Some(value) = value else {
        return ToolChoiceDecision::Auto;
    };
    match value {
        Value::String(s) => match s.as_str() {
            "auto" => ToolChoiceDecision::Auto,
            "required" | "any" => ToolChoiceDecision::Any,
            "none" => ToolChoiceDecision::Suppress,
            _ => ToolChoiceDecision::Auto,
        },
        Value::Object(map) => {
            if let Some(name) = map
                .get("tool")
                .and_then(|t| t.get("name"))
                .and_then(Value::as_str)
            {
                return ToolChoiceDecision::Named(name.to_string());
            }
            if let Some(name) = map
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
            {
                return ToolChoiceDecision::Named(name.to_string());
            }
            if map.contains_key("any") {
                return ToolChoiceDecision::Any;
            }
            ToolChoiceDecision::Auto
        }
        _ => ToolChoiceDecision::Auto,
    }
}

/// Build a Converse `ToolConfiguration` from `ChatRequest.tools`/`tool_choice`.
///
/// Why: The Converse request only carries a `toolConfig` at all when tools
/// are being offered; an empty tool list or a `"none"`-equivalent choice
/// must omit it entirely rather than sending an empty/meaningless config.
/// What: Returns `Ok(None)` when `tools` is empty or `tool_choice` resolves
/// to [`ToolChoiceDecision::Suppress`]; otherwise builds one `Tool::ToolSpec`
/// per [`ToolDefinition`] (JSON-Schema `parameters` converted to a
/// `Document` via [`json_to_document`]; a missing schema falls back to an
/// empty object schema) plus the resolved `ToolChoice`.
/// Test: `bedrock::tests::*`.
pub(super) fn build_tool_config(
    tools: &[ToolDefinition],
    tool_choice: Option<&Value>,
) -> Result<Option<ToolConfiguration>, LlmError> {
    if tools.is_empty() {
        return Ok(None);
    }

    let decision = interpret_tool_choice(tool_choice);
    if decision == ToolChoiceDecision::Suppress {
        return Ok(None);
    }

    let mut builder = ToolConfiguration::builder();
    for def in tools {
        let schema = def
            .function
            .parameters
            .clone()
            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
        let doc = json_to_document(&schema).ok_or_else(|| {
            LlmError::Bedrock(format!(
                "tool {:?} parameters must be a JSON object",
                def.function.name
            ))
        })?;
        let spec = ToolSpecification::builder()
            .name(&def.function.name)
            .set_description(def.function.description.clone())
            .input_schema(ToolInputSchema::Json(doc))
            .build()
            .map_err(|e| LlmError::Bedrock(format!("build ToolSpecification: {e}")))?;
        builder = builder.tools(Tool::ToolSpec(spec));
    }

    let sdk_choice = match decision {
        ToolChoiceDecision::Auto => SdkToolChoice::Auto(AutoToolChoice::builder().build()),
        ToolChoiceDecision::Any => SdkToolChoice::Any(AnyToolChoice::builder().build()),
        ToolChoiceDecision::Named(name) => SdkToolChoice::Tool(
            SpecificToolChoice::builder()
                .name(name)
                .build()
                .map_err(|e| LlmError::Bedrock(format!("build SpecificToolChoice: {e}")))?,
        ),
        ToolChoiceDecision::Suppress => unreachable!("handled above"),
    };
    builder = builder.tool_choice(sdk_choice);

    builder
        .build()
        .map(Some)
        .map_err(|e| LlmError::Bedrock(format!("build ToolConfiguration: {e}")))
}

/// Map a Converse response into tcode's provider-neutral `ChatResponse`.
///
/// Why: Every downstream consumer (`AgentLoop`, `Transcript`, `PerfCollector`
/// via `Provider::extract_usage`) speaks `ChatResponse`; this is the single
/// place a Converse response becomes indistinguishable from an OpenRouter one.
/// What: Joins `Text` content blocks (newline-separated) into
/// `AssistantMessage.content`; maps each `ToolUse` block into a `ToolCall`
/// (arguments re-serialised to the OpenAI JSON-string shape via
/// [`document_to_json_string`]); `finish_reason` is the raw, lowercased
/// Converse `stopReason` string; `usage` maps `TokenUsage`'s
/// input/output/cache fields into `UsageBlock` (OpenRouter-only fields —
/// `prompt_tokens_details`, `cost` — stay `None`, matching the
/// direct/Bedrock path's existing `UsageBlock` contract).
/// Test: `bedrock::tests::*`.
pub(super) fn converse_output_to_chat_response(
    output: &ConverseResponse,
    requested_model: &str,
) -> ChatResponse {
    let mut text = String::new();
    let mut tool_calls = Vec::new();

    if let Some(msg) = output.output().and_then(|o| o.as_message().ok()) {
        for block in msg.content() {
            match block {
                ContentBlock::Text(t) => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(t);
                }
                ContentBlock::ToolUse(tu) => {
                    let arguments =
                        document_to_json_string(tu.input()).unwrap_or_else(|| "{}".to_string());
                    tool_calls.push(ToolCall {
                        id: tu.tool_use_id().to_string(),
                        kind: "function".to_string(),
                        function: FunctionCall {
                            name: tu.name().to_string(),
                            arguments,
                        },
                    });
                }
                _ => {}
            }
        }
    }

    let content = if text.is_empty() { None } else { Some(text) };
    let finish_reason = Some(output.stop_reason().as_str().to_ascii_lowercase());

    let usage = output
        .usage()
        .map(|u| UsageBlock {
            prompt_tokens: u.input_tokens().max(0) as u32,
            completion_tokens: u.output_tokens().max(0) as u32,
            total_tokens: u.total_tokens().max(0) as u32,
            cache_read_input_tokens: u.cache_read_input_tokens().unwrap_or(0).max(0) as u32,
            cache_creation_input_tokens: u.cache_write_input_tokens().unwrap_or(0).max(0) as u32,
            prompt_tokens_details: None,
            cost: None,
        })
        .unwrap_or_default();

    ChatResponse {
        id: String::new(),
        model: requested_model.to_string(),
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

// ─── JSON <-> Document conversion ──────────────────────────────────────────

/// Convert a `serde_json::Value` object to an AWS smithy `Document`.
///
/// Why: Bedrock's `ToolInputSchema::Json` and `ToolUseBlock.input` both take
/// a `Document`; tcode carries JSON Schemas and tool arguments as
/// `serde_json::Value`/JSON strings.
/// What: Recursively converts the value tree. Returns `None` when the
/// top-level value is not a JSON object (Bedrock requires an object at the
/// top level for both a tool's input schema and its arguments).
/// Test: `bedrock::tests::*`.
pub(super) fn json_to_document(value: &Value) -> Option<Document> {
    match value {
        Value::Object(map) => Some(Document::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), json_value_to_doc(v)))
                .collect(),
        )),
        _ => None,
    }
}

fn json_value_to_doc(v: &Value) -> Document {
    match v {
        Value::Null => Document::Null,
        Value::Bool(b) => Document::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Document::Number(Number::NegInt(i))
            } else if let Some(u) = n.as_u64() {
                Document::Number(Number::PosInt(u))
            } else {
                Document::Number(Number::Float(n.as_f64().unwrap_or(0.0)))
            }
        }
        Value::String(s) => Document::String(s.clone()),
        Value::Array(arr) => Document::Array(arr.iter().map(json_value_to_doc).collect()),
        Value::Object(map) => Document::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), json_value_to_doc(v)))
                .collect(),
        ),
    }
}

/// Convert an AWS smithy `Document` to a JSON string.
///
/// Why: `ToolUseBlock.input` is a `Document`; tcode's `ToolCall.function.arguments`
/// is a JSON string (the OpenAI wire shape every other transport already uses),
/// so a Bedrock tool call must be re-serialised into that shape for the rest
/// of the pipeline (schema validation, tool dispatch) to consume unchanged.
/// What: Recursively converts to `serde_json::Value`, then serialises.
/// Returns `None` only on a (should-be-unreachable) serialisation failure.
/// Test: `bedrock::tests::*`.
pub(super) fn document_to_json_string(doc: &Document) -> Option<String> {
    serde_json::to_string(&doc_to_json_value(doc)).ok()
}

fn doc_to_json_value(doc: &Document) -> Value {
    match doc {
        Document::Null => Value::Null,
        Document::Bool(b) => Value::Bool(*b),
        Document::Number(n) => match n {
            Number::PosInt(u) => Value::Number((*u).into()),
            Number::NegInt(i) => Value::Number((*i).into()),
            Number::Float(f) => serde_json::Number::from_f64(*f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        },
        Document::String(s) => Value::String(s.clone()),
        Document::Array(arr) => Value::Array(arr.iter().map(doc_to_json_value).collect()),
        Document::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), doc_to_json_value(v)))
                .collect(),
        ),
    }
}
