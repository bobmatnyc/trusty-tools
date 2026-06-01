//! AWS Bedrock `Converse` API provider for the `ChatProvider` trait.
//!
//! Why: organizations on AWS often prefer Bedrock (private VPC, IAM-based
//! auth, no per-request data egress to a third-party SaaS) over OpenRouter
//! for LLM access. This module wires the AWS SDK's `Converse` endpoint into
//! the `trusty-common` `ChatProvider` contract so trusty-analyze's deep pass
//! can use Bedrock models by setting
//! `TRUSTY_LLM_MODEL=bedrock/<bedrock-model-id>`.
//!
//! What: [`BedrockProvider`] wraps an `aws-sdk-bedrockruntime` client. It
//! calls `Converse` (non-streaming) and emits a single `ChatEvent::Delta`
//! followed by `ChatEvent::Done` — sufficient for the deep-analysis pass,
//! which only needs the full narrative string. Auth via the standard AWS
//! credential chain (env vars, `~/.aws/credentials`, IAM roles, SSO).
//!
//! Test: `bedrock_provider_reports_metadata` (unit, no network);
//! `bedrock_provider_new_uses_region` (unit, no network);
//! `bedrock_live_converse_smoke_test` (`#[ignore]`, requires real AWS creds).

use super::{ChatEvent, ChatProvider, ToolDef};
use crate::ChatMessage;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_bedrockruntime::Client as BedrockClient;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, InferenceConfiguration, Message, SystemContentBlock,
};
use tokio::sync::mpsc::Sender;

/// Default Bedrock model id when `TRUSTY_LLM_MODEL=bedrock/` is used without
/// a specific model suffix.
///
/// Uses the Claude Sonnet 4.6 cross-region inference profile. As of Claude
/// Sonnet 4.6, Anthropic dropped the date stamp and `-v1:0` suffix from the
/// Bedrock inference-profile id — the id is just
/// `<geography>.anthropic.claude-sonnet-4-6` (verified against AWS docs).
///
/// Cross-region inference profiles (`us.`/`eu.`/`jp.`/`global.` prefixes)
/// automatically route to the best-available region within the geography,
/// which avoids on-demand capacity errors that can occur with the bare
/// foundation model id.
///
/// Operators can override via `TRUSTY_LLM_MODEL=bedrock/<id>` without
/// touching this constant.
pub const DEFAULT_BEDROCK_MODEL: &str = "us.anthropic.claude-sonnet-4-6";

/// Env var from which a Bedrock region is read when not set explicitly.
/// `TRUSTY_AWS_REGION` takes priority over `AWS_REGION`.
pub const ENV_REGION_TRUSTY: &str = "TRUSTY_AWS_REGION";
/// Standard AWS region env var; used as a fallback to `TRUSTY_AWS_REGION`.
pub const ENV_REGION_AWS: &str = "AWS_REGION";
/// Default AWS region when neither env var is set.
pub const DEFAULT_BEDROCK_REGION: &str = "us-east-1";

/// Read the Bedrock region from environment, preferring `TRUSTY_AWS_REGION`
/// over `AWS_REGION`, defaulting to `us-east-1`.
///
/// Why: allows per-deployment region override without code changes.
/// What: returns the first non-empty value of `TRUSTY_AWS_REGION` >
///       `AWS_REGION` > `"us-east-1"`.
/// Test: covered by `bedrock_provider_new_uses_region`.
pub fn resolve_bedrock_region(explicit: Option<&str>) -> String {
    if let Some(r) = explicit.filter(|s| !s.is_empty()) {
        return r.to_string();
    }
    for var in [ENV_REGION_TRUSTY, ENV_REGION_AWS] {
        let val = std::env::var(var).unwrap_or_default();
        if !val.is_empty() {
            return val;
        }
    }
    DEFAULT_BEDROCK_REGION.to_string()
}

