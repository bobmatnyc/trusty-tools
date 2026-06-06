//! Per-turn request dispatch for the multi-turn tool loop.
//!
//! Why: `chat_with_tools_gated` must pick one of four request shapes each turn
//! (Anthropic native, OpenRouter raw + cache_control, ollama raw, typed
//! async-openai) based on routing flags. Pulling that branch out of the loop
//! keeps the loop body readable and both files under the 500-line cap.
//! What: `dispatch_turn` builds and sends the appropriate request for the
//! current turn and normalizes the result to `(content, tool_calls, usage)`.
//! Test: Exercised end-to-end via the tool-loop integration tests; the
//! individual request builders carry their own unit tests in `http`.

use anyhow::{Context, Result};
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::{
        ChatCompletionMessageToolCall, ChatCompletionRequestMessage, ChatCompletionTool,
        CreateChatCompletionRequestArgs,
    },
};

use crate::llm::adapter::{ApiEndpoint, ModelAdapter};
use crate::llm::anthropic_native;
use crate::llm::http::{
    build_raw_request, create_chat_completion_lenient, send_anthropic_native_completion,
    send_raw_completion,
};
use crate::perf::TokenUsage;

/// Routing flags resolved once per `chat_with_tools_gated` call and reused for
/// every turn. Bundled into a struct so `dispatch_turn`'s signature stays
/// manageable.
///
/// Why: These four booleans/endpoint are computed up front and never change
/// mid-loop; passing them as one value keeps the per-turn call site terse.
/// What: `caching_active` (Anthropic prompt cache), `route_native_anthropic`
/// (direct api.anthropic.com), `needs_raw` (any provider field async-openai
/// can't model), and the resolved `endpoint`.
/// Test: Field-only struct; behavior covered via the loop integration tests.
pub(super) struct TurnRouting<'a> {
    pub caching_active: bool,
    pub route_native_anthropic: bool,
    pub needs_raw: bool,
    pub endpoint: &'a ApiEndpoint,
}

/// Build and send the request for one turn, normalizing the provider response
/// into `(content, tool_calls, usage)`.
///
/// Why: Centralizes the four-way request-shape branch so the loop in
/// `chat_with_tools_gated` only deals with the resulting tuple.
/// What: Picks the Anthropic-native, raw (cache_control / ollama / tool_choice),
/// or typed async-openai path per `routing`, forwards `stop_sequences`, and
/// returns the parsed content/tool-calls/usage.
/// Test: Covered by the tool-loop integration tests; request builders have
/// their own unit tests in `http`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_turn(
    client: &Client<OpenAIConfig>,
    model: &str,
    adapter: &dyn ModelAdapter,
    messages: &[ChatCompletionRequestMessage],
    openai_tools: &[ChatCompletionTool],
    temperature: f32,
    max_tokens: u32,
    tool_choice: Option<&serde_json::Value>,
    stop_sequences: &[String],
    routing: &TurnRouting<'_>,
) -> Result<(
    Option<String>,
    Vec<ChatCompletionMessageToolCall>,
    TokenUsage,
)> {
    if routing.route_native_anthropic {
        // #59: Native `api.anthropic.com/v1/messages` path — prompt caching
        // is supported inline (no raw-reqwest hack needed) and tool
        // results get translated into Anthropic's `tool_result` blocks.
        let mut body = anthropic_native::build_anthropic_request(
            model,
            messages,
            openai_tools,
            temperature,
            max_tokens,
            tool_choice,
            routing.caching_active,
        )?;
        // #297: Anthropic native API uses `stop_sequences` (array of strings).
        if !stop_sequences.is_empty() {
            body["stop_sequences"] = serde_json::json!(stop_sequences);
        }
        return send_anthropic_native_completion(&body, routing.endpoint).await;
    }

    if routing.needs_raw {
        let mut raw = build_raw_request(
            model,
            messages,
            openai_tools,
            temperature,
            max_tokens,
            false, // we apply cache_control via the adapter below
        )?;
        // Only inject Anthropic-specific cache_control when routing natively
        // to api.anthropic.com. Injecting it for OpenRouter breaks the
        // OpenAI-format request body that OpenRouter expects.
        if routing.route_native_anthropic {
            adapter.inject_cache_control(&mut raw, routing.caching_active);
        }
        if let Some(tc) = tool_choice {
            // When routing via OpenRouter (not native Anthropic), convert
            // Anthropic-format tool_choice to OpenAI-compatible format.
            // {"type":"any"} -> "required", {"type":"auto"} -> "auto"
            let openrouter_tc = if !routing.route_native_anthropic {
                match tc.get("type").and_then(|v| v.as_str()) {
                    Some("any") => serde_json::json!("required"),
                    Some("auto") => serde_json::json!("auto"),
                    Some("none") => serde_json::json!("none"),
                    _ => tc.clone(), // pass through if already OpenAI format
                }
            } else {
                tc.clone()
            };
            raw["tool_choice"] = openrouter_tc;
        }
        // #297: Forward stop sequences on the raw path. OpenRouter accepts
        // OpenAI's `stop` (string or array). Use the array form for both
        // single and multi-sequence cases for consistency.
        if !stop_sequences.is_empty() {
            raw["stop"] = serde_json::json!(stop_sequences);
        }
        return send_raw_completion(&raw, adapter).await;
    }

    let mut builder = CreateChatCompletionRequestArgs::default();
    builder
        .model(model)
        .temperature(temperature)
        .max_tokens(max_tokens)
        .messages(messages.to_vec());
    // #297: Forward agent-configured stop sequences (e.g. `"```\n\n"`
    // for code-returning agents) to OpenRouter's `stop` parameter so the
    // model halts as soon as it emits one — saving 50-300 output tokens
    // of trailing commentary per response.
    if !stop_sequences.is_empty() {
        builder.stop(async_openai::types::Stop::StringArray(
            stop_sequences.to_vec(),
        ));
    }
    if !openai_tools.is_empty() {
        builder.tools(openai_tools.to_vec());
    }
    let request = builder
        .build()
        .context("failed to build chat_with_tools request")?;

    let response = create_chat_completion_lenient(client, request)
        .await
        .context("chat_with_tools: OpenRouter request failed")?;

    let usage = response
        .usage
        .as_ref()
        .map(|u| {
            let cached = u
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens)
                .unwrap_or(0);
            TokenUsage::new(u.prompt_tokens, u.completion_tokens, cached, 0)
        })
        .unwrap_or_default();

    let choice = response
        .choices
        .into_iter()
        .next()
        .context("chat_with_tools: no choices in response")?;
    let message = choice.message;
    Ok((
        message.content,
        message.tool_calls.unwrap_or_default(),
        usage,
    ))
}
