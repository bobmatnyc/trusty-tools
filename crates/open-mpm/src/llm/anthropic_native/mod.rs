//! Anthropic native `/v1/messages` request/response shim.
//!
//! Why: #59 — when we route to `api.anthropic.com` directly (instead of
//! through OpenRouter's OpenAI-compatible `/v1/chat/completions`) we must
//! speak Anthropic's native wire format. That format is different enough
//! from OpenAI's that a round-trip through `serde_json::to_value` isn't
//! sufficient: `system` is a top-level field, tools use `input_schema`
//! instead of `parameters`, tool results are content blocks, responses are
//! a flat array of content blocks with `stop_reason` instead of nested
//! `choices[0].message.{content, finish_reason}`.
//! What: Two pure functions — `build_anthropic_request` (internal typed
//! messages + OpenAI-shape tools → Anthropic request body) and
//! `parse_anthropic_response` (Anthropic response JSON → `AnthropicResponse`
//! with extracted text, tool calls, stop reason, and cache-aware usage).
//! The value-level conversion helpers live in `convert`.
//! Test: See module tests at the bottom.

mod convert;

use anyhow::{Context, Result};
use async_openai::types::{
    ChatCompletionMessageToolCall, ChatCompletionRequestMessage, ChatCompletionTool,
    ChatCompletionToolType, FunctionCall,
};
use serde_json::{Value, json};

use convert::{
    attach_cache_control_to_last_assistant, convert_assistant_message, extract_text_content,
    history_token_estimate, strip_provider_prefix,
};

use crate::perf::TokenUsage;

/// Tool-call shape parsed from an Anthropic native response, already converted
/// to the same internal `ChatCompletionMessageToolCall` type the OpenRouter
/// path returns so the downstream dispatch loop doesn't need to branch.
pub type AnthropicToolCall = ChatCompletionMessageToolCall;

/// Parsed Anthropic `/v1/messages` response.
///
/// Why: Callers need text + tool calls + usage in one pass; keeping them in
/// a struct avoids re-parsing the JSON from multiple sites.
/// What: `text_content` is the concatenation of all `text` blocks;
/// `tool_calls` holds any `tool_use` blocks already normalized to
/// `ChatCompletionMessageToolCall`; `stop_reason` is the raw string from the
/// response; `usage` carries cache-aware token counts.
/// Test: `parse_anthropic_response_extracts_text_and_tool_use`,
/// `parse_anthropic_response_reads_cache_read_tokens`.
#[derive(Debug, Clone)]
pub struct AnthropicResponse {
    pub text_content: Option<String>,
    pub tool_calls: Vec<AnthropicToolCall>,
    /// Retained for observability/debug logging and future end-of-turn
    /// heuristics; the chat loop currently infers termination from the
    /// presence/absence of `tool_calls`.
    #[allow(dead_code)]
    pub stop_reason: String,
    pub usage: TokenUsage,
}