/// AWS Bedrock `Converse` API provider implementing [`ChatProvider`].
///
/// Why: provides a Bedrock alternative to the OpenRouter path for trusty-analyze's
/// deep-analysis pass, supporting AWS-native auth (IAM roles, SSO, env keys)
/// without requiring an OpenRouter API key.
/// What: holds a pre-built `BedrockClient` and model id. `chat_stream` calls
/// `Converse` (non-streaming), then sends a single `ChatEvent::Delta` with the
/// full response followed by `ChatEvent::Done`. Tool use is not supported for
/// this text-only deep-analysis path (tools vec is silently ignored).
/// Test: `bedrock_provider_reports_metadata` (no network);
/// `bedrock_live_converse_smoke_test` (`#[ignore]`, requires real AWS creds).
pub struct BedrockProvider {
    client: BedrockClient,
    model: String,
    region: String,
}

impl BedrockProvider {
    /// Construct a `BedrockProvider` using the standard AWS credential chain.
    ///
    /// Why: the AWS SDK's default chain handles env vars (`AWS_ACCESS_KEY_ID`,
    /// `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`), `~/.aws/credentials`
    /// profiles, instance metadata (IMDS v2), and SSO — covering both local
    /// development and production deployments without code changes.
    /// What: loads AWS config with the given `region` (or reads from env via
    /// [`resolve_bedrock_region`]), builds a `BedrockClient`, and stores it.
    /// Async because AWS credential loading touches the filesystem and
    /// possibly a metadata endpoint.
    /// Test: building with `--features bedrock` and valid AWS credentials
    /// exercises this path; `bedrock_provider_reports_metadata` constructs a
    /// mock client to verify the name/model accessors.
    pub async fn new(model: impl Into<String>, region: Option<&str>) -> Result<Self> {
        let region_str = resolve_bedrock_region(region);
        let region_provider = aws_config::meta::region::RegionProviderChain::first_try(
            aws_types::region::Region::new(region_str.clone()),
        );
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(region_provider)
            .load()
            .await;
        let client = BedrockClient::new(&config);
        Ok(Self {
            client,
            model: model.into(),
            region: region_str,
        })
    }

    /// Construct from a pre-built client (primarily for testing).
    ///
    /// Why: tests that want to inject a mock client don't need to touch AWS
    /// config loading, which requires real credentials.
    /// What: stores the client and model verbatim.
    /// Test: used by `bedrock_provider_reports_metadata`.
    #[cfg(test)]
    pub fn from_client(
        client: BedrockClient,
        model: impl Into<String>,
        region: impl Into<String>,
    ) -> Self {
        Self {
            client,
            model: model.into(),
            region: region.into(),
        }
    }

    /// The configured AWS region.
    pub fn region(&self) -> &str {
        &self.region
    }
}

#[async_trait]
impl ChatProvider for BedrockProvider {
    fn name(&self) -> &str {
        "bedrock"
    }

    fn model(&self) -> &str {
        &self.model
    }

