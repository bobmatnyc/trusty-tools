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
use trusty_common::claude_temperature::accepts_temperature;

use super::convert::{
    BedrockToolUse, build_tool_config, extract_text_from_output, extract_tool_uses,
    json_to_document, parse_usage,
};
use crate::perf::TokenUsage;
use crate::tools::{ToolRegistry, ToolResult};

/// The `InferenceConfiguration` every Converse request in this module sends.
///
/// Why: Opus 5.5 and Sonnet 5.5 reject a Converse request that carries
/// `temperature` with a `ValidationException` (#10939).
/// What: sets `max_tokens` and each stop sequence; sets `temperature` only
/// when trusty-common's [`accepts_temperature`] allows it for `model_id`, so
/// the model list lives in one place.
/// Test: `converse_requests_omit_temperature_only_for_claude_5_5`.
fn inference_config(
    model_id: &str,
    temperature: f32,
    max_tokens: u32,
    stop_sequences: &[String],
) -> InferenceConfiguration {
    // #10939: omit `temperature` for a model that rejects it.
    let temperature = accepts_temperature(model_id).then_some(temperature);
    let mut builder = InferenceConfiguration::builder()
        .set_temperature(temperature)
        .max_tokens(max_tokens as i32);
    for seq in stop_sequences {
        builder = builder.stop_sequences(seq.clone());
    }
    builder.build()
}

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
    let inference = inference_config(model_id, temperature, max_tokens, &[]);
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
/// Test: `bedrock_smoke_test` (#\[ignore\], requires AWS credentials).
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
    let inference = inference_config(model_id, temperature, max_tokens, &[]);

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
    initial_messages: Vec<trusty_common::inference::ChatMessage>,
    temperature: f32,
    max_tokens: u32,
    tools: Vec<Value>,
    registry: Arc<ToolRegistry>,
    allowed_tools: Option<Vec<String>>,
    max_turns: u32,
    stop_sequences: &[String],
) -> Result<(String, TokenUsage)> {
    let (system, mut messages) =
        trusty_common::inference::bedrock::conversation_messages(initial_messages)?;
    // #297: Forward agent-configured stop sequences via Bedrock's
    // InferenceConfiguration.stop_sequences. The Bedrock Converse API supports
    // up to 4 stop sequences depending on the model.
    let inference = inference_config(model_id, temperature, max_tokens, stop_sequences);
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
    use aws_sdk_bedrockruntime::config::interceptors::BeforeSerializationInterceptorContextRef;
    use aws_sdk_bedrockruntime::config::{
        BehaviorVersion, ConfigBag, Credentials, Intercept, Region,
    };
    use aws_sdk_bedrockruntime::operation::converse::ConverseInput;
    use std::sync::Mutex;
    use trusty_common::inference::ChatMessage;

    /// Records each Converse request's `InferenceConfiguration`, then fails the
    /// request before it is signed or sent, so the test never reaches AWS.
    #[derive(Debug, Clone, Default)]
    struct CaptureInference(Arc<Mutex<Vec<InferenceConfiguration>>>);

    impl Intercept for CaptureInference {
        fn name(&self) -> &'static str {
            "CaptureInference"
        }

        fn read_before_execution(
            &self,
            context: &BeforeSerializationInterceptorContextRef<'_>,
            _cfg: &mut ConfigBag,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let input = context
                .input()
                .downcast_ref::<ConverseInput>()
                .ok_or("not a Converse request")?;
            let inference = input
                .inference_config()
                .cloned()
                .ok_or("request has no inference config")?;
            self.0.lock().expect("capture lock").push(inference);
            Err("captured; request stopped before send".into())
        }
    }

    /// A Bedrock client with static placeholder credentials and `capture`.
    fn capturing_client(capture: &CaptureInference) -> BedrockClient {
        let conf = aws_sdk_bedrockruntime::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new("test", "test", None, None, "test"))
            .interceptor(capture.clone())
            .build();
        BedrockClient::from_conf(conf)
    }

    /// Why: Opus 5.5 and Sonnet 5.5 reject a Converse request that carries
    /// `temperature` (#10939); every other model keeps its configured value.
    /// What: sends one request through each of `chat_oneshot`, `chat` and
    /// `chat_with_tools` for both 5.5 ids and `us.anthropic.claude-sonnet-4-6`,
    /// captures the `InferenceConfiguration` before send, and asserts
    /// `temperature` is `None` for 5.5 and `0.3` otherwise, with `max_tokens`
    /// and the tool loop's stop sequences still set.
    /// Test: itself.
    #[tokio::test]
    async fn converse_requests_omit_temperature_only_for_claude_5_5() {
        let stop = vec!["STOP".to_string()];
        for (model, expected) in [
            ("us.anthropic.claude-opus-5-5", None),
            ("us.anthropic.claude-sonnet-5-5", None),
            ("us.anthropic.claude-sonnet-4-6", Some(0.3)),
        ] {
            let capture = CaptureInference::default();
            let client = capturing_client(&capture);
            let oneshot = chat_oneshot(&client, model, "sys", "hi", 0.3, 64, Vec::new()).await;
            assert!(oneshot.is_err(), "{model}: capture must stop chat_oneshot");
            let single = chat(&client, model, "sys", "hi", 0.3, 64).await;
            assert!(single.is_err(), "{model}: capture must stop chat");
            let looped = chat_with_tools(
                &client,
                model,
                vec![ChatMessage::system("sys"), ChatMessage::user("hi")],
                0.3,
                64,
                Vec::new(),
                Arc::new(ToolRegistry::new()),
                None,
                1,
                &stop,
            )
            .await;
            assert!(
                looped.is_err(),
                "{model}: capture must stop chat_with_tools"
            );

            let sent = capture.0.lock().expect("capture lock").clone();
            assert_eq!(sent.len(), 3, "{model}: one request per entry point");
            for (path, inference) in ["chat_oneshot", "chat", "chat_with_tools"]
                .iter()
                .zip(&sent)
            {
                assert_eq!(inference.temperature(), expected, "{model} via {path}");
                assert_eq!(inference.max_tokens(), Some(64), "{model} via {path}");
            }
            assert_eq!(
                sent[2].stop_sequences(),
                &stop[..],
                "{model} stop sequences"
            );
        }
    }

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
        // #10939: Opus 5.5 rejects `temperature`, so this request proves the
        // field is omitted.
        let (text, usage) = chat(
            &client,
            "us.anthropic.claude-opus-5-5",
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
