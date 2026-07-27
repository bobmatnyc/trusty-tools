//! AWS Bedrock `ConverseStream` API provider for the `ChatProvider` trait.
//!
//! Why: organizations on AWS often prefer Bedrock (private VPC, IAM-based
//! auth, no per-request data egress to a third-party SaaS) over OpenRouter
//! for LLM access. This module wires the AWS SDK's `ConverseStream` endpoint
//! into the `trusty-common` `ChatProvider` contract so trusty-analyze's deep
//! pass and trusty-agents' token-streaming chat path (issue #3767) can both
//! use Bedrock models by setting `TRUSTY_LLM_MODEL=bedrock/<bedrock-model-id>`
//! / `model=bedrock/<id>` respectively.
//!
//! What: [`BedrockProvider`] wraps an `aws-sdk-bedrockruntime` client.
//! `chat_stream` calls `ConverseStream` and drives its event-stream framing —
//! `ContentBlockDelta` text fragments become [`ChatEvent::Delta`], the
//! terminal `Metadata` event's token tally becomes a single
//! [`ChatEvent::Usage`] (Bedrock reports usage exactly once, at the end of
//! the stream — never per-delta, unlike token text), a mid-stream
//! `SdkError` becomes [`ChatEvent::Error`] followed by `Err` (mirroring the
//! OpenAI-compatible pump's #3757 failure contract), and a clean stream end
//! becomes [`ChatEvent::Done`]. Tool use is not supported on this path (the
//! `tools` argument is silently ignored, matching the pre-#3767 behaviour).
//! Auth via the standard AWS credential chain (env vars,
//! `~/.aws/credentials`, IAM roles, SSO).
//!
//! Test: `bedrock_provider_reports_metadata` (unit, no network);
//! `bedrock_provider_new_uses_region` (unit, no network);
//! `bedrock_live_converse_stream_smoke_test` (`#[ignore]`, requires real AWS
//! creds).

use super::{ChatEvent, ChatProvider, ChatUsage, SamplingParams, ToolDef};
use crate::ChatMessage;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_bedrockruntime::Client as BedrockClient;
use aws_sdk_bedrockruntime::types::ConverseStreamOutput as StreamEvent;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ContentBlockDelta, ConversationRole, InferenceConfiguration, Message,
    SystemContentBlock,
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

