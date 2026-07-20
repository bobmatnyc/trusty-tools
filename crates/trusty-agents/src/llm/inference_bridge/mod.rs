//! Bridge between trusty-agents' `async-openai` wire types and the shared
//! `trusty_common::inference` request/response model (#2410, epic #2400,
//! Step 1 — conversion only, not wired into dispatch yet).
//!
//! Why: trusty-agents' tool-calling loop is built directly on
//! `async_openai::types::ChatCompletionRequestMessage` (and friends) rather
//! than a crate-owned wire type the way `trusty-code` is (see
//! `trusty-code/src/llm/{message,request,response}.rs`). Rather than ripple a
//! new message type through every call site (`ToolRegistry`, the tool loop,
//! the recorder, `perf::TokenUsage`), the shared adapter is bridged at
//! exactly one seam: this module. It mirrors `trusty-code`'s
//! `llm::convert` (`crates/trusty-code/src/llm/convert.rs`) — field-by-field,
//! never a `to_value`/`from_value` round trip — but the source types differ
//! structurally, which surfaces two compromises documented below.
//!
//! What: [`to_shared_messages`]/[`to_shared_tools`] convert the outbound
//! `async-openai` request pieces to `trusty_common::inference` types;
//! [`from_shared_response`] converts a shared response back into the
//! `(content, tool_calls, usage)` tuple `tool_loop::turn::dispatch_turn`
//! returns; [`to_shared_tool_choice`] maps `agents::ToolChoice` onto the
//! shared [`tci::ToolChoice`] policy (`Auto→Auto`, `Any→Required`,
//! `None→None`) for a future step that routes the tool_choice-forcing raw
//! path through this seam.
//!
//! Known compromises (read before depending on this seam in a later step):
//!
//! 1. **`cache_control` cannot flow through the REQUEST side of this bridge.**
//!    `async_openai::types::ChatCompletionRequestMessage` and `FunctionObject`
//!    carry no `cache_control` field at all — unlike `trusty-code`'s
//!    hand-rolled `ChatMessage`/`FunctionDefinition`, which mirror the shared
//!    types field-for-field including that marker. trusty-agents' own
//!    prompt-cache mechanism (`crate::llm::http::build_raw_request` +
//!    `ModelAdapter::inject_cache_control`) instead mutates a raw
//!    `serde_json::Value` *after* the typed messages are serialised, entirely
//!    downstream of this bridge. Every `to_shared_message` conversion below
//!    therefore always sets `cache_control: None`. This is safe for Step 2
//!    because `tool_loop::turn::dispatch_turn` only ever routes through this
//!    bridge on the plain typed branch (`needs_raw == false`), which never
//!    carries cache_control today either. A future step that wants
//!    cache-control-bearing traffic on the shared seam will need a different
//!    design (e.g. constructing `tci::ChatMessage` directly rather than
//!    bridging from `ChatCompletionRequestMessage`). The shared wire type's
//!    own cache_control→content-block serialisation is still exercised here
//!    (see `shared_chat_message_cache_control_still_serialises_as_block` in
//!    `tests.rs`) to prove the seam is ready to receive it once a producer
//!    sets it.
//! 2. **Multi-part message content (images/audio) collapses to text-only.**
//!    `async-openai`'s `ChatCompletionRequest*MessageContent` allows an
//!    `Array` of content parts (text/image/audio); the shared
//!    `tci::ChatMessage.content` is `Option<String>`. Non-text parts are
//!    dropped. trusty-agents never constructs multi-part content today
//!    (verified: no `ImageUrl`/`InputAudio` content-part call site exists in
//!    this crate as of #2410) — every message is built via a builder's
//!    `.content(<string>)`, which always produces the `Text` variant — so
//!    this path is currently untriggered, but it is a real, if narrow, loss
//!    of fidelity for a hypothetical future vision-capable agent.
//!
//! Test: `inference_bridge::tests::*`.

#[cfg(test)]
mod tests;