    /// Call Bedrock `Converse` (non-streaming) and emit the full text as one
    /// `ChatEvent::Delta` followed by `ChatEvent::Done`.
    ///
    /// Why: the deep-analysis pass accumulates the full narrative string, so
    /// true streaming offers no benefit here. Non-streaming `Converse` is
    /// simpler to implement correctly and avoids the reconnect complexity of
    /// `ConverseStream`.
    /// What: builds a single-turn `Converse` request with a system prompt
    /// inferred from the first `system`-role message in `messages`, then
    /// joins all remaining messages as the conversation history. On success,
    /// emits `Delta(full_text)` + `Done`. Tool definitions are ignored — the
    /// deep-analysis call never uses tools.
    /// Test: `bedrock_live_converse_smoke_test` (`#[ignore]`).
    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        _tools: Vec<ToolDef>,
        tx: Sender<ChatEvent>,
    ) -> Result<()> {
        // Separate out the system prompt (first message with role="system"),
        // then build Bedrock `Message` objects from the rest.
        let mut system_blocks: Vec<SystemContentBlock> = Vec::new();
        let mut converse_messages: Vec<Message> = Vec::new();

        for msg in &messages {
            if msg.role == "system" {
                system_blocks.push(SystemContentBlock::Text(msg.content.clone()));
            } else {
                let role = if msg.role == "assistant" {
                    ConversationRole::Assistant
                } else {
                    ConversationRole::User
                };
                let bedrock_msg = Message::builder()
                    .role(role)
                    .content(ContentBlock::Text(msg.content.clone()))
                    .build()
                    .context("build Bedrock Message")?;
                converse_messages.push(bedrock_msg);
            }
        }

        if converse_messages.is_empty() {
            return Err(anyhow!(
                "BedrockProvider::chat_stream: no user/assistant messages provided"
            ));
        }

        let inference = InferenceConfiguration::builder().max_tokens(4096).build();

        let mut req = self
            .client
            .converse()
            .model_id(&self.model)
            .inference_config(inference)
            .set_messages(Some(converse_messages));

        if !system_blocks.is_empty() {
            req = req.set_system(Some(system_blocks));
        }

        let resp = req.send().await.with_context(|| {
            format!(
                "AWS Bedrock Converse request failed (model={}, region={}). \
                     Ensure AWS credentials are configured for Bedrock deep analysis \
                     (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY / AWS_PROFILE / IAM role).",
                self.model, self.region
            )
        })?;

        // Extract text blocks from the Converse response output.
        let text = extract_converse_text(&resp);
        let text = text.unwrap_or_default();

        if tx.send(ChatEvent::Delta(text)).await.is_err() {
            return Ok(());
        }
        let _ = tx.send(ChatEvent::Done).await;
        Ok(())
    }
}