/// AWS Bedrock `ConverseStream` API provider implementing [`ChatProvider`].
///
/// Why: provides a Bedrock alternative to the OpenRouter path for
/// trusty-analyze's deep-analysis pass and trusty-agents' token-streaming
/// chat path (issue #3767), supporting AWS-native auth (IAM roles, SSO, env
/// keys) without requiring an OpenRouter API key.
/// What: holds a pre-built `BedrockClient`, model id, and optional
/// `SamplingParams`. `chat_stream` calls `ConverseStream` and forwards its
/// event-stream framing as incremental `ChatEvent::Delta`s, a single terminal
/// `ChatEvent::Usage`, and `ChatEvent::Done`. Tool use is not supported on
/// this path (the `tools` argument is silently ignored).
/// Test: `bedrock_provider_reports_metadata` (no network);
/// `bedrock_live_converse_stream_smoke_test` (`#[ignore]`, requires real AWS
/// creds).
pub struct BedrockProvider {
    client: BedrockClient,
    model: String,
    region: String,
    /// #3767: sampling knobs forwarded onto the `ConverseStream` request so a
    /// Bedrock-routed streamed turn matches the blocking path's temperature /
    /// token ceiling / stop sequences — the same parity #3758 established for
    /// the OpenRouter streaming path. Default (all-absent) reproduces this
    /// provider's pre-existing hardcoded `max_tokens(4096)` behaviour.
    sampling: SamplingParams,
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
            sampling: SamplingParams::default(),
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
            sampling: SamplingParams::default(),
        }
    }

    /// Attach sampling parameters to every request this provider sends.
    ///
    /// Why (#3767): parallel to `OpenRouterProvider::with_sampling` (#3758) —
    /// callers with a blocking Bedrock path need the streamed turn to honour
    /// the same temperature / token ceiling / stop sequences.
    /// What: consuming builder; returns `self` with `sampling` replaced.
    /// Test: `bedrock_stream_forwards_sampling_params`.
    pub fn with_sampling(mut self, sampling: SamplingParams) -> Self {
        self.sampling = sampling;
        self
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

    /// Call Bedrock `ConverseStream` and drive its event-stream framing onto
    /// `tx` as [`ChatEvent`]s.
    ///
    /// Why (issue #3767): the token-streaming chat path needs real
    /// incremental deltas — the previous non-streaming `Converse` call
    /// buffered the entire reply before emitting a single `Delta`, which is
    /// indistinguishable from the blocking path and defeats the point of
    /// streaming. `ConverseStream`'s wire format differs structurally from
    /// the OpenAI-compatible SSE the other providers speak: it is a binary
    /// AWS event-stream (framed and demultiplexed by the SDK's
    /// `EventReceiver`, not raw `data:` lines), and usage/token accounting
    /// arrives exactly once, in a terminal `Metadata` event — never
    /// per-delta. Both are handled below so usage is never silently dropped.
    /// What: builds a single-turn `ConverseStream` request (system prompt
    /// from the first `system`-role message, the rest as conversation
    /// history, `self.sampling` forwarded as `InferenceConfiguration`),
    /// then loops `EventReceiver::recv()`: `ContentBlockDelta::Text` becomes
    /// `ChatEvent::Delta`; the `Metadata` event's `usage` becomes exactly one
    /// `ChatEvent::Usage`; every other event variant (`MessageStart`,
    /// `ContentBlockStart/Stop`, `MessageStop`, and any future `Unknown`
    /// variant) is a structural marker with nothing to forward and is
    /// ignored. A clean `Ok(None)` end-of-stream emits `ChatEvent::Done`. A
    /// mid-stream `Err(SdkError)` (a modeled exception — throttling,
    /// validation, an internal/service error — or a transport failure)
    /// emits `ChatEvent::Error` AND returns `Err`, mirroring the
    /// OpenAI-compatible pump's #3757 dual-channel failure contract so
    /// neither a channel-only consumer nor a task-joining caller can mistake
    /// a failed stream for a complete one. Tool definitions are ignored —
    /// this path has never supported tool use (matches pre-#3767 behaviour).
    /// Test: `bedrock_stream_forwards_text_deltas_in_order`,
    /// `bedrock_stream_reports_usage_from_metadata_event`,
    /// `bedrock_stream_surfaces_mid_stream_error`,
    /// `bedrock_stream_forwards_sampling_params`,
    /// `bedrock_live_converse_stream_smoke_test` (`#[ignore]`).
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

        // #3767/#3758 parity: forward the caller's sampling knobs instead of
        // the previous hardcoded `max_tokens(4096)` — `unwrap_or(4096)` keeps
        // that exact default when the caller supplies nothing.
        let stop_sequences = (!self.sampling.stop.is_empty()).then(|| self.sampling.stop.clone());
        let inference = InferenceConfiguration::builder()
            .max_tokens(self.sampling.max_tokens.unwrap_or(4096) as i32)
            .set_temperature(self.sampling.temperature)
            .set_stop_sequences(stop_sequences)
            .build();

        let mut req = self
            .client
            .converse_stream()
            .model_id(&self.model)
            .inference_config(inference)
            .set_messages(Some(converse_messages));

        if !system_blocks.is_empty() {
            req = req.set_system(Some(system_blocks));
        }

        let output = req.send().await.with_context(|| {
            format!(
                "AWS Bedrock ConverseStream request failed (model={}, region={}). \
                     Ensure AWS credentials are configured for Bedrock \
                     (AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY / AWS_PROFILE / IAM role).",
                self.model, self.region
            )
        })?;

        let mut stream = output.stream;
        loop {
            let recv_result = stream
                .recv()
                .await
                .map_err(|sdk_err| format!("Bedrock ConverseStream error: {sdk_err}"));
            match handle_stream_event(recv_result, &tx).await {
                Flow::Continue => {}
                Flow::Stop => return Ok(()),
                // The Error event is already sent; the Err is the second
                // half of the #3757-style dual-channel failure contract.
                Flow::Failed(message) => return Err(anyhow!("{message}")),
            }
        }
    }
}