use async_openai::types::{
    ChatCompletionMessageToolCall, ChatCompletionRequestAssistantMessageContent,
    ChatCompletionRequestAssistantMessageContentPart, ChatCompletionRequestDeveloperMessageContent,
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageContent,
    ChatCompletionRequestSystemMessageContentPart, ChatCompletionRequestToolMessageContent,
    ChatCompletionRequestToolMessageContentPart, ChatCompletionRequestUserMessageContent,
    ChatCompletionRequestUserMessageContentPart, ChatCompletionTool, ChatCompletionToolType,
    FunctionCall,
};
use trusty_common::inference as tci;

use crate::agents::ToolChoice as AgentToolChoice;
use crate::perf::TokenUsage;

/// Convert the outbound `async-openai` message history to the shared wire
/// message type.
///
/// Why: this is the request-side half of the seam every dispatch path funnels
/// through.
/// What: maps each `ChatCompletionRequestMessage` variant element-wise (never
/// a serde round trip); see the module doc for the two documented
/// compromises.
/// Test: `system_message_maps_role_and_text`, `user_message_maps_role_and_text`,
/// `assistant_message_with_tool_calls_maps_fields`, `tool_message_maps_fields`,
/// `multi_part_user_content_collapses_to_concatenated_text`.
pub fn to_shared_messages(messages: &[ChatCompletionRequestMessage]) -> Vec<tci::ChatMessage> {
    messages.iter().map(to_shared_message).collect()
}

fn to_shared_message(msg: &ChatCompletionRequestMessage) -> tci::ChatMessage {
    match msg {
        ChatCompletionRequestMessage::Developer(m) => tci::ChatMessage {
            role: "system".to_string(),
            content: Some(developer_content_text(&m.content)),
            tool_calls: None,
            tool_call_id: None,
            name: m.name.clone(),
            cache_control: None,
        },
        ChatCompletionRequestMessage::System(m) => tci::ChatMessage {
            role: "system".to_string(),
            content: Some(system_content_text(&m.content)),
            tool_calls: None,
            tool_call_id: None,
            name: m.name.clone(),
            cache_control: None,
        },
        ChatCompletionRequestMessage::User(m) => tci::ChatMessage {
            role: "user".to_string(),
            content: Some(user_content_text(&m.content)),
            tool_calls: None,
            tool_call_id: None,
            name: m.name.clone(),
            cache_control: None,
        },
        ChatCompletionRequestMessage::Assistant(m) => tci::ChatMessage {
            role: "assistant".to_string(),
            content: m.content.as_ref().map(assistant_content_text),
            tool_calls: m
                .tool_calls
                .as_ref()
                .map(|calls| calls.iter().map(to_shared_tool_call).collect()),
            tool_call_id: None,
            name: m.name.clone(),
            cache_control: None,
        },
        ChatCompletionRequestMessage::Tool(m) => tci::ChatMessage {
            role: "tool".to_string(),
            content: Some(tool_content_text(&m.content)),
            tool_calls: None,
            tool_call_id: Some(m.tool_call_id.clone()),
            name: None,
            cache_control: None,
        },
        // Deprecated legacy variant; trusty-agents never constructs it, but the
        // match must stay exhaustive against upstream's enum.
        ChatCompletionRequestMessage::Function(m) => tci::ChatMessage {
            role: "function".to_string(),
            content: m.content.clone(),
            tool_calls: None,
            tool_call_id: None,
            name: Some(m.name.clone()),
            cache_control: None,
        },
    }
}

/// Concatenate a developer-message content value to plain text (see
/// compromise #2 in the module doc for the `Array` case).
fn developer_content_text(content: &ChatCompletionRequestDeveloperMessageContent) -> String {
    match content {
        ChatCompletionRequestDeveloperMessageContent::Text(s) => s.clone(),
        ChatCompletionRequestDeveloperMessageContent::Array(parts) => {
            parts.iter().map(|p| p.text.as_str()).collect::<String>()
        }
    }
}

/// Concatenate a system-message content value to plain text.
fn system_content_text(content: &ChatCompletionRequestSystemMessageContent) -> String {
    match content {
        ChatCompletionRequestSystemMessageContent::Text(s) => s.clone(),
        ChatCompletionRequestSystemMessageContent::Array(parts) => parts
            .iter()
            .map(|ChatCompletionRequestSystemMessageContentPart::Text(t)| t.text.as_str())
            .collect::<String>(),
    }
}

