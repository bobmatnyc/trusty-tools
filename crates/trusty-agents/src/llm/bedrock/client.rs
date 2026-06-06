//! Bedrock Converse request orchestration (single-turn + multi-turn loops).
//!
//! Why: Bedrock's tool-use loop is structurally identical to Anthropic's but
//! reached via the typed `aws-sdk-bedrockruntime` client rather than HTTP.
//! Keeping the request orchestration here separates "how we talk to Bedrock"
//! from "how we translate values" (see `convert.rs`).
//! What: `chat_oneshot` (returns tool-use blocks without executing them),
//! `chat` (plain single-turn), and `chat_with_tools` (multi-turn dispatch
//! loop) — all built on the shared conversion helpers.
//! Test: `bedrock_smoke_test` (gated `#[ignore]`, requires real AWS creds).

use anyhow::{Context, Result};
use aws_sdk_bedrockruntime::Client as BedrockClient;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, InferenceConfiguration, Message, SystemContentBlock,
    ToolResultBlock, ToolResultContentBlock, ToolUseBlock,
};
use serde_json::Value;
use std::sync::Arc;

use super::convert::{
    BedrockToolUse, build_tool_config, extract_text_from_output, extract_tool_uses,
    json_to_document, parse_usage,
};
use crate::perf::TokenUsage;
use crate::tools::{ToolRegistry, ToolResult};

/// Single-turn chat via Bedrock Converse with optional tool definitions, returning
/// any `ToolUse` blocks WITHOUT executing them.
///
/// Why: Some callers (the CTRL coordinator, the PM REPL one-shot path) want to
/// inspect and dispatch tool calls themselves so they can emit per-tool UI
/// events between the LLM call and the tool execution. The multi-turn
/// `chat_with_tools` loop hides that boundary; this helper exposes it.
/// What: Builds a `Converse` request with optional `tool_config`, returns the
/// assistant's text content (if any), the list of tool_use blocks, and the
/// usage tally. Callers are responsible for any follow-up turn.
/// Test: Indirectly via callers in `llm::chat_oneshot_adapter_aware`; full
/// path covered by manual `bedrock_smoke_test`.
pub async fn chat_oneshot(
    client: &BedrockClient,
    model_id: &str,
    system_prompt: &str,
    user_message: &str,
    temperature: f32,
    max_tokens: u32,
    tools: Vec<Value>,
) -> Result<(Option<String>, Vec<BedrockToolUse>, TokenUsage)> {
    let system = vec![SystemContentBlock::Text(system_prompt.to_string())];
    let user = Message::builder()
        .role(ConversationRole::User)
        .content(ContentBlock::Text(user_message.to_string()))
        .build()
        .context("failed to build Bedrock user message")?;
    let inference = InferenceConfiguration::builder()
        .temperature(temperature)
        .max_tokens(max_tokens as i32)
        .build();
    let tool_config = build_tool_config(&tools)?;

    let mut req = client
        .converse()
        .model_id(model_id)
        .set_system(Some(system))
        .messages(user)
        .inference_config(inference);
    if let Some(cfg) = tool_config {
        req = req.tool_config(cfg);
    }

    let resp = req
        .send()
        .await
        .context("Bedrock Converse one-shot request failed")?;

    let usage = parse_usage(&resp);
    let text = extract_text_from_output(&resp);
    let tool_uses = extract_tool_uses(&resp);
    Ok((text, tool_uses, usage))
}

/// Single-turn chat via the Bedrock Converse API.
///
/// Why: Mirrors `crate::llm::chat` for the OpenAI-compatible path so callers
/// that don't need tool calling can hit Bedrock with the same shape.
/// What: Sends a single user message with a system prompt, returns the
/// assistant's joined text and token usage.
/// Test: `bedrock_smoke_test` (#[ignore], requires AWS credentials).
pub async fn chat(
    client: &BedrockClient,
    model_id: &str,
    system_prompt: &str,
    user_message: &str,
    temperature: f32,
    max_tokens: u32,
) -> Result<(Option<String>, TokenUsage)> {
    let system = vec![SystemContentBlock::Text(system_prompt.to_string())];
    let user = Message::builder()
        .role(ConversationRole::User)
        .content(ContentBlock::Text(user_message.to_string()))
        .build()
        .context("failed to build Bedrock user message")?;
    let inference = InferenceConfiguration::builder()
        .temperature(temperature)
        .max_tokens(max_tokens as i32)
        .build();

    let resp = client
        .converse()
        .model_id(model_id)
        .set_system(Some(system))
        .messages(user)
        .inference_config(inference)
        .send()
        .await
        .context("Bedrock Converse single-turn request failed")?;

    let usage = parse_usage(&resp);
    let text = extract_text_from_output(&resp);
    Ok((text, usage))
}

