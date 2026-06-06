//! Small pure helpers + shared types for the LLM chat loop.
//!
//! Why: The workhorse `chat_with_tools_gated` loop relies on a number of
//! independently-testable pure functions (XML tool-call promotion, qwen3
//! `/think` injection, finish_task detection, tool-discipline decisions,
//! message extraction). Keeping them here — alongside the response types and
//! the client constructor — keeps `mod.rs` focused on the loop itself.
//! What: `ToolCall` / `ChatResponse` types, `create_client`, the
//! `SCOPE_REMINDER` constant, and the pure helpers used by the loop.
//! Test: See module tests at the bottom.

use std::sync::OnceLock;

use anyhow::{Context, Result};
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::{
        ChatCompletionMessageToolCall, ChatCompletionRequestAssistantMessageArgs,
        ChatCompletionRequestMessage, ChatCompletionTool, ChatCompletionToolArgs, FunctionCall,
        FunctionObjectArgs,
    },
};

use super::credentials;
use super::{ThinkingMode, classify_thinking_mode};
use crate::perf::TokenUsage;

/// Mid-conversation scope reminder (CC pattern §2.8 / §3 Recommendation 7).
///
/// Why: Long-running engineer agents (max_turns ≥ 20) drift on the original
/// task scope when only the turn-0 system prompt asserts those constraints.
/// Anthropic's Claude Code injects `<system-reminder>` tags mid-conversation
/// to reduce this drift; we mimic the pattern as a user-role message after
/// each file-write tool result past turn 5.
/// What: A short directive re-stating scope and verification expectations.
/// Test: `chat_with_tools_gated` is exercised end-to-end via integration; the
/// constant's presence is asserted indirectly by ensuring the loop compiles
/// and the message vector grows by one after a `write_file` past turn 5.
pub(super) const SCOPE_REMINDER: &str = "<system-reminder>Scope: complete only what the original task specified. Do not add features or refactors beyond the task. Verify output before finishing.</system-reminder>";

/// Parsed tool invocation from a chat completion response.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `id` retained for future multi-turn tool-message threading
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Decoded JSON arguments from the model.
    pub arguments: serde_json::Value,
}

/// Normalized chat completion response.
#[derive(Debug, Clone, Default)]
pub struct ChatResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    /// Token usage extracted from the OpenRouter response (#47).
    /// Zero when the provider omitted usage or on cache-less non-Anthropic paths.
    pub usage: TokenUsage,
}

/// Build an async-openai client configured for OpenRouter.
///
/// Why: OpenRouter exposes an OpenAI-compatible API on a different base URL;
/// using async-openai's `OpenAIConfig` keeps us on a well-maintained client.
/// What: Reads `OPENROUTER_API_KEY` from env, sets base URL to OpenRouter.
/// Test: Called with a dummy env var set; assert no panic. Real calls are
/// integration-tested via the smoke test.
pub fn create_client() -> Result<Client<OpenAIConfig>> {
    // #250: Tolerate missing OPENROUTER_API_KEY when an alternative credential
    // is configured (ANTHROPIC_API_KEY for direct API; CLAUDE_CODE_OAUTH_TOKEN
    // for the claude CLI subprocess path). The downstream call sites either
    // route through a different code path entirely (claude CLI) or override
    // `use_anthropic_direct=true` which bypasses this client. The base URL
    // stays OpenRouter so any OpenRouter-routed call still works when its
    // key is present.
    let api_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_else(|_| {
        // Empty key — async-openai will only fail if the request actually
        // tries to use it. Direct-Anthropic / claude-code paths short-circuit
        // before then.
        String::new()
    });
    // Note: this is the bare-client constructor. We don't have an agent
    // runner context here; pass `None` so claude-code is never auto-selected
    // just because OAuth is in the env.
    if api_key.is_empty() && credentials::pick_credentials(None).is_none() {
        anyhow::bail!("{}", credentials::missing_credentials_error());
    }
    let base_url = std::env::var("OPENROUTER_BASE_URL")
        .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string());
    let config = OpenAIConfig::new()
        .with_api_key(api_key)
        .with_api_base(base_url);
    Ok(Client::with_config(config))
}

/// Extract inline `<tool_call>…</tool_call>` JSON payloads from assistant content.
///
/// Why: Some local models (notably qwen3:30b via Ollama) ignore the OpenAI
/// `tool_calls` schema and instead emit tool calls as inline XML inside the
/// assistant `content` field, e.g.
/// `<tool_call>{"name":"foo","arguments":{...}}</tool_call>`. Without parsing
/// these, the harness renders raw XML to the user and skips dispatch entirely.
/// What: Scans `content` for `<tool_call>…</tool_call>` blocks and returns the
/// parsed JSON payloads (those that parse cleanly; malformed blocks are
/// skipped silently).
/// Test: Pass a string with two well-formed tool_call blocks and one garbage
/// block; assert exactly two values returned with the expected `name` fields.
pub(super) fn extract_xml_tool_calls(content: &str) -> Vec<serde_json::Value> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?s)<tool_call>(.*?)</tool_call>").expect("static regex compiles")
    });
    re.captures_iter(content)
        .filter_map(|cap| serde_json::from_str(cap.get(1)?.as_str().trim()).ok())
        .collect()
}