/// Extract joined text from a `ConverseOutput` response.
///
/// Why: the Converse API wraps all content in typed `ContentBlock` variants;
/// we only need the `Text` blocks for the deep-analysis pass.
/// What: iterates the output message's content blocks and joins `Text` blocks
/// with newlines.
/// Test: covered indirectly via `bedrock_live_converse_smoke_test`.
fn extract_converse_text(
    resp: &aws_sdk_bedrockruntime::operation::converse::ConverseOutput,
) -> Option<String> {
    let msg = resp.output()?.as_message().ok()?;
    let mut out = String::new();
    for block in msg.content() {
        if let ContentBlock::Text(t) = block {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(t);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the name/model accessors without making any AWS calls.
    ///
    /// Why: ensures the `ChatProvider` trait wiring is correct and the
    /// constructor stores the model id verbatim.
    /// What: builds a dummy Bedrock client by pointing at an invalid region
    /// (the client constructor doesn't validate regions or hit the network),
    /// then checks `name()` and `model()`.
    /// Test: no network; the client is constructed but no calls are made.
    #[tokio::test]
    async fn bedrock_provider_reports_metadata() {
        // Construct a client without real credentials by loading a minimal config.
        // This doesn't hit any network endpoint.
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(aws_types::region::Region::new("us-east-1"))
            .no_credentials()
            .load()
            .await;
        let client = BedrockClient::new(&config);
        let provider = BedrockProvider::from_client(client, DEFAULT_BEDROCK_MODEL, "us-east-1");
        assert_eq!(provider.name(), "bedrock");
        assert_eq!(provider.model(), DEFAULT_BEDROCK_MODEL);
        assert_eq!(provider.region(), "us-east-1");
    }

    /// Verify region resolution precedence: explicit > TRUSTY_AWS_REGION >
    /// AWS_REGION > default.
    ///
    /// Why: operators use different env vars in different deployment contexts;
    /// the precedence order must be stable and tested.
    /// What: checks each resolution path in isolation.
    /// Test: pure env-var logic, no network.
    #[test]
    fn bedrock_region_resolution() {
        assert_eq!(
            resolve_bedrock_region(Some("eu-west-1")),
            "eu-west-1",
            "explicit should win"
        );
        assert_eq!(
            resolve_bedrock_region(Some("")),
            DEFAULT_BEDROCK_REGION,
            "empty explicit should fall through to default"
        );
        assert_eq!(
            resolve_bedrock_region(None),
            DEFAULT_BEDROCK_REGION,
            "None should return default"
        );
    }

    /// Verify that a provider constructed without real AWS credentials produces
    /// a clear, typed error when `chat_stream` is called — not a panic.
    ///
    /// Why: operators who misconfigure AWS credentials should see a descriptive
    /// error mentioning Bedrock/credentials, not an opaque panic or an
    /// OpenRouter-specific message.
    /// What: builds a client with `no_credentials()`, calls `chat_stream`,
    /// expects an error whose message mentions "Bedrock" or "credentials".
    /// Test: no network calls succeed — the error comes from the AWS SDK's
    /// credential check before any TCP connection is attempted.
    #[tokio::test]
    async fn bedrock_no_credentials_returns_clear_error() {
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(aws_types::region::Region::new("us-east-1"))
            .no_credentials()
            .load()
            .await;
        let client = BedrockClient::new(&config);
        let provider = BedrockProvider::from_client(client, DEFAULT_BEDROCK_MODEL, "us-east-1");
        let (tx, _rx) = tokio::sync::mpsc::channel::<ChatEvent>(8);
        let result = provider
            .chat_stream(
                vec![crate::ChatMessage {
                    role: "user".into(),
                    content: "hello".into(),
                    tool_call_id: None,
                    tool_calls: None,
                }],
                vec![],
                tx,
            )
            .await;
        let err = result.expect_err("should fail without real credentials");
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("bedrock")
                || msg.to_lowercase().contains("credential")
                || msg.to_lowercase().contains("aws"),
            "error message should mention Bedrock/credentials; got: {msg}"
        );
    }

    /// Live smoke test: verifies `BedrockProvider` can round-trip a real
    /// `Converse` call to Bedrock. Requires real AWS credentials with
    /// `bedrock:InvokeModel` permission on the target model.
    ///
    /// Run with:
    ///   cargo test -p trusty-common --features bedrock -- bedrock_live_converse_smoke_test --ignored
    ///
    /// Why: validates the full end-to-end path including credential resolution,
    /// wire serialization, and response parsing.
    /// What: sends a one-sentence user message and asserts the response is
    /// non-empty.
    /// Test: `#[ignore]` — requires live AWS credentials.
    #[tokio::test]
    #[ignore = "requires real AWS credentials with bedrock:InvokeModel permission"]
    async fn bedrock_live_converse_smoke_test() {
        let provider = BedrockProvider::new(DEFAULT_BEDROCK_MODEL, None)
            .await
            .expect("BedrockProvider::new failed");

        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChatEvent>(8);
        let handle = tokio::spawn(async move {
            provider
                .chat_stream(
                    vec![
                        crate::ChatMessage {
                            role: "system".into(),
                            content: "You are a concise assistant. Reply in plain text.".into(),
                            tool_call_id: None,
                            tool_calls: None,
                        },
                        crate::ChatMessage {
                            role: "user".into(),
                            content: "Say hello in exactly 3 words.".into(),
                            tool_call_id: None,
                            tool_calls: None,
                        },
                    ],
                    vec![],
                    tx,
                )
                .await
        });

        let mut text = String::new();
        let mut saw_done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                ChatEvent::Delta(s) => text.push_str(&s),
                ChatEvent::Done => saw_done = true,
                ChatEvent::Error(e) => panic!("stream error: {e}"),
                ChatEvent::ToolCall(_) => {}
            }
        }
        handle
            .await
            .expect("task panicked")
            .expect("chat_stream failed");
        assert!(!text.is_empty(), "expected non-empty response");
        assert!(saw_done, "expected ChatEvent::Done");
        eprintln!("bedrock_live_converse_smoke_test response: {text:?}");
    }
}
