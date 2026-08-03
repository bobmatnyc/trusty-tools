//! AWS Bedrock Converse API inference adapter (epic #2400 Wave 2, #2407).
//!
//! Why: Bedrock speaks the Anthropic-dialect Converse API (IAM-based auth, no
//! API key, private-VPC deployments), not the OpenAI-compatible
//! `/chat/completions` schema the [`super::providers::OpenAiCompatAdapter`]
//! family shares — so it needs its own [`InferenceAdapter`] implementation. This
//! module is that adapter, ported from tcode's WORKING `llm::bedrock` transport
//! (#2407) so the M3 bake-off `bedrock/us.anthropic.claude-sonnet-4-6` path is
//! preserved with no regression: the wire-format conversion (`convert`) and the
//! `cachePoint` prompt-cache translation (`cache`) are byte-for-byte the proven
//! logic, retargeted only from tcode's wire types to
//! [`crate::inference::types`] and from `LlmError` to [`InferenceError`].
//! What: [`BedrockAdapter`] wraps a lazily-constructed
//! `aws_sdk_bedrockruntime::Client` (so a `Configurator` that merely registers
//! the factory never touches AWS credentials — #2245) and implements
//! [`InferenceAdapter`]: `chat` converts the request via [`build_converse_parts`]
//! (which funnels [`convert::build_converse_messages`]/
//! [`convert::build_tool_config`]), calls the unary `Converse` operation, maps
//! SDK errors to [`InferenceError::Provider`], and converts the response back via
//! [`convert::converse_output_to_chat_response`]; `chat_stream` (#4426) sends the
//! SAME converted request to `ConverseStream` instead and maps its binary
//! event-stream framing into the neutral [`ChatStream`] via [`stream`], so a
//! `bedrock/*` turn streams token-by-token rather than falling back to the
//! trait's buffered default; `map_tool_choice` emits Converse's own tool-choice
//! JSON shape. Region resolves `TRUSTY_AWS_REGION` > `AWS_REGION` > `us-east-1`;
//! credentials come from the standard AWS credential chain (env,
//! `~/.aws/credentials`, IMDS, SSO — so `AWS_PROFILE=cto` works with zero code
//! changes). [`register_bedrock_factory`] wires the adapter into a
//! [`Configurator`] under [`ProviderId::Bedrock`].
//! Test: `super::tests::*` (region resolution, message/tool-choice/response
//! conversion, and the `stream_*` suite driving the real streaming path against
//! a scripted `ConverseStream` event sequence — all offline, no real AWS) plus
//! `#[ignore]`-gated live `Converse` and `ConverseStream` calls; the configurator
//! wiring is covered by `crates/trusty-common/tests/inference_bedrock.rs`.

pub(crate) mod cache;
pub(crate) mod convert;
pub(crate) mod stream;

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_bedrockruntime::Client as BedrockRuntimeClient;
use aws_sdk_bedrockruntime::types::{
    InferenceConfiguration, Message, SystemContentBlock, ToolConfiguration,
};
use aws_smithy_types::error::display::DisplayErrorContext;
use aws_types::region::Region;
use serde_json::{Value, json};
use tokio::sync::OnceCell;

use crate::inference::adapter::InferenceAdapter;
use crate::inference::configurator::{Configurator, ResolvedProvider};
use crate::inference::error::InferenceError;
use crate::inference::registry::{ProviderCapabilities, ProviderId, capabilities};
use crate::inference::streaming::ChatStream;
use crate::inference::types::{ChatRequest, ChatResponse, ToolChoice};

/// Region env var: trusty-specific override (checked before the standard
/// `AWS_REGION`).
const ENV_REGION_TRUSTY: &str = "TRUSTY_AWS_REGION";
/// Region env var: standard AWS fallback.
const ENV_REGION_AWS: &str = "AWS_REGION";
/// Default AWS region when neither env var is set.
const DEFAULT_REGION: &str = "us-east-1";

