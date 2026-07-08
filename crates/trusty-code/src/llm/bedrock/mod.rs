//! AWS Bedrock Converse API transport for trusty-code (#1021 phase 1).
//!
//! Why: trusty-code's agent loop only ever speaks `ChatRequest`/`ChatResponse`
//! through `LlmClientTrait`; running on AWS Bedrock (IAM-based auth, no
//! OpenRouter API key, private VPC) needs a transport that fulfils that same
//! contract by calling the Bedrock Converse API instead of OpenRouter's HTTP
//! endpoint. This module is that transport — ported from the WORKING
//! reference implementation in `trusty-review::llm::bedrock` (region
//! resolution, default AWS credential chain, Converse call shape), adapted to
//! tcode's multi-turn, many-tool conversational shape rather than
//! trusty-review's single-shot structured-output-forcing use case.
//! What: [`BedrockChatClient`] wraps an `aws_sdk_bedrockruntime::Client`.
//! `chat` converts the request via [`convert::build_converse_messages`] and
//! [`convert::build_tool_config`], calls `Converse` (non-streaming), maps SDK
//! errors to [`LlmError::Bedrock`], and converts the response back via
//! [`convert::converse_output_to_chat_response`]. Region resolves
//! `TRUSTY_AWS_REGION` > `AWS_REGION` > `us-east-1`; credentials come from the
//! standard AWS credential chain (env vars, `~/.aws/credentials`, IMDS, SSO —
//! so `AWS_PROFILE=cto` works with zero code changes). (#2260) When
//! `req`'s messages/tools carry `build_request`'s cache markers,
//! [`convert::build_converse_messages`]/[`convert::build_tool_config`] also
//! emit Bedrock-native `cachePoint` breakpoints via `cache` — see that
//! module's doc for the wire-shape translation and minimum-size guard.
//! Test: `bedrock::tests::*` (region resolution, message/tool-choice/response
//! conversion — all unit-level, no real AWS calls) plus an `#[ignore]`-gated
//! live Converse call.

mod cache;
mod convert;

use aws_config::BehaviorVersion;
use aws_sdk_bedrockruntime::Client as BedrockRuntimeClient;

use super::{ChatRequest, ChatResponse, LlmError};

/// Region env var: trusty-specific override (checked before the standard
/// `AWS_REGION`).
const ENV_REGION_TRUSTY: &str = "TRUSTY_AWS_REGION";
/// Region env var: standard AWS fallback.
const ENV_REGION_AWS: &str = "AWS_REGION";
/// Default AWS region when neither env var is set.
const DEFAULT_REGION: &str = "us-east-1";

/// Resolve the AWS region for the Bedrock client.
///
/// Why: Operators may specify region via either `TRUSTY_AWS_REGION`
/// (trusty-specific) or `AWS_REGION` (standard); the trusty var takes
/// precedence, mirroring `trusty-review::llm::bedrock::resolve_bedrock_region`.
/// What: Returns the first non-empty value, in priority order: `explicit`,
/// then `TRUSTY_AWS_REGION`, then `AWS_REGION`, else `"us-east-1"`.
/// Test: `bedrock::tests::region_resolution_*`.
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

/// AWS Bedrock Converse API transport.
///
/// Why: Satisfies the same `chat(&ChatRequest) -> Result<ChatResponse, ..>`
/// contract every other LLM transport in this crate exposes, so
/// `DispatchingLlmClient` (`super::dispatch`) can route `bedrock/*` model
/// slugs here without `AgentLoop` or any other caller knowing the backend
/// differs.
/// What: Holds a pre-built `BedrockRuntimeClient` and the resolved region.
/// `chat` builds a Converse request from `ChatRequest` (messages, tools,
/// tool_choice), sends it, and maps the response back.
/// Test: `bedrock::tests::*`.
#[derive(Debug)]
pub struct BedrockChatClient {
    client: BedrockRuntimeClient,
    region: String,
}

impl BedrockChatClient {
    /// Construct a `BedrockChatClient` using the standard AWS credential chain.
    ///
    /// Why: The AWS SDK's default chain handles env vars
    /// (`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`/`AWS_SESSION_TOKEN`),
    /// `~/.aws/credentials` profiles (including `AWS_PROFILE`), IMDSv2, and
    /// SSO — covering local dev and production deployments with zero code
    /// changes.
    /// What: Resolves the region (`explicit` > `TRUSTY_AWS_REGION` >
    /// `AWS_REGION` > `us-east-1`), loads AWS config pinned to that region,
    /// and builds a `BedrockRuntimeClient`. Async because credential
    /// resolution may touch the filesystem or IMDS.
    /// Test: `bedrock::tests::region_resolution_*` (no network); the
    /// real-credentials path is exercised by the `#[ignore]`-gated live test.
    pub async fn new(region: Option<&str>) -> Result<Self, LlmError> {
        let region_str = resolve_bedrock_region(region);
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(aws_config::meta::region::RegionProviderChain::first_try(
                aws_types::region::Region::new(region_str.clone()),
            ))
            .load()
            .await;
        Ok(Self {
            client: BedrockRuntimeClient::new(&config),
            region: region_str,
        })
    }