/// Multi-turn chat with tool calling via the Bedrock Converse API.
///
/// Why: Bedrock's tool-use loop is structurally identical to Anthropic's
/// (assistant emits ToolUse blocks → caller dispatches each tool →
/// reply includes one or more ToolResult blocks under role=user). This
/// function owns that loop end-to-end so the rest of the harness only
/// needs to know about the final `(content, usage)` tuple.
/// What: Builds a `ToolConfiguration` from the OpenAI-format tools,
/// repeatedly calls `Converse` and dispatches any tool-use blocks via
/// `registry.dispatch_gated`, exits when the model returns plain text or
/// `max_turns` is exhausted.
/// Test: Indirectly via `bedrock_smoke_test` and the agent integration
/// tests that exercise a Bedrock agent end-to-end.
#[allow(clippy::too_many_arguments)]
pub async fn chat_with_tools(
    client: &BedrockClient,
    model_id: &str,
    system_prompt: &str,
    user_message: &str,
    temperature: f32,
    max_tokens: u32,
    tools: Vec<Value>,
    registry: Arc<ToolRegistry>,
    allowed_tools: Option<Vec<String>>,
    max_turns: u32,
    stop_sequences: &[String],
) -> Result<(String, TokenUsage)> {
    let system = vec![SystemContentBlock::Text(system_prompt.to_string())];
    let mut messages: Vec<Message> = vec![
        Message::builder()
            .role(ConversationRole::User)
            .content(ContentBlock::Text(user_message.to_string()))
            .build()
            .context("failed to build initial user message")?,
    ];
    // #297: Forward agent-configured stop sequences via Bedrock's
    // InferenceConfiguration.stop_sequences. The Bedrock Converse API supports
    // up to 4 stop sequences depending on the model.
    let mut inference_builder = InferenceConfiguration::builder()
        .temperature(temperature)
        .max_tokens(max_tokens as i32);
    for seq in stop_sequences {
        inference_builder = inference_builder.stop_sequences(seq.clone());
    }
    let inference = inference_builder.build();
    let tool_config = build_tool_config(&tools)?;

    let mut total_usage = TokenUsage::default();

    for turn in 0..max_turns {
        let mut req = client
            .converse()
            .model_id(model_id)
            .set_system(Some(system.clone()))
            .set_messages(Some(messages.clone()))
            .inference_config(inference.clone());
        if let Some(cfg) = tool_config.clone() {
            req = req.tool_config(cfg);
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("Bedrock Converse turn {turn} failed"))?;
        total_usage.add(&parse_usage(&resp));

        let tool_uses = extract_tool_uses(&resp);
        let text = extract_text_from_output(&resp);

        if tool_uses.is_empty() {
            // Plain-text turn — terminate with whatever the model produced.
            return Ok((text.unwrap_or_default(), total_usage));
        }

        // Append assistant message (text + tool_use blocks) for the next
        // turn's context.
        let assistant_blocks: Vec<ContentBlock> = {
            let mut blocks: Vec<ContentBlock> = Vec::new();
            if let Some(t) = &text
                && !t.is_empty()
            {
                blocks.push(ContentBlock::Text(t.clone()));
            }
            for tu in &tool_uses {
                let tub = ToolUseBlock::builder()
                    .tool_use_id(tu.id.clone())
                    .name(tu.name.clone())
                    .input(json_to_document(&tu.input))
                    .build()
                    .context("failed to build ToolUseBlock for assistant turn")?;
                blocks.push(ContentBlock::ToolUse(tub));
            }
            blocks
        };
        let assistant_msg = Message::builder()
            .role(ConversationRole::Assistant)
            .set_content(Some(assistant_blocks))
            .build()
            .context("failed to build assistant message")?;
        messages.push(assistant_msg);

        // Dispatch each tool call and collect ToolResult blocks under a
        // single user-role message (Bedrock convention).
        let mut result_blocks: Vec<ContentBlock> = Vec::with_capacity(tool_uses.len());
        for tu in &tool_uses {
            let result = registry
                .dispatch_gated(&tu.name, tu.input.clone(), allowed_tools.as_deref())
                .await;
            let is_err = result.is_error();
            let content_str = match &result {
                ToolResult::Success(s) => s.clone(),
                ToolResult::Error { message, .. } => format!("ERROR: {message}"),
            };
            let mut builder = ToolResultBlock::builder()
                .tool_use_id(tu.id.clone())
                .content(ToolResultContentBlock::Text(content_str));
            if is_err {
                builder = builder.status(aws_sdk_bedrockruntime::types::ToolResultStatus::Error);
            }
            let trb = builder.build().context("failed to build ToolResultBlock")?;
            result_blocks.push(ContentBlock::ToolResult(trb));
        }
        let user_msg = Message::builder()
            .role(ConversationRole::User)
            .set_content(Some(result_blocks))
            .build()
            .context("failed to build tool-result user message")?;
        messages.push(user_msg);
    }

    anyhow::bail!(
        "Bedrock chat_with_tools exceeded max_turns ({max_turns}) without a final text response"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::bedrock::build_client;

    /// Smoke test against a real AWS Bedrock endpoint. Gated with #[ignore]
    /// because it requires credentials. Run manually with:
    ///   cargo test bedrock_smoke_test -- --ignored
    /// using the `cto` profile (us-east-1) per project conventions.
    #[tokio::test]
    #[ignore = "requires AWS credentials (profile=cto, region=us-east-1)"]
    async fn bedrock_smoke_test() {
        let client = build_client(Some("cto"), Some("us-east-1"))
            .await
            .expect("build_client failed");
        // Use the cross-region inference profile id ("us." prefix) — the
        // raw foundation model id rejects on-demand throughput in us-east-1.
        let (text, usage) = chat(
            &client,
            "us.anthropic.claude-3-5-haiku-20241022-v1:0",
            "You are a concise assistant. Reply in plain text.",
            "Say hello in exactly 3 words.",
            0.0,
            64,
        )
        .await
        .expect("Bedrock chat failed");
        let body = text.expect("expected non-empty response text");
        assert!(!body.is_empty(), "empty response body");
        assert!(
            usage.prompt_tokens > 0 || usage.completion_tokens > 0,
            "expected non-zero usage: {usage:?}"
        );
        eprintln!("bedrock_smoke_test response: {body:?} usage={usage:?}");
    }
}
