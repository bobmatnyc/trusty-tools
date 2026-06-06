//! Multi-turn tool-calling chat loop.
//!
//! Why: Single-shot `chat()` can't drive agents that reason, call a tool,
//! inspect the result, and continue. This module owns the canonical
//! "LLM -> tool -> LLM" loop, including provider routing (delegated per-turn to
//! `turn::dispatch_turn`), per-agent tool gating, concurrent tool dispatch,
//! and tool-call discipline.
//! What: `chat_with_tools` (convenience wrapper picking an adapter) and
//! `chat_with_tools_gated` (the full loop with every routing/gating flag).
//! Test: `tests::parallel_tool_dispatch_does_not_cancel_peers` covers the
//! concurrent-dispatch contract without a live LLM.

#[cfg(test)]
mod tests;
mod turn;

use std::sync::Arc;

use anyhow::{Context, Result};
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::{
        ChatCompletionMessageToolCall, ChatCompletionRequestMessage,
        ChatCompletionRequestToolMessageArgs, ChatCompletionRequestUserMessageArgs, FunctionCall,
    },
};

use super::adapter::{self, ModelAdapter, adapter_for_model};
use super::bedrock;
use super::compress::trim_messages_with_manager;
use super::events::{
    emit_llm_requested, emit_llm_responded, first_user_text_for_prefix, record_dispatch_usage,
    runner_label_for,
};
use super::helpers::{
    SCOPE_REMINDER, build_assistant_tool_call_message, extract_finish_task_summary,
    extract_system_and_first_user, extract_xml_tool_calls, maybe_inject_qwen3_think,
    should_retry_plain_text_turn, strip_xml_tool_noise,
};
use crate::context::ContextManager;
use crate::perf::TokenUsage;
use crate::tools::{ToolRegistry, ToolResult};
use turn::{TurnRouting, dispatch_turn};

/// Multi-turn chat loop with tool-calling support.
///
/// Why: The single-shot `chat()` can't handle agents that need to reason,
/// call a tool, look at the result, and continue. This function drives the
/// canonical "LLM -> tool -> LLM" loop until the model returns plain text or
/// we hit `max_turns`.
/// What: Seeds a conversation with the provided `messages` (typically system
/// + initial user), then repeatedly:
///   1. Sends the running conversation to the model.
///   2. If the response is text with no tool calls, returns it.
///   3. Otherwise, appends the assistant message and executes each tool call
///      via the `ToolRegistry`, appending each result as a `tool` message.
/// Test: Exercised via integration when a research agent is dispatched;
/// logic covered by `ToolRegistry` dispatch unit tests.
#[allow(dead_code)]
pub async fn chat_with_tools(
    client: &Client<OpenAIConfig>,
    model: &str,
    initial_messages: Vec<ChatCompletionRequestMessage>,
    registry: Arc<ToolRegistry>,
    temperature: f32,
    max_tokens: u32,
    max_turns: u32,
) -> Result<(String, TokenUsage)> {
    let adapter: Box<dyn ModelAdapter> = adapter_for_model(model);
    chat_with_tools_gated(
        client,
        model,
        &*adapter,
        initial_messages,
        registry,
        None,
        temperature,
        max_tokens,
        max_turns,
        false,
        None,
        false,
        false,
        &[],
    )
    .await
}