/// Concatenate a user-message content value to plain text, dropping any
/// non-text parts (image/audio — see compromise #2 in the module doc).
///
/// Why: unreachable today (verified: no `ImageUrl`/`InputAudio` content-part
/// call site exists in this crate as of #2410) but silent on a future
/// vision-capable call site would degrade a prompt with zero signal. A
/// `tracing::warn!` — not an error, since dropping is still the documented,
/// intended behaviour for now — makes that degradation loud instead.
/// What: keeps `Text` parts, drops `ImageUrl`/`InputAudio` parts after
/// logging how many of each were dropped.
/// Test: `multi_part_user_content_collapses_to_concatenated_text`.
fn user_content_text(content: &ChatCompletionRequestUserMessageContent) -> String {
    match content {
        ChatCompletionRequestUserMessageContent::Text(s) => s.clone(),
        ChatCompletionRequestUserMessageContent::Array(parts) => {
            let (mut images, mut audio) = (0u32, 0u32);
            let text = parts
                .iter()
                .filter_map(|p| match p {
                    ChatCompletionRequestUserMessageContentPart::Text(t) => Some(t.text.as_str()),
                    ChatCompletionRequestUserMessageContentPart::ImageUrl(_) => {
                        images += 1;
                        None
                    }
                    ChatCompletionRequestUserMessageContentPart::InputAudio(_) => {
                        audio += 1;
                        None
                    }
                })
                .collect::<String>();
            if images > 0 || audio > 0 {
                tracing::warn!(
                    images_dropped = images,
                    audio_parts_dropped = audio,
                    "inference_bridge: dropped non-text content part(s) — the shared \
                     inference seam's ChatMessage.content is text-only (see \
                     inference_bridge module doc, compromise #2)"
                );
            }
            text
        }
    }
}

/// Concatenate an assistant-message content value to plain text, dropping any
/// refusal parts.
///
/// Why: same silent-degradation risk as [`user_content_text`] — a refusal
/// part carries the model's stated refusal reason, which is currently
/// discarded rather than surfaced as `content`.
/// What: keeps `Text` parts, drops `Refusal` parts after logging how many
/// were dropped.
/// Test: exercised via `assistant_message_with_tool_calls_maps_fields`
/// (single-string content path); the `Array`/refusal branch has no
/// constructible call site in this crate today (same verification as
/// compromise #2), so it is defended by this warn rather than a dedicated
/// unit test.
fn assistant_content_text(content: &ChatCompletionRequestAssistantMessageContent) -> String {
    match content {
        ChatCompletionRequestAssistantMessageContent::Text(s) => s.clone(),
        ChatCompletionRequestAssistantMessageContent::Array(parts) => {
            let mut refusals = 0u32;
            let text = parts
                .iter()
                .filter_map(|p| match p {
                    ChatCompletionRequestAssistantMessageContentPart::Text(t) => {
                        Some(t.text.as_str())
                    }
                    ChatCompletionRequestAssistantMessageContentPart::Refusal(_) => {
                        refusals += 1;
                        None
                    }
                })
                .collect::<String>();
            if refusals > 0 {
                tracing::warn!(
                    refusal_parts_dropped = refusals,
                    "inference_bridge: dropped assistant refusal content part(s) — the \
                     shared inference seam's ChatMessage.content is text-only (see \
                     inference_bridge module doc, compromise #2)"
                );
            }
            text
        }
    }
}

/// Concatenate a tool-result content value to plain text.
fn tool_content_text(content: &ChatCompletionRequestToolMessageContent) -> String {
    match content {
        ChatCompletionRequestToolMessageContent::Text(s) => s.clone(),
        ChatCompletionRequestToolMessageContent::Array(parts) => parts
            .iter()
            .map(|ChatCompletionRequestToolMessageContentPart::Text(t)| t.text.as_str())
            .collect::<String>(),
    }
}

/// Map an `async-openai` tool call (assistant-emitted, echoed in history) to
/// the shared wire shape.
fn to_shared_tool_call(call: &ChatCompletionMessageToolCall) -> tci::ToolCall {
    tci::ToolCall {
        id: call.id.clone(),
        kind: "function".to_string(),
        function: tci::FunctionCall {
            name: call.function.name.clone(),
            arguments: call.function.arguments.clone(),
        },
    }
}