/// Strip residual `<tool_call>` / `<tool_response>` markup from content.
///
/// Why: Even after we dispatch XML-style tool calls, residual
/// `<tool_call>` / `<tool_response>` markup can survive in the final assistant
/// content and leak to the user. Strip it as a defense-in-depth measure so the
/// REPL never shows raw tool XML.
/// What: Removes `<tool_call>…</tool_call>` and `<tool_response>…</tool_response>`
/// blocks (including their inner JSON payloads), trims whitespace, and returns
/// the cleaned text.
/// Test: Feed a string containing both kinds of blocks plus surrounding prose;
/// assert blocks are gone and prose survives intact.
pub(super) fn strip_xml_tool_noise(content: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?s)<tool_call>.*?</tool_call>|<tool_response>.*?</tool_response>")
            .expect("static regex compiles")
    });
    re.replace_all(content, "").trim().to_string()
}

/// Inject `/think\n` at the start of the last user message when the prompt
/// classifies as reasoning-heavy.
///
/// Why: qwen3's chat template recognizes `/think` and `/no_think` as special
/// tokens. The ctrl agent sets `/no_think` in its system prompt (fast default);
/// this helper opt-in-overrides on a per-turn basis when the user actually
/// needs chain-of-thought.
/// What: Walks `messages` in reverse for the last `role == "user"` entry,
/// extracts its text content, runs `classify_thinking_mode`, and — only when
/// the result is `Think` and the content does not already start with `/think`
/// or `/no_think` — prepends `/think\n` and writes the message back. On any
/// serde failure the messages vec is left untouched (fail-open).
/// Test: Covered indirectly via `thinking_classifier` unit tests; behaviour
/// here is a thin Value-level mutation with no LLM dependency.
pub(super) fn maybe_inject_qwen3_think(messages: &mut [ChatCompletionRequestMessage]) {
    // Walk in reverse — find the last user message.
    let last_user_idx = messages.iter().enumerate().rev().find_map(|(i, m)| {
        let v = serde_json::to_value(m).ok()?;
        if v.get("role").and_then(|r| r.as_str()) == Some("user") {
            Some(i)
        } else {
            None
        }
    });
    let Some(idx) = last_user_idx else {
        return;
    };

    let mut v = match serde_json::to_value(&messages[idx]) {
        Ok(v) => v,
        Err(_) => return,
    };

    // Pull the user's text content out of either the string or array shape.
    let content_text: String = match v.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(str::to_string))
            .collect::<Vec<_>>()
            .join(" "),
        _ => return,
    };

    // Don't double-tag if the user (or an earlier injection) already set the mode.
    let trimmed = content_text.trim_start();
    if trimmed.starts_with("/think") || trimmed.starts_with("/no_think") {
        return;
    }

    if classify_thinking_mode(&content_text) != ThinkingMode::Think {
        return;
    }

    // Mutate the content. For string content we can prepend directly; for
    // the array shape we prepend onto the first text part (qwen3 reads them
    // concatenated anyway).
    match v.get_mut("content") {
        Some(c @ serde_json::Value::String(_)) => {
            let s = c.as_str().unwrap_or("").to_string();
            *c = serde_json::Value::String(format!("/think\n{s}"));
        }
        Some(serde_json::Value::Array(parts)) => {
            if let Some(first_text) = parts
                .iter_mut()
                .find(|p| p.get("text").and_then(|t| t.as_str()).is_some())
                && let Some(t) = first_text.get_mut("text")
            {
                let s = t.as_str().unwrap_or("").to_string();
                *t = serde_json::Value::String(format!("/think\n{s}"));
            }
        }
        _ => return,
    }

    if let Ok(rebuilt) = serde_json::from_value::<ChatCompletionRequestMessage>(v) {
        messages[idx] = rebuilt;
    }
}

/// Scan a turn's tool calls for a `finish_task` invocation and return its
/// `summary` argument if present.
///
/// Why: #57 — the chat loop needs to short-circuit on `finish_task` before
/// dispatching tools. Keeping the detection/extraction logic as a pure
/// function makes it unit-testable without mocking an LLM round-trip.
/// What: Returns `Some(summary)` if any tool call's name equals
/// `FINISH_TASK_TOOL_NAME`; the summary defaults to an empty string when
/// the arguments are missing or malformed so the loop still exits cleanly.
/// Test: `extract_finish_task_summary_*` below.
pub(super) fn extract_finish_task_summary(
    tool_calls: &[ChatCompletionMessageToolCall],
) -> Option<String> {
    let finish = tool_calls
        .iter()
        .find(|tc| tc.function.name == crate::tools::finish_task::FINISH_TASK_TOOL_NAME)?;
    let args: serde_json::Value =
        serde_json::from_str(&finish.function.arguments).unwrap_or_default();
    let summary = args
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some(summary)
}