    /// Construct from the environment (`TRUSTY_AWS_REGION`/`AWS_REGION`, no
    /// explicit override).
    ///
    /// Why: Convenience entry point for `DispatchingLlmClient`'s lazy
    /// construction — it never has a per-call region override to pass.
    /// What: Delegates to [`Self::new`] with `region: None`.
    /// Test: Exercised indirectly via `dispatch::tests`.
    pub async fn from_env() -> Result<Self, LlmError> {
        Self::new(None).await
    }

    /// The AWS region this client is configured for.
    pub fn region(&self) -> &str {
        &self.region
    }

    /// Execute one Converse call and return the response.
    ///
    /// Why: The single method `DispatchingLlmClient` needs to satisfy
    /// `LlmClientTrait::chat` for `bedrock/*` model slugs.
    /// What: Converts `req` via [`convert::build_converse_messages`] and
    /// [`convert::build_tool_config`], sends the Converse request (model id =
    /// [`bedrock_model_id`] applied to `req.model`, e.g.
    /// `us.anthropic.claude-sonnet-4-6` with the `bedrock/` routing prefix
    /// stripped — see that function's doc for why this is the single place
    /// the strip happens), maps SDK errors to `LlmError::Bedrock` with full
    /// source-chain context via `DisplayErrorContext`, and converts the
    /// response back via [`convert::converse_output_to_chat_response`] (which
    /// keeps the FULL `req.model` slug, prefix included, for telemetry
    /// readability — only the value sent to AWS is stripped).
    /// Test: `bedrock::tests::*` cover the conversion helpers directly; the
    /// `#[ignore]`-gated `live_bedrock_call` test exercises this end-to-end.
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let (system_blocks, messages) = convert::build_converse_messages(req)?;

        let inference = aws_sdk_bedrockruntime::types::InferenceConfiguration::builder()
            .set_max_tokens(req.max_tokens.map(|v| v as i32))
            .set_temperature(req.temperature)
            .build();

        let mut sdk_req = self
            .client
            .converse()
            .model_id(bedrock_model_id(&req.model))
            .inference_config(inference)
            .set_messages(Some(messages));

        if !system_blocks.is_empty() {
            sdk_req = sdk_req.set_system(Some(system_blocks));
        }

        if let Some(tools) = &req.tools {
            let tool_config = convert::build_tool_config(tools, req.tool_choice.as_ref())?;
            if let Some(tool_config) = tool_config {
                sdk_req = sdk_req.tool_config(tool_config);
            }
        }

        let resp = sdk_req.send().await.map_err(|e| {
            LlmError::Bedrock(format!(
                "Converse call failed (model={}, region={}): {}",
                req.model,
                self.region,
                aws_smithy_types::error::display::DisplayErrorContext(&e)
            ))
        })?;

        Ok(convert::converse_output_to_chat_response(&resp, &req.model))
    }
}

/// Strip the `bedrock/` dispatch-routing prefix from a model slug, yielding
/// the bare id the Converse API's `model_id` parameter expects.
///
/// Why: `req.model` carries the FULL dispatch slug (e.g.
/// `bedrock/us.anthropic.claude-sonnet-4-6`) so `provider::provider_for` and
/// `DispatchingLlmClient::routes_to_bedrock` can pattern-match the `bedrock/`
/// prefix to pick this transport — but AWS Bedrock's Converse `model_id`
/// rejects that prefixed form outright: confirmed live against the `cto`
/// account/`us-east-1`, `aws bedrock-runtime converse --model-id
/// "bedrock/us.anthropic.claude-sonnet-4-6"` returns `ValidationException:
/// The provided model identifier is invalid`, while the bare id
/// (`us.anthropic.claude-sonnet-4-6`) succeeds. This is now the single
/// authoritative strip point — `chat` calls it right before `.model_id(...)`
/// so the provider is correct regardless of what any caller passes.
/// What: Returns `slug` with a leading `"bedrock/"` removed via
/// `str::strip_prefix`, or `slug` unchanged when there is no such prefix
/// (defensive passthrough — a caller that already hands over a bare id must
/// not be mangled or panic).
/// Test: `bedrock::tests::bedrock_model_id_strips_prefix`,
/// `bedrock::tests::bedrock_model_id_passthrough_without_prefix`.
fn bedrock_model_id(slug: &str) -> &str {
    slug.strip_prefix("bedrock/").unwrap_or(slug)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
