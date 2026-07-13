//! [`ChatRequest`]/[`ChatResponse`] <-> AWS Bedrock Converse API conversion.
//!
//! Why: [`super::BedrockAdapter`] must speak the same provider-neutral
//! [`ChatRequest`]/[`ChatResponse`] types every other [`crate::inference::InferenceAdapter`]
//! uses, so callers never branch on the backend. This module is the entire
//! translation seam: the OpenAI-shaped messages/tools/tool_choice become
//! Converse's `Message`/`Tool`/`ToolChoice` request shapes, and a Converse
//! response becomes a [`ChatResponse`] indistinguishable (to callers) from an
//! OpenRouter one. Ported byte-for-byte from tcode's proven `llm::bedrock::convert`
//! (#2407) — only the type family (tcode wire types → `crate::inference::types`)
//! and the error type ([`LlmError::Bedrock`] → [`InferenceError::Provider`])
//! change, so the M3 bake-off L1+L2 semantics are preserved.
//! What: [`build_converse_messages`] walks `ChatRequest.messages`, splitting
//! `system`-role entries into Converse's separate system-prompt array and
//! merging consecutive same-role turns (Converse requires strict user/assistant
//! alternation; `tool`-role results are Converse `ToolResult` blocks nested
//! inside a `user`-role message). [`build_tool_config`] maps `ChatRequest.tools`
//! and `ChatRequest.tool_choice` into a Converse `ToolConfiguration`, interpreting
//! `tool_choice` generically (OpenAI-shaped strings/objects, and the
//! Converse-shaped JSON [`super::BedrockAdapter::map_tool_choice`] produces) so
//! either input flows correctly. [`converse_output_to_chat_response`] maps the
//! Converse response's content blocks and token usage back into [`ChatResponse`].
//! (#2260) Both [`build_converse_messages`] and [`build_tool_config`] also
//! translate `cache_control` markers into Bedrock-native `cachePoint` blocks via
//! [`super::cache`] — see that module's doc for the wire-shape translation and
//! minimum-size guard.
//! Test: `super::tests::*` (this module has no inline tests — kept below the
//! 500-SLOC production cap by co-locating tests in the sibling `tests.rs`).

use std::collections::{HashMap, HashSet};

use aws_sdk_bedrockruntime::operation::converse::ConverseOutput as ConverseResponse;
use aws_sdk_bedrockruntime::types::{
    AnyToolChoice, AutoToolChoice, ContentBlock, ConversationRole, Message, SpecificToolChoice,
    SystemContentBlock, Tool, ToolChoice as SdkToolChoice, ToolConfiguration, ToolInputSchema,
    ToolResultBlock, ToolResultContentBlock, ToolResultStatus, ToolSpecification, ToolUseBlock,
};
use aws_smithy_types::{Document, Number};
use serde_json::Value;

use super::cache;
use crate::inference::error::InferenceError;
use crate::inference::types::{
    AssistantMessage, ChatChoice, ChatMessage, ChatRequest, ChatResponse, FunctionCall, ToolCall,
    ToolDefinition, UsageBlock,
};

