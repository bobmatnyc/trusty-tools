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
//! [`InferenceAdapter`]: `chat` converts the request via
//! [`convert::build_converse_messages`]/[`convert::build_tool_config`], calls
//! `Converse` (non-streaming), maps SDK errors to [`InferenceError::Provider`],
//! and converts the response back via [`convert::converse_output_to_chat_response`];
//! `map_tool_choice` emits Converse's own tool-choice JSON shape. Region resolves
//! `TRUSTY_AWS_REGION` > `AWS_REGION` > `us-east-1`; credentials come from the
//! standard AWS credential chain (env, `~/.aws/credentials`, IMDS, SSO — so
//! `AWS_PROFILE=cto` works with zero code changes). [`register_bedrock_factory`]
//! wires the adapter into a [`Configurator`] under [`ProviderId::Bedrock`].
//! Test: `super::tests::*` (region resolution, message/tool-choice/response
//! conversion — all offline, no real AWS) plus an `#[ignore]`-gated live
//! Converse call; the configurator wiring is covered by
//! `crates/trusty-common/tests/inference_bedrock.rs`.

pub(crate) mod cache;
pub(crate) mod convert;

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_bedrockruntime::Client as BedrockRuntimeClient;
use aws_sdk_bedrockruntime::types::InferenceConfiguration;
use aws_smithy_types::error::display::DisplayErrorContext;
use aws_types::region::Region;
use serde_json::{Value, json};
use tokio::sync::OnceCell;

use crate::inference::adapter::InferenceAdapter;
use crate::inference::configurator::{Configurator, ResolvedProvider};
use crate::inference::error::InferenceError;
use crate::inference::registry::{ProviderCapabilities, ProviderId, capabilities};
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
        let (system_blocks, messages) = convert::build_converse_messages(request)?;

        let inference = InferenceConfiguration::builder()
            .set_max_tokens(request.max_tokens.map(|v| v as i32))
            .set_temperature(request.temperature)
            .build();

        let mut sdk_req = client
            .converse()
            .model_id(bedrock_model_id(&request.model))
            .inference_config(inference)
            .set_messages(Some(messages));

        if !system_blocks.is_empty() {
            sdk_req = sdk_req.set_system(Some(system_blocks));
        }

        if let Some(tools) = &request.tools {
            let tool_config = convert::build_tool_config(tools, request.tool_choice.as_ref())?;
            if let Some(tool_config) = tool_config {
                sdk_req = sdk_req.tool_config(tool_config);
            }
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
/// What: returns `slug` with a leading `"bedrock/"` removed, or `slug` unchanged
/// when there is no such prefix (defensive passthrough — a caller that already
/// hands over a bare id must not be mangled).
/// Test: `super::tests::bedrock_model_id_strips_prefix`,
/// `super::tests::bedrock_model_id_passthrough_without_prefix`.
pub(crate) fn bedrock_model_id(slug: &str) -> &str {
    slug.strip_prefix("bedrock/").unwrap_or(slug)
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
/// [`KeyStore`]: crate::inference::credentials::KeyStore
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