/// Resolve the AWS region for the Bedrock client.
///
/// Why: operators may specify region via either `TRUSTY_AWS_REGION`
/// (trusty-specific) or `AWS_REGION` (standard); the trusty var takes precedence.
/// What: returns the first non-empty value, in priority order: `explicit`, then
/// `TRUSTY_AWS_REGION`, then `AWS_REGION`, else `"us-east-1"`.
/// Test: `super::tests::region_resolution_*`.
pub(crate) fn resolve_bedrock_region(explicit: Option<&str>) -> String {
    if let Some(r) = explicit.filter(|s| !s.is_empty()) {
        return r.to_string();
    }
    for var in [ENV_REGION_TRUSTY, ENV_REGION_AWS] {
        if let Ok(val) = std::env::var(var) {
            let val = val.trim().to_string();
            if !val.is_empty() {
                return val;
            }
        }
    }
    DEFAULT_REGION.to_string()
}

/// AWS Bedrock Converse API inference adapter.
///
/// Why: satisfies the shared [`InferenceAdapter`] contract so the configurator,
/// the agent loop, trusty-review, and tga can drive a `bedrock/*` model through
/// `Box<dyn InferenceAdapter>` identically to any OpenAI-dialect provider — the
/// Converse mechanics stay entirely inside this adapter.
/// What: holds the resolved region, a lazily-built `BedrockRuntimeClient` (a
/// `tokio::sync::OnceCell`, so construction touches no AWS credentials until the
/// first `chat` — a `Configurator` that merely registers the factory never hits
/// the AWS SDK, #2245), and the Bedrock [`ProviderCapabilities`].
/// Test: `super::tests::*`.
#[derive(Debug)]
pub struct BedrockAdapter {
    region: String,
    client: OnceCell<BedrockRuntimeClient>,
    capabilities: ProviderCapabilities,
}

impl BedrockAdapter {
    /// Construct a `BedrockAdapter` for the resolved (or defaulted) region.
    ///
    /// Why: construction must be synchronous and credential-free so an
    /// [`AdapterFactory`](crate::inference::AdapterFactory) closure can build it
    /// without an async context and a pure-OpenRouter run never touches AWS.
    /// The AWS client (whose config load is async and may touch the filesystem
    /// or IMDS) is deferred to the first [`Self::chat`] via the internal
    /// `OnceCell`.
    /// What: resolves the region (`region` > `TRUSTY_AWS_REGION` > `AWS_REGION` >
    /// `us-east-1`) and stores an empty client cell plus the Bedrock
    /// capabilities. Never fails.
    /// Test: `super::tests::region_resolution_*`,
    /// `super::tests::new_does_not_touch_aws`.
    pub fn new(region: Option<&str>) -> Self {
        Self {
            region: resolve_bedrock_region(region),
            client: OnceCell::new(),
            capabilities: *capabilities(ProviderId::Bedrock),
        }
    }

    /// The AWS region this adapter is configured for.
    ///
    /// Why: exposed for diagnostics and the region-resolution tests.
    /// What: the resolved region string.
    /// Test: `super::tests::region_resolution_*`.
    pub fn region(&self) -> &str {
        &self.region
    }

    /// Get (or lazily build) the Bedrock Converse client.
    ///
    /// Why: AWS credential/region resolution is async and must never run for an
    /// adapter that is built but never used; `OnceCell::get_or_try_init`
    /// guarantees at most one build and lets a failed attempt be retried on the
    /// next `chat` rather than poisoning the adapter.
    /// What: loads AWS config pinned to [`Self::region`] via the standard
    /// credential chain and builds a `BedrockRuntimeClient` on first use.
    /// Test: exercised indirectly by the `#[ignore]`-gated live Converse call.
    async fn client(&self) -> Result<&BedrockRuntimeClient, InferenceError> {
        self.client
            .get_or_try_init(|| async {
                let config = aws_config::defaults(BehaviorVersion::latest())
                    .region(RegionProviderChain::first_try(Region::new(
                        self.region.clone(),
                    )))
                    .load()
                    .await;
                Ok::<_, InferenceError>(BedrockRuntimeClient::new(&config))
            })
            .await
    }
}

#[async_trait]
impl InferenceAdapter for BedrockAdapter {
    /// The stable provider name (`"bedrock"`).
    fn name(&self) -> &str {
        ProviderId::Bedrock.as_str()
    }