/// Variant of `chat_with_tools` that applies a per-agent tool allowlist and
/// dispatches all tool calls in a single turn concurrently.
///
/// Why: (#25) Different agents should only be able to call the tools they
/// need; (#26) the LLM frequently emits multiple tool calls per turn (e.g.
/// two `fetch_url` calls), and running them sequentially multiplies latency
/// unnecessarily. Using `FuturesUnordered` lets them proceed in parallel
/// while preserving per-call tool_use_id matching.
/// What: For each turn, dispatches the request via `turn::dispatch_turn`,
/// collects the assistant's `tool_calls`, runs each via
/// `registry.dispatch_gated(name, args, allowed)` concurrently, then appends
/// the results as `tool` messages in tool_call order. Errors are surfaced as
/// `is_error: true` tool_result content so the LLM can reason about the
/// failure. The loop always continues until the model returns plain text or
/// `max_turns`.
/// Test: See `tests::parallel_tool_dispatch_does_not_cancel_peers` (mocks two
/// tools; asserts both ran and one erroring did not cancel the other).
//
// Why (signature): This is the workhorse function-call loop and every
// parameter is independently load-bearing (model routing, sampling, gating
// flags, etc.). A wrapper struct would just punt the documentation problem one
// layer up without reducing complexity at the call sites.
#[allow(clippy::too_many_arguments)]
pub async fn chat_with_tools_gated(
    client: &Client<OpenAIConfig>,
    model: &str,
    adapter: &dyn ModelAdapter,
    initial_messages: Vec<ChatCompletionRequestMessage>,
    registry: Arc<ToolRegistry>,
    allowed_tools: Option<Vec<String>>,
    temperature: f32,
    max_tokens: u32,
    max_turns: u32,
    enable_prompt_caching: bool,
    tool_choice: Option<serde_json::Value>,
    use_finish_task: bool,
    use_anthropic_direct: bool,
    stop_sequences: &[String],
) -> Result<(String, TokenUsage)> {
    // #201: Bedrock-routed agents take a totally different code path — no
    // OpenAI-compatible HTTP, no async-openai client. We extract the system
    // prompt and the initial user message from `initial_messages`, then hand
    // off to the Bedrock multi-turn loop. AWS profile/region come from env
    // vars set by the in-process runner before this call (see
    // `TAGENT_AWS_PROFILE`/`TAGENT_AWS_REGION`); falling back to SDK
    // defaults preserves operator overrides.
    if adapter.provider() == adapter::Provider::Bedrock {
        let model_id = model.strip_prefix("bedrock/").unwrap_or(model);
        let (system_prompt, user_message) = extract_system_and_first_user(&initial_messages)?;
        let openai_tools = registry.openai_tools()?;
        let tools_json: Vec<serde_json::Value> = openai_tools
            .iter()
            .map(|t| serde_json::to_value(t).context("serialize tool for bedrock"))
            .collect::<Result<Vec<_>>>()?;
        let aws_profile =
            crate::env_compat::env_var("TAGENT_AWS_PROFILE", "OPEN_MPM_AWS_PROFILE").ok();
        let aws_region =
            crate::env_compat::env_var("TAGENT_AWS_REGION", "OPEN_MPM_AWS_REGION").ok();
        let bedrock_client =
            bedrock::build_client(aws_profile.as_deref(), aws_region.as_deref()).await?;
        return bedrock::chat_with_tools(
            &bedrock_client,
            model_id,
            &system_prompt,
            &user_message,
            temperature,
            max_tokens,
            tools_json,
            registry,
            allowed_tools,
            max_turns,
            stop_sequences,
        )
        .await;
    }

    use futures::stream::{FuturesUnordered, StreamExt};

    let openai_tools = registry.openai_tools()?;
    let mut messages = initial_messages;
    // #287: ollama models are routed via `ollama/<name>` — the prefix is only
    // used by `adapter_for_model` for selection. Strip it before sending the
    // request body so ollama's OpenAI-compat layer sees the bare model id
    // (e.g. `llama3.2:latest`). Force the raw HTTP path for these calls so
    // they hit the local ollama base URL instead of the async-openai client's
    // hardwired OpenRouter endpoint.
    let is_ollama = model.starts_with("ollama/");
    let model_owned = model.strip_prefix("ollama/").unwrap_or(model).to_string();
    let model = model_owned.as_str();
    // qwen3 supports `/think` and `/no_think` as in-message special tokens.
    // The system prompt sets `/no_think` as the default for speed; we inject
    // `/think` into the LAST user message when the prompt is reasoning-heavy
    // (math, debugging, architecture, multi-step). Detection is case-insensitive
    // on the model name. See `crate::llm::thinking_classifier`.
    if model.to_ascii_lowercase().contains("qwen3") {
        maybe_inject_qwen3_think(&mut messages);
    }
    // #47: Accumulate token usage across every turn so callers can attribute
    // a single aggregated cost/latency figure to the whole task.
    let mut total_usage = TokenUsage::default();
    // #50: Prompt caching is Anthropic-specific — the adapter's
    // `inject_cache_control` is a no-op for everyone else, so we can route
    // through the raw path whenever the caller opts in.
    let caching_active =
        enable_prompt_caching && adapter.provider() == adapter::Provider::Anthropic;
    // #59: Choose native Anthropic routing once, up front, so the request path
    // is consistent for the entire tool loop even if env changes mid-flight.
    let endpoint = adapter.api_endpoint(use_anthropic_direct);
    let route_native_anthropic = use_anthropic_direct
        && adapter.uses_native_format()
        && endpoint.auth_header_name == "x-api-key";
    // Build request the typed async-openai way (no tool_choice override, no
    // caching) or as a raw `serde_json::Value` when we need provider-specific
    // fields (cache_control and/or a non-trivial tool_choice shape) that
    // async-openai 0.28 cannot model.
    let needs_raw = caching_active || tool_choice.is_some() || route_native_anthropic || is_ollama;
    let routing = TurnRouting {
        caching_active,
        route_native_anthropic,
        needs_raw,
        endpoint: &endpoint,
    };
    // #33: Count consecutive turns in which the model returned plain text
    // instead of a tool call. Reset to 0 whenever a tool call is emitted.
    // We allow ONE plain-text-mid-task turn (injecting a reminder to use a
    // tool), then accept the second plain-text response as the final answer.
    let mut consecutive_no_tool_turns: u32 = 0;

    // #69: Apply a default 50% context-window budget trim before each turn.
    // Protects the system message (index 0) and evicts oldest turns only when
    // the accumulated prompt exceeds ~half the model's context window.
    let ctx_manager = ContextManager::new(0.5);

    for turn in 0..max_turns {
        tracing::debug!(
            turn,
            model,
            caching_active,
            "chat_with_tools: sending request"
        );

        // #69: Trim messages before each LLM call so long tool-call chains
        // don't overflow the context window.
        messages = trim_messages_with_manager(messages, &ctx_manager, model);

        let llm_started = emit_llm_requested(model, None);
        let (content, tool_calls, turn_usage) = dispatch_turn(
            client,
            model,
            adapter,
            &messages,
            &openai_tools,
            temperature,
            max_tokens,
            tool_choice.as_ref(),
            stop_sequences,
            &routing,
        )
        .await?;

        let turn_duration_ms = llm_started.elapsed().as_millis() as u64;
        emit_llm_responded(
            model,
            llm_started,
            Some(turn_usage.completion_tokens),
            Some(turn_usage.prompt_tokens),
        );
        // #281: Emit per-dispatch usage record for each turn of the tool loop.
        // `task_prefix` uses the first user message from the conversation so
        // the log entry is human-recognizable; per-turn assistant/tool churn
        // doesn't change the prefix.
        let runner_label = runner_label_for(adapter, route_native_anthropic);
        let task_prefix_src = first_user_text_for_prefix(&messages);
        record_dispatch_usage(
            model,
            runner_label,
            turn_usage.prompt_tokens,
            turn_usage.completion_tokens,
            turn_duration_ms,
            &task_prefix_src,
        );
        total_usage.add(&turn_usage);

        // BUG-FIX: qwen3:30b (and similar local models) emit tool calls as
        // inline `<tool_call>{…}</tool_call>` XML inside `content` instead of
        // populating the OpenAI `tool_calls` array. When the structured array
        // is empty but the content carries XML tool calls, promote them to
        // real `ChatCompletionMessageToolCall` values so the existing dispatch
        // path below executes them. Each synthesized id is unique within the
        // turn so tool_result pairing works.
        let mut tool_calls = tool_calls;
        if tool_calls.is_empty()
            && let Some(text) = content.as_deref()
            && text.contains("<tool_call>")
        {
            let xml_calls = extract_xml_tool_calls(text);
            if !xml_calls.is_empty() {
                tracing::info!(
                    count = xml_calls.len(),
                    "promoting XML <tool_call> blocks to structured tool calls"
                );
                for (i, v) in xml_calls.into_iter().enumerate() {
                    let name = v
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    if name.is_empty() {
                        continue;
                    }
                    let args = v
                        .get("arguments")
                        .cloned()
                        .unwrap_or(serde_json::Value::Object(Default::default()));
                    let arguments =
                        serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
                    tool_calls.push(ChatCompletionMessageToolCall {
                        id: format!("xml_tc_{turn}_{i}"),
                        r#type: async_openai::types::ChatCompletionToolType::Function,
                        function: FunctionCall { name, arguments },
                    });
                }
            }
        }

        if tool_calls.is_empty() {
            // #33: Tool-call discipline. A tool-calling agent producing plain
            // text mid-task is usually confusion, not completion. On the
            // FIRST such turn (and only if we have turns remaining), inject
            // an error reminding the agent to use a tool and retry. On the
            // SECOND, give up and accept the text as the final answer so we
            // degrade gracefully rather than loop forever.
            if should_retry_plain_text_turn(consecutive_no_tool_turns, turn, max_turns) {
                consecutive_no_tool_turns += 1;
                let reminder: ChatCompletionRequestMessage =
                    ChatCompletionRequestUserMessageArgs::default()
                        .content(
                            "Please use one of the available tools to complete this task, \
                             or provide your final answer directly.",
                        )
                        .build()
                        .context("failed to build tool-discipline reminder message")?
                        .into();
                messages.push(reminder);
                tracing::warn!(
                    turn,
                    "agent produced plain-text mid-task; injecting retry error"
                );
                continue;
            }
            // Either we already retried once, or this is the final turn.
            // Accept whatever the model produced. Strip any residual XML
            // tool_call / tool_response markup so it never reaches the user.
            let final_content = strip_xml_tool_noise(&content.unwrap_or_default());
            return Ok((final_content, total_usage));
        }

        // Tool call present — discipline counter resets.
        consecutive_no_tool_turns = 0;

        // #57: `finish_task` is a terminal tool — as soon as the model calls
        // it we exit the loop, using the supplied `summary` as the agent's
        // final text output. We check BEFORE dispatching so we don't emit a
        // stray tool_result message (there's no turn after this).
        if use_finish_task && let Some(summary) = extract_finish_task_summary(&tool_calls) {
            tracing::info!(turn, "finish_task called — exiting chat loop");
            return Ok((strip_xml_tool_noise(&summary), total_usage));
        }

        // Append assistant message (with tool calls) before sending tool results.
        let assistant_msg = build_assistant_tool_call_message(&content, &tool_calls)?;
        messages.push(assistant_msg);

        // Dispatch all tool calls in this turn concurrently. We preserve the
        // assistant-provided order for tool_result messages (matching by id)
        // because some providers require it.
        let mut futs = FuturesUnordered::new();
        for tc in &tool_calls {
            let registry = Arc::clone(&registry);
            let id = tc.id.clone();
            let name = tc.function.name.clone();
            let raw_args = tc.function.arguments.clone();
            let allowed = allowed_tools.clone();
            futs.push(async move {
                let args: serde_json::Value = serde_json::from_str(&raw_args)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                let result = registry
                    .dispatch_gated(&name, args, allowed.as_deref())
                    .await;
                (id, name, result)
            });
        }

        let mut results_by_id: std::collections::HashMap<String, (String, ToolResult)> =
            std::collections::HashMap::with_capacity(tool_calls.len());
        while let Some((id, name, result)) = futs.next().await {
            tracing::debug!(tool = %name, is_error = result.is_error(), "tool result");
            if result.is_fatal() {
                tracing::warn!(
                    tool = %name,
                    message = %result.content(),
                    "tool returned fatal (non-recoverable) error"
                );
            }
            results_by_id.insert(id, (name, result));
        }

        // Emit tool_result messages in original order so providers that care
        // about pairing see matching ids.
        // Track whether this turn included a file-write tool so we can append
        // a scope reminder after the tool_result block (see SCOPE_REMINDER).
        let mut had_file_write = false;
        for tc in &tool_calls {
            let (name, result) = match results_by_id.remove(&tc.id) {
                Some(v) => v,
                None => (
                    tc.function.name.clone(),
                    ToolResult::err("internal: tool result missing"),
                ),
            };
            if matches!(name.as_str(), "write_file" | "edit_file") {
                had_file_write = true;
            }
            let raw_str = match &result {
                ToolResult::Success(s) => s.clone(),
                ToolResult::Error { message, .. } => format!("ERROR: {message}"),
            };
            // #269/#420: Compress tool output before injecting into conversation
            // history. Uses the async RTK-style path (tries rtk subprocess first,
            // falls back to native filter chain) so per-tool filters (test runner,
            // git diff, file read, etc.) strip noise that would otherwise bloat
            // the next request's prompt. Errors and unknown tools pass through.
            let content_str = match &result {
                ToolResult::Success(_) => {
                    crate::compress::compress_tool_output_async(&name, &raw_str).await
                }
                ToolResult::Error { .. } => raw_str,
            };
            let tool_msg: ChatCompletionRequestMessage =
                ChatCompletionRequestToolMessageArgs::default()
                    .tool_call_id(tc.id.clone())
                    .content(content_str)
                    .build()
                    .context("failed to build tool result message")?
                    .into();
            messages.push(tool_msg);
            // `name` retained for trace context; silence unused.
            let _ = name;
        }

        // CC pattern: mid-conversation scope reminder. After a file-write tool
        // result, once the agent has been running for more than 5 turns, inject
        // a brief user-side reminder to keep scope and verification in mind.
        // Why: long-running engineer agents (max_turns ≥ 20) drift on scope and
        // skip verification when only the turn-0 system prompt asserts these
        // constraints. A cheap, periodic reinforcement reduces drift measurably.
        if had_file_write && turn > 5 {
            let reminder_msg: ChatCompletionRequestMessage =
                ChatCompletionRequestUserMessageArgs::default()
                    .content(SCOPE_REMINDER)
                    .build()
                    .context("failed to build scope reminder message")?
                    .into();
            messages.push(reminder_msg);
        }
    }

    anyhow::bail!(
        "chat_with_tools exceeded max_turns ({max_turns}) without a final text response (total_usage: prompt={} completion={})",
        total_usage.prompt_tokens,
        total_usage.completion_tokens,
    )
}