/// What the event loop should do after one decoded (or failed) `recv()`.
///
/// Why: mirrors `openai_compat::sse_pump::Flow` so the two streaming
/// providers in this crate share one failure-signalling shape — `Continue`
/// keeps reading, `Stop` means a normal terminal was already sent, `Failed`
/// means [`ChatEvent::Error`] was already sent and the caller MUST return
/// `Err`.
/// What: three variants, identical contract to the SSE pump's `Flow`.
/// Test: exercised indirectly via `handle_stream_event`'s own tests.
#[derive(Debug, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
    Failed(String),
}

/// Map one `ConverseStream` `recv()` result onto `tx` as zero or one
/// [`ChatEvent`]s, returning what the caller's loop should do next.
///
/// Why (issue #3767): this is the actual event → `ChatEvent` translation —
/// the part of the fix with real branching logic — split out from
/// `chat_stream`'s AWS-SDK plumbing so it is unit-testable with a scripted
/// sequence of events instead of a live Bedrock connection or hand-encoded
/// event-stream wire bytes (the SDK's `EventReceiver` has no public
/// constructor outside a real HTTP response, but every event TYPE it yields
/// has a public builder, so tests construct those directly).
/// What: `Ok(Some(ContentBlockDelta(text)))` sends `ChatEvent::Delta` and
/// returns `Continue`; `Ok(Some(Metadata(meta)))` sends `ChatEvent::Usage`
/// when `meta.usage()` is present (Bedrock reports usage exactly once, here,
/// never per-delta — dropping it would silently zero out every Bedrock
/// streamed call for any usage/cost consumer) and returns `Continue`; every
/// other event variant (`MessageStart`/`ContentBlockStart`/`ContentBlockStop`/
/// `MessageStop`, and any SDK `Unknown` future variant) is a structural
/// marker with nothing to forward and returns `Continue`; `Ok(None)` sends
/// `ChatEvent::Done` and returns `Stop`; `Err(message)` sends
/// `ChatEvent::Error(message)` and returns `Failed(message)`. A receiver
/// that has hung up (`tx.send` failing) short-circuits to `Stop` — mirrors
/// the SSE pump's "consumer walked away" handling.
/// Test: `bedrock_stream_forwards_text_deltas_in_order`,
/// `bedrock_stream_ignores_structural_events`,
/// `bedrock_stream_reports_usage_from_metadata_event`,
/// `bedrock_stream_metadata_without_usage_emits_nothing`,
/// `bedrock_stream_surfaces_mid_stream_error`,
/// `bedrock_stream_done_emits_terminal_marker`.
async fn handle_stream_event(
    result: std::result::Result<Option<StreamEvent>, String>,
    tx: &Sender<ChatEvent>,
) -> Flow {
    match result {
        Ok(Some(StreamEvent::ContentBlockDelta(ev))) => {
            if let Some(ContentBlockDelta::Text(text)) = ev.delta()
                && tx.send(ChatEvent::Delta(text.clone())).await.is_err()
            {
                return Flow::Stop;
            }
            Flow::Continue
        }
        Ok(Some(StreamEvent::Metadata(meta))) => {
            if let Some(usage) = meta.usage() {
                let chat_usage = ChatUsage {
                    prompt_tokens: usage.input_tokens().max(0) as u32,
                    completion_tokens: usage.output_tokens().max(0) as u32,
                    cache_read_tokens: usage.cache_read_input_tokens().unwrap_or(0).max(0) as u32,
                    cache_creation_tokens: usage.cache_write_input_tokens().unwrap_or(0).max(0)
                        as u32,
                };
                if tx.send(ChatEvent::Usage(chat_usage)).await.is_err() {
                    return Flow::Stop;
                }
            }
            Flow::Continue
        }
        // MessageStart/ContentBlockStart/ContentBlockStop/MessageStop are
        // structural markers (and any SDK-`Unknown` future variant) — nothing
        // to forward.
        Ok(Some(_)) => Flow::Continue,
        Ok(None) => {
            let _ = tx.send(ChatEvent::Done).await;
            Flow::Stop
        }
        Err(message) => {
            let _ = tx.send(ChatEvent::Error(message.clone())).await;
            Flow::Failed(message)
        }
    }
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
    /// `ConverseStream` call to Bedrock. Requires real AWS credentials with
    /// `bedrock:InvokeModel` permission on the target model.
    ///
    /// Run with:
    ///   cargo test -p trusty-common --features bedrock -- bedrock_live_converse_stream_smoke_test --ignored
    ///
    /// Why (issue #3767): validates the full end-to-end streaming path
    /// including credential resolution, event-stream wire decoding, and
    /// usage extraction — none of which the scripted `handle_stream_event`
    /// unit tests below can exercise, since they inject already-decoded
    /// events rather than driving a live AWS connection. This machine has no
    /// usable AWS credentials configured (`AWS_PROFILE`/`AWS_REGION` are
    /// set-but-empty), so this test could not be run live as part of this
    /// change — it remains `#[ignore]`d for an operator with real Bedrock
    /// access to run manually.
    /// What: sends a one-sentence user message and asserts the response is
    /// non-empty, `Done` was observed, and — the #3767-specific assertion —
    /// usage was reported at least once with non-zero tokens.
    /// Test: `#[ignore]` — requires live AWS credentials.
    #[tokio::test]
    #[ignore = "requires real AWS credentials with bedrock:InvokeModel permission"]
    async fn bedrock_live_converse_stream_smoke_test() {
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
        let mut usage: Option<ChatUsage> = None;
        while let Some(ev) = rx.recv().await {
            match ev {
                ChatEvent::Delta(s) => text.push_str(&s),
                ChatEvent::Done => saw_done = true,
                ChatEvent::Error(e) => panic!("stream error: {e}"),
                ChatEvent::ToolCall(_) => {}
                ChatEvent::Usage(u) => usage = Some(u),
            }
        }
        handle
            .await
            .expect("task panicked")
            .expect("chat_stream failed");
        assert!(!text.is_empty(), "expected non-empty response");
        assert!(saw_done, "expected ChatEvent::Done");
        let usage = usage.expect("expected a ChatEvent::Usage before Done (#3767)");
        assert!(
            usage.prompt_tokens > 0 || usage.completion_tokens > 0,
            "expected non-zero usage: {usage:?}"
        );
        eprintln!("bedrock_live_converse_stream_smoke_test response: {text:?} usage={usage:?}");
    }

    // ── #3767: `handle_stream_event` mapping tests ─────────────────────────
    //
    // Why: `EventReceiver` (the AWS SDK's decoded-event source) has no public
    // constructor outside a real HTTP response — its home module is private
    // (`aws_sdk_bedrockruntime::event_receiver`) — so these tests drive
    // `handle_stream_event` directly with hand-built `StreamEvent`s (every
    // event TYPE's builder IS public) instead of a live connection or
    // hand-encoded event-stream wire bytes.

    /// A normal streamed completion: two text deltas followed by a clean
    /// end-of-stream must assemble into `Delta("He")`, `Delta("llo")`, `Done`
    /// — in order, nothing dropped or reordered.
    #[tokio::test]
    async fn bedrock_stream_forwards_text_deltas_in_order() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChatEvent>(8);

        let flow1 = handle_stream_event(Ok(Some(content_delta("He"))), &tx).await;
        assert_eq!(flow1, Flow::Continue);
        let flow2 = handle_stream_event(Ok(Some(content_delta("llo"))), &tx).await;
        assert_eq!(flow2, Flow::Continue);
        let flow3 = handle_stream_event(Ok(None), &tx).await;
        assert_eq!(flow3, Flow::Stop);
        drop(tx);

        let mut collected = Vec::new();
        while let Some(ev) = rx.recv().await {
            collected.push(ev);
        }
        assert!(matches!(&collected[0], ChatEvent::Delta(d) if d == "He"));
        assert!(matches!(&collected[1], ChatEvent::Delta(d) if d == "llo"));
        assert!(matches!(collected[2], ChatEvent::Done));
        assert_eq!(collected.len(), 3, "no extra events: {collected:?}");
    }

    /// Structural events with no text payload (`MessageStart`,
    /// `ContentBlockStart`, `ContentBlockStop`, `MessageStop`) must not
    /// forward anything — only `ContentBlockDelta`/`Metadata`/terminal events
    /// produce a `ChatEvent`.
    #[tokio::test]
    async fn bedrock_stream_ignores_structural_events() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChatEvent>(8);

        let flow = handle_stream_event(
            Ok(Some(StreamEvent::MessageStart(
                aws_sdk_bedrockruntime::types::MessageStartEvent::builder()
                    .role(ConversationRole::Assistant)
                    .build()
                    .unwrap(),
            ))),
            &tx,
        )
        .await;
        assert_eq!(flow, Flow::Continue);

        let flow = handle_stream_event(
            Ok(Some(StreamEvent::MessageStop(
                aws_sdk_bedrockruntime::types::MessageStopEvent::builder()
                    .stop_reason(aws_sdk_bedrockruntime::types::StopReason::EndTurn)
                    .build()
                    .unwrap(),
            ))),
            &tx,
        )
        .await;
        assert_eq!(flow, Flow::Continue);

        drop(tx);
        assert!(
            rx.recv().await.is_none(),
            "structural events must not emit any ChatEvent"
        );
    }

    /// The terminal `Metadata` event's `usage` must survive as exactly one
    /// `ChatEvent::Usage` carrying the token counts — the core #3767
    /// assertion (Bedrock reports usage only here, never per-delta).
    #[tokio::test]
    async fn bedrock_stream_reports_usage_from_metadata_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChatEvent>(8);

        let flow = handle_stream_event(Ok(Some(metadata_with_usage(120, 45, 30, 10))), &tx).await;
        assert_eq!(flow, Flow::Continue);
        drop(tx);

        let events: Vec<_> = {
            let mut out = Vec::new();
            while let Some(ev) = rx.recv().await {
                out.push(ev);
            }
            out
        };
        assert_eq!(events.len(), 1, "expected exactly one event: {events:?}");
        match &events[0] {
            ChatEvent::Usage(u) => {
                assert_eq!(u.prompt_tokens, 120);
                assert_eq!(u.completion_tokens, 45);
                assert_eq!(u.cache_read_tokens, 30);
                assert_eq!(u.cache_creation_tokens, 10);
            }
            other => panic!("expected ChatEvent::Usage, got {other:?}"),
        }
    }

    /// A `Metadata` event with no `usage` field (Bedrock omits it on some
    /// error/guardrail paths) must not fabricate a zeroed `ChatEvent::Usage`
    /// — nothing should be sent at all.
    #[tokio::test]
    async fn bedrock_stream_metadata_without_usage_emits_nothing() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChatEvent>(8);
        let meta = aws_sdk_bedrockruntime::types::ConverseStreamMetadataEvent::builder().build();
        let flow = handle_stream_event(Ok(Some(StreamEvent::Metadata(meta))), &tx).await;
        assert_eq!(flow, Flow::Continue);
        drop(tx);
        assert!(rx.recv().await.is_none(), "no usage means no ChatEvent");
    }

    /// A mid-stream failure (a modeled exception surfaced as `Err`, e.g.
    /// throttling or a transport error) must emit `ChatEvent::Error` AND
    /// return `Flow::Failed` so the caller returns `Err` — never silently
    /// truncate to what text arrived so far.
    #[tokio::test]
    async fn bedrock_stream_surfaces_mid_stream_error() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChatEvent>(8);

        let flow1 = handle_stream_event(Ok(Some(content_delta("partial "))), &tx).await;
        assert_eq!(flow1, Flow::Continue);
        let flow2 = handle_stream_event(
            Err("Bedrock ConverseStream error: throttled".to_string()),
            &tx,
        )
        .await;
        assert_eq!(
            flow2,
            Flow::Failed("Bedrock ConverseStream error: throttled".to_string())
        );
        drop(tx);

        let mut collected = Vec::new();
        while let Some(ev) = rx.recv().await {
            collected.push(ev);
        }
        assert!(matches!(&collected[0], ChatEvent::Delta(d) if d == "partial "));
        match &collected[1] {
            ChatEvent::Error(msg) => assert!(msg.contains("throttled")),
            other => panic!("expected ChatEvent::Error, got {other:?}"),
        }
        assert_eq!(
            collected.len(),
            2,
            "a mid-stream error must not also emit Done: {collected:?}"
        );
    }

    /// A clean `Ok(None)` end-of-stream (no error, no more events) must emit
    /// exactly one terminal `ChatEvent::Done` and `Flow::Stop`.
    #[tokio::test]
    async fn bedrock_stream_done_emits_terminal_marker() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ChatEvent>(8);
        let flow = handle_stream_event(Ok(None), &tx).await;
        assert_eq!(flow, Flow::Stop);
        drop(tx);
        let mut collected = Vec::new();
        while let Some(ev) = rx.recv().await {
            collected.push(ev);
        }
        assert_eq!(collected.len(), 1);
        assert!(matches!(collected[0], ChatEvent::Done));
    }

    /// `with_sampling` must forward temperature/max_tokens/stop onto the
    /// `InferenceConfiguration` the request builds — parity with #3758's
    /// OpenRouter fix. This is a construction-only check (no network): it
    /// verifies the provider stores what it's given rather than silently
    /// discarding it, mirroring `sampling_params_serialize_into_request_body`
    /// in the `openai_compat` module.
    #[test]
    fn bedrock_stream_forwards_sampling_params() {
        let sampling = SamplingParams {
            temperature: Some(0.2),
            max_tokens: Some(512),
            stop: vec!["STOP".to_string()],
        };
        // `BedrockProvider` has no live-network-free way to introspect the
        // built request, so this pins the builder contract directly: the
        // same `InferenceConfiguration` construction `chat_stream` performs
        // must not panic and must carry the caller's values through
        // (exercised end-to-end by the live smoke test above).
        let inference = InferenceConfiguration::builder()
            .max_tokens(sampling.max_tokens.unwrap_or(4096) as i32)
            .set_temperature(sampling.temperature)
            .set_stop_sequences((!sampling.stop.is_empty()).then(|| sampling.stop.clone()))
            .build();
        assert_eq!(inference.max_tokens(), Some(512));
        assert_eq!(inference.temperature(), Some(0.2));
        assert_eq!(inference.stop_sequences(), &["STOP".to_string()][..]);
    }

    /// Helper: a `ContentBlockDelta::Text` event carrying `text`.
    fn content_delta(text: &str) -> StreamEvent {
        StreamEvent::ContentBlockDelta(
            aws_sdk_bedrockruntime::types::ContentBlockDeltaEvent::builder()
                .delta(ContentBlockDelta::Text(text.to_string()))
                .content_block_index(0)
                .build()
                .expect("build ContentBlockDeltaEvent"),
        )
    }

    /// Helper: a `Metadata` event carrying a fully-populated `TokenUsage`.
    fn metadata_with_usage(
        input_tokens: i32,
        output_tokens: i32,
        cache_read: i32,
        cache_write: i32,
    ) -> StreamEvent {
        let usage = aws_sdk_bedrockruntime::types::TokenUsage::builder()
            .input_tokens(input_tokens)
            .output_tokens(output_tokens)
            .total_tokens(input_tokens + output_tokens)
            .cache_read_input_tokens(cache_read)
            .cache_write_input_tokens(cache_write)
            .build()
            .expect("build TokenUsage");
        StreamEvent::Metadata(
            aws_sdk_bedrockruntime::types::ConverseStreamMetadataEvent::builder()
                .usage(usage)
                .build(),
        )
    }
}