    /// The Bedrock capability descriptor.
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    /// Execute one Converse call and return the normalized response.
    ///
    /// Why: the one method the whole agent loop depends on; all Converse
    /// mechanics (message conversion, tool config, region/auth, SDK-error
    /// classification) live here so callers only speak [`ChatRequest`]/
    /// [`ChatResponse`].
    /// What: converts `request` via [`convert::build_converse_messages`] and
    /// [`convert::build_tool_config`], sends the Converse request (model id =
    /// [`bedrock_model_id`] applied to `request.model`, e.g.
    /// `us.anthropic.claude-sonnet-4-6` with the `bedrock/` routing prefix
    /// stripped), maps SDK errors to [`InferenceError::Provider`] with full
    /// source-chain context via `DisplayErrorContext`, and converts the response
    /// back via [`convert::converse_output_to_chat_response`] (which keeps the
    /// FULL `request.model` slug, prefix included, for telemetry readability —
    /// only the value sent to AWS is stripped).
    /// Test: `super::tests::*` cover the conversion helpers directly; the
    /// `#[ignore]`-gated `live_bedrock_call` exercises this end-to-end.
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let client = self.client().await?;
        let parts = build_converse_parts(request)?;

        let mut sdk_req = client
            .converse()
            .model_id(bedrock_model_id(&request.model))
            .inference_config(parts.inference)
            .set_messages(Some(parts.messages));

        if !parts.system.is_empty() {
            sdk_req = sdk_req.set_system(Some(parts.system));
        }
        if let Some(tool_config) = parts.tool_config {
            sdk_req = sdk_req.tool_config(tool_config);
        }

        let resp = sdk_req.send().await.map_err(|e| {
            InferenceError::Provider(format!(
                "Converse call failed (model={}, region={}): {}",
                request.model,
                self.region,
                DisplayErrorContext(&e)
            ))
        })?;

        Ok(convert::converse_output_to_chat_response(
            &resp,
            &request.model,
        ))
    }

    /// Execute one `ConverseStream` call and yield its events incrementally
    /// (#4426).
    ///
    /// Why: without this override a `bedrock/*` turn used the trait's buffered
    /// default — the caller waited for the whole answer and then received it as
    /// a single delta, which is indistinguishable from not streaming at all.
    /// Bedrock's own streaming operation has been proven in this workspace since
    /// #3767 (`crate::chat::bedrock_impl`, live on trusty-agents' chat path);
    /// this is that capability moved onto [`InferenceAdapter`], the surface epic
    /// #4429 is converging on. Every consumer of the shared adapter — trusty-code
    /// foremost, which delegates `chat_stream` straight through — gets real token
    /// streaming with no change of its own.
    /// What: converts `request` with the SAME [`build_converse_parts`] the unary
    /// [`Self::chat`] uses (so the two transports can never disagree about
    /// messages, sampling, or tool config), sends `ConverseStream`, and returns
    /// the SDK's event receiver wrapped as a [`ChatStream`] via
    /// [`stream::drive`]. A failed `stream=true` handshake surfaces
    /// SYNCHRONOUSLY as `Err` (per the trait contract, so the caller may choose
    /// to retry non-streaming); a mid-stream failure surfaces as a terminal `Err`
    /// item and never as a `Done`. Dropping the returned stream drops the
    /// receiver, cancelling the AWS request.
    /// Test: `super::tests::stream_*` drive [`stream::drive`] over a scripted
    /// event sequence; `super::tests::live_bedrock_converse_stream` (`#[ignore]`)
    /// covers this method end to end against the real service.
    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatStream, InferenceError> {
        let client = self.client().await?;
        let parts = build_converse_parts(request)?;

        let mut sdk_req = client
            .converse_stream()
            .model_id(bedrock_model_id(&request.model))
            .inference_config(parts.inference)
            .set_messages(Some(parts.messages));

        if !parts.system.is_empty() {
            sdk_req = sdk_req.set_system(Some(parts.system));
        }
        if let Some(tool_config) = parts.tool_config {
            sdk_req = sdk_req.tool_config(tool_config);
        }

        let output = sdk_req.send().await.map_err(|e| {
            InferenceError::Provider(format!(
                "ConverseStream call failed (model={}, region={}): {}",
                request.model,
                self.region,
                DisplayErrorContext(&e)
            ))
        })?;

        Ok(stream::drive(stream::SdkEventSource::new(
            output,
            &request.model,
            &self.region,
        )))
    }

    /// Translate a neutral [`ToolChoice`] into Converse's own tool-choice JSON
    /// shape.
    ///
    /// Why: Converse's `toolChoice` is `{"auto":{}}` / `{"any":{}}` /
    /// `{"tool":{"name":...}}` — structurally different from the OpenAI dialect
    /// the [`InferenceAdapter::map_tool_choice`] default produces.
    /// [`convert::build_tool_config`] accepts BOTH this shape and the OpenAI
    /// shape, so this mapping is what a dialect-aware caller uses.
    /// What: `None` -> `"none"` (a sentinel string; Converse has no "don't call
    /// any tool" choice, so [`convert::build_tool_config`] omits `toolConfig`
    /// entirely when it sees this). `Auto` -> `{"auto":{}}`. `Required` ->
    /// `{"any":{}}` (Converse's "must call some tool"). `Function(name)` ->
    /// `{"tool":{"name":name}}`.
    /// Test: `super::tests::map_tool_choice_*`.
    fn map_tool_choice(&self, choice: ToolChoice) -> Value {
        match choice {
            ToolChoice::None => json!("none"),
            ToolChoice::Auto => json!({"auto": {}}),
            ToolChoice::Required => json!({"any": {}}),
            ToolChoice::Function(name) => json!({"tool": {"name": name}}),
        }
    }
}

