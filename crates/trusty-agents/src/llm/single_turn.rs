//! Single-shot chat entry points.
//!
//! Why: Many callers (the PM loop, the CTRL coordinator's one-shot path)
//! only need "send system + user, get one response" without the multi-turn
//! tool loop. Keeping these here separates the simple request/response shape
//! from the heavier orchestration in `tool_loop`.
//! What: `chat` (OpenRouter, async-openai client) and `chat_adapter_aware`
//! (routes Bedrock-prefixed models to the Converse API, forwards everything
//! else to `chat`). Both normalize to `ChatResponse`.
//! Test: Routing is covered by `adapter::tests`; the Bedrock path is exercised
//! by `bedrock::client::tests::bedrock_smoke_test` and CTRL integration.

use anyhow::{Context, Result};
use async_openai::{
    Client,
    config::OpenAIConfig,
    types::{
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
        ChatCompletionRequestUserMessageArgs, ChatCompletionTool, CreateChatCompletionRequestArgs,
    },
};

use super::adapter::{self, adapter_for_model};
use super::bedrock;
use super::events::{emit_llm_requested, emit_llm_responded, record_dispatch_usage};
use super::helpers::{ChatResponse, ToolCall};
use super::http::create_chat_completion_lenient;
use crate::perf::TokenUsage;

/// Send a single chat completion request.
///
/// Why: Hides async-openai builder boilerplate from callers and uniformly
/// converts tool-call arguments (which arrive as stringified JSON) into
/// `serde_json::Value` so downstream dispatch can index by field.
/// What: Sends `system` + `user` messages, optional tools, returns
/// `ChatResponse { content, tool_calls }`.
/// Test: Smoke test with a trivial prompt; asserts non-empty response or
/// a tool_call with `name == "delegate_to_agent"`.
pub async fn chat(
    client: &Client<OpenAIConfig>,
    model: &str,
    system_prompt: &str,
    user_message: &str,
    temperature: f32,
    max_tokens: u32,
    tools: Vec<ChatCompletionTool>,
) -> Result<ChatResponse> {
    let system_msg: ChatCompletionRequestMessage =
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt)
            .build()
            .context("failed to build system message")?
            .into();

    let user_msg: ChatCompletionRequestMessage = ChatCompletionRequestUserMessageArgs::default()
        .content(user_message)
        .build()
        .context("failed to build user message")?
        .into();

    let mut builder = CreateChatCompletionRequestArgs::default();
    builder
        .model(model)
        .temperature(temperature)
        .max_tokens(max_tokens)
        .messages(vec![system_msg, user_msg]);

    if !tools.is_empty() {
        builder.tools(tools);
    }

    let request = builder.build().context("failed to build chat request")?;

    tracing::debug!(model = %model, "sending chat request to OpenRouter");
    let started = emit_llm_requested(model, None);
    let response = create_chat_completion_lenient(client, request)
        .await
        .context("OpenRouter chat request failed")?;
    let duration_ms = started.elapsed().as_millis() as u64;
    emit_llm_responded(
        model,
        started,
        response.usage.as_ref().map(|u| u.completion_tokens),
        response.usage.as_ref().map(|u| u.prompt_tokens),
    );

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

    // #281: Emit per-dispatch usage record. `chat()` always goes through
    // OpenRouter (Bedrock/Anthropic-direct have separate paths), so the
    // runner label is hard-coded.
    record_dispatch_usage(
        model,
        "openrouter",
        usage.prompt_tokens,
        usage.completion_tokens,
        duration_ms,
        user_message,
    );

    let choice = response
        .choices
        .into_iter()
        .next()
        .context("OpenRouter returned no choices")?;

    let mut out = ChatResponse {
        content: choice.message.content.clone(),
        tool_calls: Vec::new(),
        usage,
    };

    if let Some(calls) = choice.message.tool_calls {
        for tc in calls {
            let args: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                .with_context(|| {
                    format!(
                        "failed to parse tool_call arguments as JSON: {}",
                        tc.function.arguments
                    )
                })?;
            out.tool_calls.push(ToolCall {
                id: tc.id,
                name: tc.function.name,
                arguments: args,
            });
        }
    }

    Ok(out)
}

/// Adapter-aware single-shot chat: routes Bedrock-prefixed models to AWS
/// Bedrock Converse, falling back to the OpenRouter `chat()` path otherwise.
///
/// Why: `chat()` is hardwired to the async-openai client (OpenRouter). Callers
/// like the CTRL coordinator load model names from TOML and may target Bedrock
/// (`bedrock/...`) or Anthropic-direct in addition to OpenRouter. Without this
/// helper they would have to fan out the same provider switch themselves.
/// What: Inspects the model string via `adapter_for_model`. For Bedrock,
/// builds a Bedrock client (using `TAGENT_AWS_PROFILE`/`TAGENT_AWS_REGION`
/// env vars set by callers via `BedrockEnvGuard`), calls `bedrock::chat_oneshot`,
/// and translates the `(text, tool_uses, usage)` shape to `ChatResponse`. For
/// every other provider it forwards to `chat()` unchanged.
/// Test: Routing itself is covered by `adapter_for_model` unit tests; the
/// Bedrock path is exercised by `bedrock_smoke_test` and end-to-end CTRL
/// integration when a `bedrock/...` ctrl.toml is loaded.
pub async fn chat_adapter_aware(
    client: &Client<OpenAIConfig>,
    model: &str,
    system_prompt: &str,
    user_message: &str,
    temperature: f32,
    max_tokens: u32,
    tools: Vec<ChatCompletionTool>,
) -> Result<ChatResponse> {
    let adapter = adapter_for_model(model);
    if adapter.provider() != adapter::Provider::Bedrock {
        return chat(
            client,
            model,
            system_prompt,
            user_message,
            temperature,
            max_tokens,
            tools,
        )
        .await;
    }

    let model_id = model.strip_prefix("bedrock/").unwrap_or(model);
    let aws_profile = crate::env_compat::env_var("TAGENT_AWS_PROFILE", "OPEN_MPM_AWS_PROFILE").ok();
    let aws_region = crate::env_compat::env_var("TAGENT_AWS_REGION", "OPEN_MPM_AWS_REGION").ok();
    let bedrock_client =
        bedrock::build_client(aws_profile.as_deref(), aws_region.as_deref()).await?;
    let tools_json: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| serde_json::to_value(t).context("serialize tool for bedrock"))
        .collect::<Result<Vec<_>>>()?;

    tracing::debug!(model = %model, "sending one-shot chat request to Bedrock");
    let started = emit_llm_requested(model, None);
    let (text, tool_uses, usage) = bedrock::chat_oneshot(
        &bedrock_client,
        model_id,
        system_prompt,
        user_message,
        temperature,
        max_tokens,
        tools_json,
    )
    .await?;
    let duration_ms = started.elapsed().as_millis() as u64;
    emit_llm_responded(
        model,
        started,
        Some(usage.completion_tokens),
        Some(usage.prompt_tokens),
    );
    // #281: Emit per-dispatch usage record for Bedrock one-shot calls.
    record_dispatch_usage(
        model,
        "bedrock",
        usage.prompt_tokens,
        usage.completion_tokens,
        duration_ms,
        user_message,
    );

    let tool_calls: Vec<ToolCall> = tool_uses
        .into_iter()
        .map(|tu| ToolCall {
            id: tu.id,
            name: tu.name,
            arguments: tu.input,
        })
        .collect();

    Ok(ChatResponse {
        content: text,
        tool_calls,
        usage,
    })
}