/// Build an Anthropic native `/v1/messages` request body from our internal
/// message list and OpenAI-shape tools.
///
/// Why: The PM + sub-agent paths already assemble typed
/// `ChatCompletionRequestMessage` values and OpenAI-shape `ChatCompletionTool`
/// values; this function converts them to the Anthropic shape so we can
/// opt-in to direct-API routing without changing every call site.
/// What: Extracts a top-level `system` string from the first `role:system`
/// message, rewrites remaining messages into Anthropic's user/assistant
/// format (with tool-result content blocks when a `role:tool` message is
/// present), and maps each tool's `parameters` to `input_schema`. When
/// `enable_caching=true` a `cache_control: {type: "ephemeral"}` marker is
/// attached to the system block and to the last tool definition so
/// subsequent requests hit the prompt cache.
/// Test: `build_anthropic_request_places_system_top_level`,
/// `build_anthropic_request_converts_tool_parameters_to_input_schema`,
/// `build_anthropic_request_strips_provider_prefix_from_model`,
/// `build_anthropic_request_converts_tool_result_messages`.
pub fn build_anthropic_request(
    model: &str,
    messages: &[ChatCompletionRequestMessage],
    tools: &[ChatCompletionTool],
    temperature: f32,
    max_tokens: u32,
    tool_choice: Option<&Value>,
    enable_caching: bool,
) -> Result<Value> {
    // Anthropic's `model` field expects a bare name like `claude-sonnet-4-5`
    // (no `anthropic/` or `openrouter/` prefix). Strip any single leading
    // `vendor/` segment so configs written for OpenRouter keep working.
    let model_name = strip_provider_prefix(model);

    // Round-trip messages through JSON to inspect role/content uniformly.
    let mut system_prompt: Option<String> = None;
    let mut out_messages: Vec<Value> = Vec::with_capacity(messages.len());

    for msg in messages {
        let v = serde_json::to_value(msg).context("serialize chat message for Anthropic")?;
        let role = v.get("role").and_then(|r| r.as_str()).unwrap_or("");
        match role {
            "system" => {
                // Take only the FIRST system message's text. Later ones are
                // ignored — our callers only ever send one.
                if system_prompt.is_none() {
                    system_prompt = Some(extract_text_content(v.get("content")));
                }
            }
            "user" => {
                // User messages are already in a compatible shape: a string
                // content field is valid on Anthropic too. Pass through as-is
                // with only the fields Anthropic accepts.
                let content = v.get("content").cloned().unwrap_or(Value::Null);
                out_messages.push(json!({
                    "role": "user",
                    "content": content,
                }));
            }
            "assistant" => {
                out_messages.push(convert_assistant_message(&v));
            }
            "tool" => {
                // Anthropic represents tool results as a user message with a
                // `tool_result` content block. If the previous out_message is
                // already a user message with tool_result content, append;
                // otherwise push a new one.
                let tool_use_id = v
                    .get("tool_call_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let result_text = extract_text_content(v.get("content"));
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": result_text,
                });
                if let Some(last) = out_messages.last_mut()
                    && last.get("role").and_then(|r| r.as_str()) == Some("user")
                    && let Some(arr) = last.get_mut("content").and_then(|c| c.as_array_mut())
                {
                    arr.push(block);
                    continue;
                }
                out_messages.push(json!({
                    "role": "user",
                    "content": [block],
                }));
            }
            _ => {
                // Unknown roles are dropped with a trace; we don't want to
                // synthesize garbage into the request.
                tracing::debug!(role = %role, "dropping unknown role when building Anthropic request");
            }
        }
    }

    let mut body = json!({
        "model": model_name,
        "max_tokens": max_tokens,
        "temperature": temperature,
        "messages": out_messages,
    });

    if let Some(sys) = system_prompt {
        if enable_caching {
            // Anthropic supports cache_control on the system block.
            body["system"] = json!([
                {
                    "type": "text",
                    "text": sys,
                    "cache_control": {"type": "ephemeral"}
                }
            ]);
        } else {
            body["system"] = Value::String(sys);
        }
    }

    // #29: Conversation-history cache breakpoint.
    //
    // Why: The system-prompt cache (above) only de-duplicates the agent's
    // identity prompt. Long multi-turn workflows (e.g. research phases that
    // accumulate many tool results) re-pay full input rates on every turn for
    // the GROWING conversation history. Adding a second `cache_control`
    // breakpoint on the LAST assistant block lets Anthropic cache up through
    // the most recent assistant turn so the next request only pays cache-read
    // rates (~10%) for everything before it.
    // What: When `enable_caching=true` AND the converted message list contains
    // an assistant message AND the rough token estimate of all messages
    // exceeds 2000 tokens, attach `cache_control: {"type":"ephemeral"}` to the
    // last content block of the most recent assistant message. Anthropic
    // allows up to 4 cache breakpoints per request; we use only 2 (system +
    // last assistant) so callers stay well under the limit.
    // Test: `build_anthropic_request_caches_last_assistant_when_large`,
    // `build_anthropic_request_skips_history_cache_when_small`.
    if enable_caching && history_token_estimate(&out_messages) > 2000 {
        attach_cache_control_to_last_assistant(&mut out_messages);
        // Re-emit messages because we already moved a clone into `body` above.
        body["messages"] = Value::Array(out_messages.clone());
    }

    if !tools.is_empty() {
        let mut tools_out: Vec<Value> = Vec::with_capacity(tools.len());
        for t in tools {
            let tv = serde_json::to_value(t).context("serialize tool for Anthropic")?;
            // OpenAI shape: {type:"function", function:{name,description,parameters}}
            let func = tv.get("function").cloned().unwrap_or(Value::Null);
            let name = func
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let description = func
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input_schema = func
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object","properties":{}}));
            tools_out.push(json!({
                "name": name,
                "description": description,
                "input_schema": input_schema,
            }));
        }
        if enable_caching
            && let Some(last) = tools_out.last_mut()
            && let Some(obj) = last.as_object_mut()
        {
            obj.insert("cache_control".to_string(), json!({"type": "ephemeral"}));
        }
        body["tools"] = Value::Array(tools_out);
    }

    if let Some(tc) = tool_choice {
        body["tool_choice"] = tc.clone();
    }

    Ok(body)
}

/// Parse a raw Anthropic `/v1/messages` response JSON into `AnthropicResponse`.
///
/// Why: The chat loop already works in terms of `ChatCompletionMessageToolCall`;
/// converting on ingress keeps the tool-dispatch code path identical for both
/// routes.
/// What: Walks `response.content[]`, accumulating `text` blocks into
/// `text_content` and `tool_use` blocks into `tool_calls`. Reads
/// `stop_reason` (defaulting to `"end_turn"`) and extracts usage including
/// `cache_read_input_tokens` / `cache_creation_input_tokens`.
/// Test: `parse_anthropic_response_extracts_text_and_tool_use`,
/// `parse_anthropic_response_reads_cache_read_tokens`,
/// `parse_anthropic_response_empty_content_yields_none`.
pub fn parse_anthropic_response(response: &Value) -> AnthropicResponse {
    let mut text_buf = String::new();
    let mut tool_calls: Vec<AnthropicToolCall> = Vec::new();

    if let Some(blocks) = response.get("content").and_then(|c| c.as_array()) {
        for block in blocks {
            match block.get("type").and_then(|v| v.as_str()) {
                Some("text") => {
                    if let Some(s) = block.get("text").and_then(|v| v.as_str()) {
                        if !text_buf.is_empty() {
                            text_buf.push('\n');
                        }
                        text_buf.push_str(s);
                    }
                }
                Some("tool_use") => {
                    let id = block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                    let arguments =
                        serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                    tool_calls.push(ChatCompletionMessageToolCall {
                        id,
                        r#type: ChatCompletionToolType::Function,
                        function: FunctionCall { name, arguments },
                    });
                }
                _ => {}
            }
        }
    }

    let stop_reason = response
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("end_turn")
        .to_string();

    let usage = response
        .get("usage")
        .map(|u| {
            let prompt = u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let completion = u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let cache_read = u
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let cache_creation = u
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            TokenUsage::new(prompt, completion, cache_read, cache_creation)
        })
        .unwrap_or_default();

    let text_content = if text_buf.is_empty() {
        None
    } else {
        Some(text_buf)
    };

    AnthropicResponse {
        text_content,
        tool_calls,
        stop_reason,
        usage,
    }
}

#[cfg(test)]
mod tests;