/// One [`ChatRequest`] converted into the four pieces both Converse operations
/// take (#4426).
///
/// Why: `Converse` and `ConverseStream` have DIFFERENT fluent-builder types, so
/// the request assembly cannot be shared by passing a builder around — but the
/// conversion itself must be shared, or the streaming and buffered transports
/// silently drift (a `cache_control` marker, a tool-pairing repair, or a
/// sampling knob honoured on one path and not the other is exactly the class of
/// bug that makes "streaming broke tool calls" reports). Producing the pieces
/// once and letting each caller mount them on its own builder keeps a single
/// conversion with two call sites.
/// What: `system` is Converse's system-prompt array (empty when the transcript
/// has no system content); `messages` is the alternation-safe, tool-pairing-
/// repaired conversation; `inference` carries `max_tokens`/`temperature`;
/// `tool_config` is `None` when the request declares no tools (or when the
/// tool-choice mapping says to omit it entirely).
/// Test: covered through both call sites — `super::tests::*` conversion tests
/// for the pieces and `super::tests::stream_*` for the streaming mount.
pub(crate) struct ConverseParts {
    /// Converse's top-level system-prompt blocks.
    pub(crate) system: Vec<SystemContentBlock>,
    /// The user/assistant conversation, role-merged and tool-pairing-repaired.
    pub(crate) messages: Vec<Message>,
    /// Sampling configuration (`max_tokens`, `temperature`).
    pub(crate) inference: InferenceConfiguration,
    /// Tool definitions + tool choice, when the request declares any.
    pub(crate) tool_config: Option<ToolConfiguration>,
}

/// Convert a [`ChatRequest`] into the shared Converse request pieces.
///
/// Why: see [`ConverseParts`] — this is the ONE conversion both
/// [`BedrockAdapter::chat`] and [`BedrockAdapter::chat_stream`] run, so the two
/// transports are guaranteed to send the same thing.
/// What: delegates messages/system to [`convert::build_converse_messages`] and
/// tools to [`convert::build_tool_config`] (only when `request.tools` is set),
/// and builds the [`InferenceConfiguration`] from `max_tokens`/`temperature`
/// via `set_*` so an absent knob omits the field rather than sending a default.
/// Errors propagate from the converters (an unrepresentable message or an
/// invalid tool schema).
/// Test: `super::tests::*` (message/tool conversion) and
/// `super::tests::stream_*`.
pub(crate) fn build_converse_parts(request: &ChatRequest) -> Result<ConverseParts, InferenceError> {
    let (system, messages) = convert::build_converse_messages(request)?;

    let inference = InferenceConfiguration::builder()
        .set_max_tokens(request.max_tokens.map(|v| v as i32))
        .set_temperature(request.temperature)
        .build();

    let tool_config = match &request.tools {
        Some(tools) => convert::build_tool_config(tools, request.tool_choice.as_ref())?,
        None => None,
    };

    Ok(ConverseParts {
        system,
        messages,
        inference,
        tool_config,
    })
}