/// Split `req.messages` into Converse's system-prompt array and the alternating
/// user/assistant message list.
///
/// Why: Converse rejects a message list that doesn't strictly alternate
/// `user`/`assistant` roles, and has no `system`-role message type — system
/// content is a separate top-level field. A transcript can contain several
/// `system`-role entries (the seed system prompt, plus any compaction-summary
/// messages) and consecutive `tool`-role results answering a multi-tool-call
/// turn; both must be handled without violating alternation.
/// What: `system`-role entries become `SystemContentBlock::Text` (appended in
/// order, wherever they occur — Converse's system array has no per-turn position
/// semantics). Every other message maps to Converse role `User` (`user` and
/// `tool` roles) or `Assistant` (`assistant` role); consecutive messages of the
/// same Converse role are merged into ONE `Message` with multiple content
/// blocks, which is exactly what happens for a run of `tool`-role results
/// answering one assistant turn's tool calls. (#2278) Before the final return,
/// [`enforce_tool_pairing`] rewrites `messages` so the emitted request always
/// satisfies Bedrock's ToolUse/ToolResult pairing invariant, regardless of what
/// produced the input history.
/// Test: `super::tests::*`.
pub(super) fn build_converse_messages(
    req: &ChatRequest,
) -> Result<(Vec<SystemContentBlock>, Vec<Message>), InferenceError> {
    let mut system_blocks = Vec::new();
    let mut messages = Vec::new();
    let mut current_role: Option<ConversationRole> = None;
    let mut current_blocks: Vec<ContentBlock> = Vec::new();

    for msg in &req.messages {
        if msg.role == "system" {
            // (#2278) Flush the in-progress same-role batch BEFORE diverting
            // this entry to the system-prompt array, rather than bare
            // `continue`. A mid-conversation system-role entry (e.g. a
            // compaction-summary placeholder) has no in-conversation home on
            // Converse either way — the system array has no per-turn position
            // semantics — but failing to flush here silently merges content
            // that occurred BEFORE this entry with content that occurs AFTER it
            // into one artificial message, corrupting ordering beyond just
            // losing the placeholder text.
            flush_message(&mut messages, &mut current_role, &mut current_blocks)?;
            if let Some(text) = &msg.content {
                system_blocks.push(SystemContentBlock::Text(text.clone()));
                // (#2260) A cache-eligible run marks exactly one system-role
                // entry (the seed system prompt) with `cache_control`; translate
                // that marker into a Bedrock-native cachePoint immediately after
                // its Text block, guarded by the minimum-size floor so a
                // too-small prompt never wastes a checkpoint slot.
                if msg.cache_control.is_some() && cache::system_cacheable(text) {
                    system_blocks.push(SystemContentBlock::CachePoint(cache::cache_point_block()));
                }
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

        // (rolling-history follow-up to #2260) A cache-eligible run marks the
        // last two non-system messages with `cache_control`; translate that
        // marker into a trailing Bedrock-native cachePoint content block for
        // THIS message, appended after its own content so it lands inside the
        // right (possibly same-role-merged) `Message` once `flush_message`
        // runs. Unlike the system/tools breakpoints, no minimum-size floor is
        // applied here — the history segments this marks are, by construction,
        // the dominant token cost, never a too-small fixture.
        if msg.cache_control.is_some() {
            current_blocks.push(ContentBlock::CachePoint(cache::cache_point_block()));
        }
    }

    flush_message(&mut messages, &mut current_role, &mut current_blocks)?;
    enforce_tool_pairing(&mut messages)?;
    Ok((system_blocks, messages))
}

/// Enforce Bedrock's ToolUse/ToolResult pairing invariant on the fully
/// role-merged Converse message list (#2278 Fix B — the Bedrock-specific
/// backstop).
///
/// Why: a count-based compaction cutoff can fold an assistant's `tool_calls`
/// entry into a summary while one of its answering `tool`-role entries survives
/// verbatim, or vice versa. Any unpaired `ToolUse`/`ToolResult` in the request
/// makes Bedrock's Converse API reject the whole call with `ValidationException`.
/// This runs unconditionally on every request as a last-resort guarantee that
/// the WIRE shape is always valid, independent of whatever produced the input
/// history.
/// What: two passes over `messages` (already strictly alternating
/// `User`/`Assistant`). Pass 1 walks every content block in encounter order,
/// tracking every `tool_use_id` introduced by a `ToolUse` block; a `ToolResult`
/// whose id was never introduced by an earlier (or same, block-order) `ToolUse`
/// is an orphan and is dropped. Pass 2 walks every `ToolUse` and, for each whose
/// id has no matching `ToolResult` in the immediately-following message,
/// synthesizes a placeholder `ToolResult` (status `Error`) appended to that
/// following message — or, if there is no following message, a brand-new
/// trailing `User` message carrying just the placeholders.
/// Test: `super::tests::enforce_tool_pairing_*`.
fn enforce_tool_pairing(messages: &mut Vec<Message>) -> Result<(), InferenceError> {
    // Pass 1: drop orphan ToolResult blocks; record every tool_use_id ever
    // introduced, in block encounter order.
    let mut introduced: HashSet<String> = HashSet::new();
    for msg in messages.iter_mut() {
        let role = msg.role().clone();
        let mut kept = Vec::with_capacity(msg.content().len());
        for block in msg.content() {
            match block {
                ContentBlock::ToolUse(tu) => {
                    introduced.insert(tu.tool_use_id().to_string());
                    kept.push(block.clone());
                }
                ContentBlock::ToolResult(tr) if !introduced.contains(tr.tool_use_id()) => {
                    // Orphan: no ToolUse ever introduced this id. Silently
                    // drop — the model has nothing to attach it to.
                }
                other => kept.push(other.clone()),
            }
        }
        *msg = Message::builder()
            .role(role)
            .set_content(Some(kept))
            .build()
            .map_err(|e| InferenceError::Provider(format!("rebuild Message: {e}")))?;
    }

    // Pass 2: for each ToolUse, compute any missing answers in the
    // immediately-following message up front (before mutating anything), so
    // index arithmetic below can't be invalidated by an earlier insertion.
    let mut missing_by_index: Vec<Vec<String>> = vec![Vec::new(); messages.len()];
    for (i, msg) in messages.iter().enumerate() {
        let own_ids: Vec<String> = msg
            .content()
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse(tu) => Some(tu.tool_use_id().to_string()),
                _ => None,
            })
            .collect();
        if own_ids.is_empty() {
            continue;
        }
        let answered: HashSet<&str> = messages
            .get(i + 1)
            .map(|next| {
                next.content()
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolResult(tr) => Some(tr.tool_use_id()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        missing_by_index[i] = own_ids
            .into_iter()
            .filter(|id| !answered.contains(id.as_str()))
            .collect();
    }

    // Apply back-to-front: inserting a brand-new message at i+1 must never
    // shift an index this loop still needs to visit (all remaining indices
    // are < i+1).
    for i in (0..messages.len()).rev() {
        if missing_by_index[i].is_empty() {
            continue;
        }
        let placeholders = missing_by_index[i]
            .iter()
            .map(|id| placeholder_tool_result(id))
            .collect::<Result<Vec<_>, _>>()?;

        let append_to_next = messages
            .get(i + 1)
            .is_some_and(|next| next.role() == &ConversationRole::User);
        if append_to_next {
            let mut content = messages[i + 1].content().to_vec();
            content.extend(placeholders);
            messages[i + 1] = Message::builder()
                .role(ConversationRole::User)
                .set_content(Some(content))
                .build()
                .map_err(|e| InferenceError::Provider(format!("rebuild Message: {e}")))?;
        } else {
            let new_msg = Message::builder()
                .role(ConversationRole::User)
                .set_content(Some(placeholders))
                .build()
                .map_err(|e| InferenceError::Provider(format!("build placeholder Message: {e}")))?;
            messages.insert(i + 1, new_msg);
        }
    }

    Ok(())
}

/// Build one synthesized placeholder `ToolResult` block for a `tool_use_id` that
/// reached the end of its answering window with no real result.
///
/// Why: Bedrock requires every `ToolUse` to have a matching `ToolResult`; when
/// the real result was dropped (compaction splitting a turn group, or any other
/// history-drift source), the model still needs SOME result to keep the
/// conversation valid — an explicit "unavailable" marker is honest about what
/// happened, unlike silently fabricating a fake success.
/// What: a `ToolResultBlock` keyed by `tool_use_id`, `status: Error`, and a
/// single text content block saying the result is unavailable.
/// Test: `super::tests::enforce_tool_pairing_synthesizes_placeholder_for_unanswered_tool_use`.
fn placeholder_tool_result(tool_use_id: &str) -> Result<ContentBlock, InferenceError> {
    let block = ToolResultBlock::builder()
        .tool_use_id(tool_use_id)
        .content(ToolResultContentBlock::Text(
            "[result unavailable — history was compacted]".to_string(),
        ))
        .status(ToolResultStatus::Error)
        .build()
        .map_err(|e| InferenceError::Provider(format!("build placeholder ToolResultBlock: {e}")))?;
    Ok(ContentBlock::ToolResult(block))
}

/// Append one `ChatMessage`'s content as Converse `ContentBlock`s onto the
/// in-progress same-role batch.
///
/// Why: `user`, `tool`, and `assistant` messages each carry their payload in a
/// different `ChatMessage` shape (plain text; a tool result keyed by
/// `tool_call_id`; text plus zero or more tool calls) that must become the
/// matching Converse content-block variant.
/// What: `assistant` -> a `Text` block for non-empty `content`, plus one
/// `ToolUse` block per `tool_calls` entry (arguments parsed from the OpenAI
/// JSON-string shape into a `Document`). `tool` -> one `ToolResult` block keyed
/// by `tool_call_id`. Anything else (`user`) -> a `Text` block for `content`
/// when present.
/// Test: `super::tests::*`.
fn append_content_blocks(
    msg: &ChatMessage,
    blocks: &mut Vec<ContentBlock>,
) -> Result<(), InferenceError> {
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
                .map_err(|e| InferenceError::Provider(format!("build ToolResultBlock: {e}")))?;
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
/// Why: re-sending prior assistant tool calls as history requires the exact
/// `tool_use_id`/`name`/`input` triple Converse expects — `input` as a
/// `Document`, not the OpenAI JSON-string shape.
/// What: parses `call.function.arguments` as JSON (falling back to an empty
/// object on parse failure — a malformed prior call must not abort the whole
/// request) and converts it to a `Document` via [`json_to_document`].
/// Test: `super::tests::*`.
fn tool_use_block(call: &ToolCall) -> Result<ToolUseBlock, InferenceError> {
    let parsed: Value = serde_json::from_str(&call.function.arguments)
        .unwrap_or_else(|_| Value::Object(Default::default()));
    let input = json_to_document(&parsed).unwrap_or_else(|| Document::Object(HashMap::new()));
    ToolUseBlock::builder()
        .tool_use_id(&call.id)
        .name(&call.function.name)
        .input(input)
        .build()
        .map_err(|e| InferenceError::Provider(format!("build ToolUseBlock: {e}")))
}

/// Flush the in-progress same-role content-block batch into `messages`.
///
/// Why: shared by the per-message loop (on a role change) and the final call
/// after the loop ends, so the "merge consecutive same-role turns" logic lives
/// in exactly one place.
/// What: no-op when `blocks` is empty; otherwise builds one `Message` from the
/// accumulated blocks and role, pushes it, and clears both.
/// Test: exercised by every `super::tests::build_converse_messages_*`.
fn flush_message(
    messages: &mut Vec<Message>,
    current_role: &mut Option<ConversationRole>,
    blocks: &mut Vec<ContentBlock>,
) -> Result<(), InferenceError> {
    if blocks.is_empty() {
        return Ok(());
    }
    let role = current_role.take().unwrap_or(ConversationRole::User);
    let message = Message::builder()
        .role(role)
        .set_content(Some(std::mem::take(blocks)))
        .build()
        .map_err(|e| InferenceError::Provider(format!("build Message: {e}")))?;
    messages.push(message);
    Ok(())
}

/// The provider-neutral decision [`interpret_tool_choice`] resolves
/// `ChatRequest.tool_choice` to.
///
/// Why: `ChatRequest.tool_choice` is a generic `serde_json::Value` so every
/// adapter can share the field; this enum is the Bedrock adapter's own
/// intermediate representation before building the SDK's `ToolChoice`.
/// What: mirrors Converse's three real choices plus `Suppress` (OpenAI's
/// `"none"`, which Converse has no equivalent for — the adapter omits
/// `toolConfig` entirely instead).
/// Test: exercised via `super::tests::build_tool_config_*`.
#[derive(Debug, PartialEq, Eq)]
enum ToolChoiceDecision {
    Auto,
    Any,
    Named(String),
    Suppress,
}

/// Interpret `ChatRequest.tool_choice` generically across both wire shapes this
/// adapter may see.
///
/// Why: a caller may send the bare OpenAI string `"auto"` for every provider, or
/// the Converse-shaped JSON that [`super::BedrockAdapter::map_tool_choice`]
/// produces (`{"auto":{}}`, `{"any":{}}`, `{"tool":{"name":...}}`). Interpreting
/// both here means the adapter is correct regardless of which the caller sends.
/// What: `None` (or an unrecognised shape) defaults to `Auto` — the safe,
/// permissive default. `"auto"`/`"required"`/`"any"`/`"none"` strings map per the
/// OpenAI convention (`"required"` and Converse's `"any"` naming both mean "must
/// call some tool"). A JSON object naming a tool under either `tool.name`
/// (Converse shape) or `function.name` (OpenAI function-selector shape) maps to
/// [`ToolChoiceDecision::Named`].
/// Test: `super::tests::build_tool_config_*`.
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
/// Why: the Converse request only carries a `toolConfig` at all when tools are
/// being offered; an empty tool list or a `"none"`-equivalent choice must omit
/// it entirely rather than sending an empty/meaningless config.
/// What: returns `Ok(None)` when `tools` is empty or `tool_choice` resolves to
/// [`ToolChoiceDecision::Suppress`]; otherwise builds one `Tool::ToolSpec` per
/// [`ToolDefinition`] (JSON-Schema `parameters` converted to a `Document` via
/// [`json_to_document`]; a missing schema falls back to an empty object schema)
/// plus the resolved `ToolChoice`.
/// Test: `super::tests::build_tool_config_*`.
pub(super) fn build_tool_config(
    tools: &[ToolDefinition],
    tool_choice: Option<&Value>,
) -> Result<Option<ToolConfiguration>, InferenceError> {
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
            InferenceError::Provider(format!(
                "tool {:?} parameters must be a JSON object",
                def.function.name
            ))
        })?;
        let spec = ToolSpecification::builder()
            .name(&def.function.name)
            .set_description(def.function.description.clone())
            .input_schema(ToolInputSchema::Json(doc))
            .build()
            .map_err(|e| InferenceError::Provider(format!("build ToolSpecification: {e}")))?;
        builder = builder.tools(Tool::ToolSpec(spec));
    }

    // (#2260) A cache-eligible run marks the LAST tool definition's
    // `function.cache_control`; translate that marker into a Bedrock-native
    // cachePoint appended after all `ToolSpec` entries, so the entire
    // (byte-stable) tools array caches as one unit. Guarded by the
    // minimum-size floor.
    if tools
        .last()
        .is_some_and(|def| def.function.cache_control.is_some())
        && cache::tools_cacheable(tools)
    {
        builder = builder.tools(Tool::CachePoint(cache::cache_point_block()));
    }

    let sdk_choice = match decision {
        ToolChoiceDecision::Auto => SdkToolChoice::Auto(AutoToolChoice::builder().build()),
        ToolChoiceDecision::Any => SdkToolChoice::Any(AnyToolChoice::builder().build()),
        ToolChoiceDecision::Named(name) => SdkToolChoice::Tool(
            SpecificToolChoice::builder()
                .name(name)
                .build()
                .map_err(|e| InferenceError::Provider(format!("build SpecificToolChoice: {e}")))?,
        ),
        ToolChoiceDecision::Suppress => unreachable!("handled above"),
    };
    builder = builder.tool_choice(sdk_choice);

    builder
        .build()
        .map(Some)
        .map_err(|e| InferenceError::Provider(format!("build ToolConfiguration: {e}")))
}

/// Map a Converse response into the provider-neutral [`ChatResponse`].
///
/// Why: every downstream consumer speaks [`ChatResponse`]; this is the single
/// place a Converse response becomes indistinguishable from an OpenRouter one.
/// What: joins `Text` content blocks (newline-separated) into
/// `AssistantMessage.content`; maps each `ToolUse` block into a `ToolCall`
/// (arguments re-serialised to the OpenAI JSON-string shape via
/// [`document_to_json_string`]); `finish_reason` is the raw, lowercased Converse
/// `stopReason` string; `usage` maps `TokenUsage`'s input/output/cache fields
/// into [`UsageBlock`]. The camelCase Anthropic/Bedrock usage payload is
/// hand-constructed here (not deserialised through the OpenAI-shaped serde,
/// which would yield zeros — see #2403 review); OpenRouter-only fields
/// (`prompt_tokens_details`, `cost`) stay `None`.
/// Test: `super::tests::converse_output_to_chat_response_*`.
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
/// Why: Bedrock's `ToolInputSchema::Json` and `ToolUseBlock.input` both take a
/// `Document`; the neutral types carry JSON Schemas and tool arguments as
/// `serde_json::Value`/JSON strings.
/// What: recursively converts the value tree. Returns `None` when the top-level
/// value is not a JSON object (Bedrock requires an object at the top level for
/// both a tool's input schema and its arguments).
/// Test: `super::tests::json_to_document_*`.
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
/// Why: `ToolUseBlock.input` is a `Document`; a `ToolCall.function.arguments` is
/// a JSON string (the OpenAI wire shape every other adapter uses), so a Bedrock
/// tool call must be re-serialised into that shape for the rest of the pipeline
/// (schema validation, tool dispatch) to consume unchanged.
/// What: recursively converts to `serde_json::Value`, then serialises. Returns
/// `None` only on a (should-be-unreachable) serialisation failure.
/// Test: `super::tests::json_to_document_round_trips_nested_object`.
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