/// Convert `ToolRegistry::openai_tools()`'s output to the shared tool-schema
/// type.
///
/// Why: `ToolRegistry` builds tool definitions as `async_openai::ChatCompletionTool`
/// (see `crates/trusty-agents/src/tools/mod.rs:226`); the shared adapter needs
/// `tci::ToolDefinition`.
/// What: element-wise conversion; `cache_control` is always `None` because
/// `async_openai::types::FunctionObject` has no such field (module doc
/// compromise #1 — the same gap, on the tool-schema side).
/// Test: `to_shared_tools_maps_function_definition`.
pub fn to_shared_tools(tools: &[ChatCompletionTool]) -> Vec<tci::ToolDefinition> {
    tools
        .iter()
        .map(|t| {
            tci::ToolDefinition::function(tci::FunctionDefinition {
                name: t.function.name.clone(),
                description: t.function.description.clone(),
                parameters: t.function.parameters.clone(),
                cache_control: None,
            })
        })
        .collect()
}

/// Map `agents::ToolChoice` (the TOML-configured policy) onto the shared
/// neutral [`tci::ToolChoice`].
///
/// Why: `crates/trusty-agents/src/agents/params.rs`'s `ToolChoice` is a
/// 3-variant TOML-friendly enum (`Auto`/`Any`/`None`); the shared adapter
/// speaks the 4-variant `tci::ToolChoice` (adding `Function(name)`, which
/// `agents::ToolChoice` cannot express and no trusty-agents call site needs
/// today).
/// What: `Auto → Auto`, `Any → Required` (force some tool call), `None → None`.
/// Test: `to_shared_tool_choice_maps_every_agent_variant`.
pub fn to_shared_tool_choice(choice: AgentToolChoice) -> tci::ToolChoice {
    match choice {
        AgentToolChoice::Auto => tci::ToolChoice::Auto,
        AgentToolChoice::Any => tci::ToolChoice::Required,
        AgentToolChoice::None => tci::ToolChoice::None,
    }
}

/// Convert a shared [`tci::ChatResponse`] into the `(content, tool_calls,
/// usage)` tuple `tool_loop::turn::dispatch_turn` returns for every routing
/// branch.
///
/// Why: every downstream consumer (the tool loop, `record_dispatch_usage`,
/// the XML-tool-call promotion fallback) speaks this tuple shape regardless
/// of which branch produced it; bridging back to it here means the shared
/// branch is a drop-in for the legacy typed branch.
/// What: `first_text()` for content, `first_tool_calls()` mapped element-wise
/// for tool calls, and `usage()` mapped into `TokenUsage` (the four token
/// buckets `record_dispatch_usage` reads; the shared `Usage.cost_usd` has no
/// `TokenUsage` field to carry it into — cost is computed downstream from
/// `TokenUsage` + model via `crate::perf::cost_usd`, unaffected by this seam).
/// Test: `from_shared_response_maps_content_tool_calls_and_usage`,
/// `from_shared_response_handles_no_tool_calls`.
pub fn from_shared_response(
    resp: tci::ChatResponse,
) -> (
    Option<String>,
    Vec<ChatCompletionMessageToolCall>,
    TokenUsage,
) {
    let content = resp.first_text();
    let tool_calls = resp
        .first_tool_calls()
        .iter()
        .map(from_shared_tool_call)
        .collect();
    let u = resp.usage();
    let usage = TokenUsage::new(
        u.prompt_tokens,
        u.completion_tokens,
        u.cache_read_tokens,
        u.cache_creation_tokens,
    );
    (content, tool_calls, usage)
}

/// Map a shared [`tci::ToolCall`] back to the `async-openai` wire shape.
fn from_shared_tool_call(call: &tci::ToolCall) -> ChatCompletionMessageToolCall {
    ChatCompletionMessageToolCall {
        id: call.id.clone(),
        r#type: ChatCompletionToolType::Function,
        function: FunctionCall {
            name: call.function.name.clone(),
            arguments: call.function.arguments.clone(),
        },
    }
}