/// Strip the `bedrock/` dispatch-routing prefix from a model slug, yielding the
/// bare id the Converse API's `model_id` parameter expects.
///
/// Why: `request.model` carries the FULL dispatch slug (e.g.
/// `bedrock/us.anthropic.claude-sonnet-4-6`) so provider resolution can
/// pattern-match the `bedrock/` prefix — but AWS Bedrock's Converse `model_id`
/// rejects that prefixed form outright (`ValidationException: The provided model
/// identifier is invalid`, confirmed live), while the bare id succeeds.
/// [`BedrockAdapter::chat`] calls this right before `.model_id(...)` so the value
/// sent to AWS is correct regardless of what any caller passes.
/// #4493: this was one of two hand-rolled per-provider copies of that rule
/// (Anthropic-direct had the other) while the OpenAI-dialect providers had none
/// — the reason `openai/gpt-4o-mini` reached `api.openai.com` prefixed. It now
/// delegates to the ONE shared implementation so the three cannot drift.
/// What: forwards to [`ProviderId::wire_model_id`] for [`ProviderId::Bedrock`],
/// which returns `slug` with a leading `"bedrock/"` removed, or `slug` unchanged
/// when there is no such prefix (defensive passthrough — a caller that already
/// hands over a bare id must not be mangled).
/// Test: `super::tests::bedrock_model_id_strips_prefix`,
/// `super::tests::bedrock_model_id_passthrough_without_prefix`.
pub(crate) fn bedrock_model_id(slug: &str) -> &str {
    ProviderId::Bedrock.wire_model_id(slug)
}

/// Build a Bedrock adapter for a resolved provider.
///
/// Why: the single construction path the factory funnels through. Bedrock
/// resolves with NO key (the AWS credential chain, not a [`KeyStore`] secret), so
/// unlike the keyed OpenAI-dialect factories this ignores `resolved.key()`
/// entirely.
/// What: builds a [`BedrockAdapter`] against the ambient region
/// (`TRUSTY_AWS_REGION`/`AWS_REGION`/default). Infallible — the AWS client is
/// constructed lazily on first `chat`.
/// Test: `crates/trusty-common/tests/inference_bedrock.rs`.
///
/// [`KeyStore`]: crate::credentials::KeyStore
pub fn build(resolved: &ResolvedProvider) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    debug_assert_eq!(resolved.provider(), ProviderId::Bedrock);
    Ok(Box::new(BedrockAdapter::new(None)))
}

/// Production factory: build a Bedrock adapter for a resolved provider.
///
/// Why: this is what [`register_bedrock_factory`] registers into a
/// [`Configurator`] so `build("bedrock/<slug>", &store)` yields a live Bedrock
/// adapter.
/// What: delegates to [`build`].
/// Test: `crates/trusty-common/tests/inference_bedrock.rs`.
pub fn factory(resolved: &ResolvedProvider) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    build(resolved)
}

/// Register the Bedrock adapter factory into `cfg` under [`ProviderId::Bedrock`].
///
/// Why: Bedrock is deliberately NOT part of
/// [`super::providers::register_default_factories`] (that entry point is the
/// OpenAI-dialect family). A consumer that wants Bedrock opts in with this one
/// explicit call, keeping the Anthropic-dialect wiring additive and separate
/// from the OpenAI-dialect registration seam (#2407/#2408).
/// What: registers [`factory`] under [`ProviderId::Bedrock`]; a later
/// registration for Bedrock replaces it.
/// Test: `crates/trusty-common/tests/inference_bedrock.rs::bedrock_factory_registers_and_builds`.
pub fn register_bedrock_factory(cfg: &mut Configurator) {
    cfg.register(ProviderId::Bedrock, Box::new(factory));
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