/// Decide, given the current retry count and remaining turns, whether the
/// tool-discipline loop should inject a retry-reminder or accept a plain-text
/// response as final.
///
/// Why: #33 — this logic lives inline in `chat_with_tools_gated` but is
/// cleanly testable as a pure function: we want the first plain-text turn
/// to trigger a retry and the second (or final-turn) plain-text to be
/// accepted gracefully.
/// What: Returns `true` if the caller should retry (inject error + continue),
/// `false` if it should accept the plain-text content as the final answer.
/// Test: See `tool_discipline_decision_*` tests below.
pub fn should_retry_plain_text_turn(
    consecutive_no_tool_turns: u32,
    turn: u32,
    max_turns: u32,
) -> bool {
    consecutive_no_tool_turns == 0 && turn < max_turns.saturating_sub(1)
}

/// Pull a `(system_prompt, first_user_message)` pair out of a typed
/// `ChatCompletionRequestMessage` vector.
///
/// Why: #201 — Bedrock's Converse API takes the system prompt as a separate
/// `system` field and an initial `messages` vector. The harness-side typed
/// messages are an OpenAI-shaped `system + user` pair; this helper round-trips
/// them through JSON so we don't depend on async-openai's enum being open
/// for matching.
/// What: Returns the first system message's text content and the first user
/// message's text content; falls back to empty strings if either is missing.
/// Test: `extract_system_and_first_user_basic`.
pub(super) fn extract_system_and_first_user(
    messages: &[ChatCompletionRequestMessage],
) -> Result<(String, String)> {
    let mut system = String::new();
    let mut user = String::new();
    for m in messages {
        let v = serde_json::to_value(m).context("serialize message for bedrock extraction")?;
        let role = v.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let content = v.get("content");
        let text = match content {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(parts)) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(str::to_string))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        match role {
            "system" if system.is_empty() => system = text,
            "user" if user.is_empty() => user = text,
            _ => {}
        }
        if !system.is_empty() && !user.is_empty() {
            break;
        }
    }
    Ok((system, user))
}

/// Build a single assistant message carrying the model's tool calls, for
/// injection back into the conversation before appending tool results.
///
/// Why: Multi-turn tool loops must re-send the assistant's prior tool-call
/// message so providers can pair tool_results to their calls by id.
/// What: Clones the tool calls into a fresh assistant message, attaching any
/// non-empty text content alongside.
/// Test: Exercised end-to-end via the chat loop integration tests.
pub(super) fn build_assistant_tool_call_message(
    content: &Option<String>,
    tool_calls: &[ChatCompletionMessageToolCall],
) -> Result<ChatCompletionRequestMessage> {
    let calls: Vec<ChatCompletionMessageToolCall> = tool_calls
        .iter()
        .map(|tc| ChatCompletionMessageToolCall {
            id: tc.id.clone(),
            r#type: tc.r#type.clone(),
            function: FunctionCall {
                name: tc.function.name.clone(),
                arguments: tc.function.arguments.clone(),
            },
        })
        .collect();

    let mut builder = ChatCompletionRequestAssistantMessageArgs::default();
    if let Some(text) = content
        && !text.is_empty()
    {
        builder.content(text.as_str());
    }
    builder.tool_calls(calls);
    Ok(builder
        .build()
        .context("failed to build assistant tool-call message")?
        .into())
}

/// Construct a minimal `ChatCompletionTool` wrapper around a raw schema.
/// Kept here as a helper for callers that want to bridge between raw JSON
/// schemas and async-openai's typed API.
///
/// Why: Some call sites carry tool schemas as raw JSON; this bridges them into
/// the typed builder without re-implementing the shape.
/// What: Extracts `function.{name,description,parameters}` and builds a
/// `ChatCompletionTool`, defaulting parameters to an empty object schema.
/// Test: Exercised via callers; pure builder logic.
#[allow(dead_code)]
pub(crate) fn chat_tool_from_schema(schema: &serde_json::Value) -> Result<ChatCompletionTool> {
    let function = schema
        .get("function")
        .cloned()
        .context("tool schema missing 'function' object")?;
    let name = function
        .get("name")
        .and_then(|v| v.as_str())
        .context("tool schema missing function.name")?
        .to_string();
    let description = function
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let parameters = function
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"type":"object","properties":{}}));
    let func = FunctionObjectArgs::default()
        .name(name)
        .description(description)
        .parameters(parameters)
        .build()
        .context("failed to build FunctionObject")?;
    let tool = ChatCompletionToolArgs::default()
        .function(func)
        .build()
        .context("failed to build ChatCompletionTool")?;
    Ok(tool)
}

#[cfg(test)]
mod tests;
