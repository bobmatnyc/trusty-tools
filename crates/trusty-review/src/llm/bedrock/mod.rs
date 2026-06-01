//! AWS Bedrock Converse API LLM provider for trusty-review.
//!
//! Why: organisations on AWS can use Bedrock-hosted models (IAM-based auth,
//! private VPC, no third-party SaaS egress) without an OpenRouter API key.
//! This module wires the AWS SDK `Converse` API into the [`LlmProvider`] trait
//! so the review pipeline is provider-agnostic.
//!
//! What: [`BedrockProvider`] calls `Converse` (non-streaming), maps
//! [`LlmRequest`] → Bedrock request, extracts the response text + token usage
//! from the Converse output, measures wall-clock latency, and computes a cost
//! estimate from a pricing table.  Transient errors are retried (3 attempts,
//! exponential backoff).  Config/lifecycle errors (ModelNotFound,
//! AccessDenied, etc.) are never retried and always alarm.
//!
//! When `LlmRequest.response_schema` is set, the provider builds a
//! `ToolConfiguration` with a single tool and forces `toolChoice = TOOL`
//! (the named tool).  The model's `toolUse.input` JSON is extracted and
//! returned as `LlmResponse.text` — clean, directly deserializable JSON
//! with no fence-stripping required.
//!
//! Region resolution: `TRUSTY_AWS_REGION` > `AWS_REGION` > `us-east-1`.
//! Credentials: standard AWS credential chain (env vars, `~/.aws/credentials`,
//! instance metadata/IMDS, SSO) — no API key needed.
//!
//! Model-id validation: the `us.` cross-region inference-profile prefix is
//! required.  Bedrock will reject a bare foundation-model id (e.g.
//! `anthropic.claude-sonnet-4-6`) with a ValidationException; we surface this
//! early as [`LlmError::Validation`] so operators see it immediately.
//!
//! Test: `bedrock_region_resolution`, `bedrock_us_prefix_validation`,
//! `bedrock_cost_estimate_*`, `bedrock_converse_request_construction`,
//! `bedrock_no_credentials_returns_error`,
//! `bedrock_request_includes_tool_config_when_schema_set` (all unit-level,
//! no real AWS calls).

pub mod pricing;
pub mod tool_use;

pub use pricing::{estimate_bedrock_cost_usd, normalize_model_family};
pub use tool_use::{build_tool_config, document_to_json_string, json_to_document};

use std::time::Instant;

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_bedrockruntime::Client as BedrockClient;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, InferenceConfiguration, Message, SystemContentBlock,
};
use tracing::{debug, warn};

use super::{LlmProvider, LlmRequest, LlmResponse, error::LlmError};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Region env var: trusty-specific override.
const ENV_REGION_TRUSTY: &str = "TRUSTY_AWS_REGION";
/// Region env var: standard AWS fallback.
const ENV_REGION_AWS: &str = "AWS_REGION";
/// Default AWS region when neither env var is set.
const DEFAULT_REGION: &str = "us-east-1";

/// Required prefix for Bedrock cross-region inference profiles.
///
/// Why: bare foundation-model ids (e.g. `anthropic.claude-sonnet-4-6`) fail
/// at runtime with a ValidationException.  Cross-region inference profiles
/// (`us.anthropic.*`, `eu.anthropic.*`, etc.) route to the best-available
/// region and are the recommended way to invoke Anthropic models on Bedrock.
pub(crate) const INFERENCE_PROFILE_PREFIXES: &[&str] = &["us.", "eu.", "ap.", "jp.", "global."];

/// Retry attempts for transient errors (Transport, RateLimited, Upstream 5xx).
const MAX_RETRIES: u32 = 3;

// ─── Region resolution ────────────────────────────────────────────────────────

/// Resolve the AWS region for the Bedrock client.
///
/// Why: operators may specify region via either `TRUSTY_AWS_REGION` (trusty-specific)
/// or `AWS_REGION` (standard); the trusty var takes precedence.
/// What: returns the first non-empty value of `explicit` > `TRUSTY_AWS_REGION`
///       > `AWS_REGION` > `"us-east-1"`.
/// Test: `bedrock_region_resolution`.
pub fn resolve_bedrock_region(explicit: Option<&str>) -> String {
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

// ─── Model id validation ──────────────────────────────────────────────────────

/// Validate that `model_id` has a cross-region inference-profile prefix.
///
/// Why: Bedrock will reject bare foundation-model ids at runtime with a
/// ValidationException; we surface the error at construction time so operators
/// see it immediately (same behaviour as `us.`-prefix validation in
/// trusty-analyze).
/// What: returns `Ok(())` if any `INFERENCE_PROFILE_PREFIXES` matches;
/// `Err(LlmError::Validation)` otherwise.
/// Test: `bedrock_us_prefix_validation`.
fn validate_model_id(model_id: &str) -> Result<(), LlmError> {
    let has_profile_prefix = INFERENCE_PROFILE_PREFIXES
        .iter()
        .any(|pfx| model_id.starts_with(pfx));
    if has_profile_prefix {
        return Ok(());
    }
    Err(LlmError::Validation(format!(
        "Bedrock model id {model_id:?} must start with a cross-region inference-profile \
         prefix (us., eu., ap., jp., or global.). \
         Example: \"us.anthropic.claude-sonnet-4-6\". \
         Bare foundation-model ids are not supported."
    )))
}

// ─── Provider ─────────────────────────────────────────────────────────────────

/// AWS Bedrock Converse API provider for trusty-review.
///
/// Why: satisfies the [`LlmProvider`] trait using Bedrock so the review
/// pipeline works without an OpenRouter API key; uses IAM-based auth suitable
/// for production AWS deployments.
/// What: holds a pre-built `BedrockClient` and the resolved model id and
/// region.  `complete` calls `Converse` (non-streaming), extracts text + token
/// usage from the response, measures latency, computes cost, and retries up to
/// [`MAX_RETRIES`] times for transient errors.  When `response_schema` is set,
/// the `Converse` call includes a `ToolConfiguration` that forces the model to
/// call the named tool; the `toolUse.input` JSON is returned as
/// `LlmResponse.text`.
/// Test: `bedrock_converse_request_construction`,
/// `bedrock_no_credentials_returns_error`,
/// `bedrock_request_includes_tool_config_when_schema_set`.
pub struct BedrockProvider {
    client: BedrockClient,
    /// Default model id; used in error messages and as fallback when the request
    /// does not override the model.
    pub model: String,
    region: String,
}

impl BedrockProvider {
    /// Construct a `BedrockProvider` using the standard AWS credential chain.
    ///
    /// Why: the AWS SDK's default chain handles env vars
    /// (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`),
    /// `~/.aws/credentials` profiles, IMDS v2, and SSO — covering both local
    /// dev and production deployments without code changes.
    /// What: validates the model id (requires an inference-profile prefix),
    /// resolves the region, loads AWS config, and builds a `BedrockClient`.
    /// Returns `LlmError::Validation` if the model id is invalid.
    /// Async because credential loading may touch the filesystem or IMDS.
    /// Test: `bedrock_us_prefix_validation` (validation path, no network);
    /// real-credentials path tested in ignored integration tests.
    pub async fn new(model: impl Into<String>, region: Option<&str>) -> Result<Self, LlmError> {
        let model = model.into();
        validate_model_id(&model)?;

        let region_str = resolve_bedrock_region(region);
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(aws_config::meta::region::RegionProviderChain::first_try(
                aws_types::region::Region::new(region_str.clone()),
            ))
            .load()
            .await;
        let client = BedrockClient::new(&config);
        Ok(Self {
            client,
            model,
            region: region_str,
        })
    }

    /// Construct from a pre-built `BedrockClient` (for testing).
    ///
    /// Why: tests can inject a client built with `no_credentials()` to verify
    /// provider logic without touching AWS.
    /// What: stores the client verbatim; skips model-id validation so tests
    /// can pass any id.
    /// Test: used by `bedrock_converse_request_construction` and
    /// `bedrock_no_credentials_returns_error`.
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

    /// The AWS region the client is configured for.
    pub fn region(&self) -> &str {
        &self.region
    }

    /// Execute a single Converse call and return the response.
    ///
    /// Why: extracted from `complete` so retry logic is visible and testable.
    /// What: builds a Bedrock `Converse` request from [`LlmRequest`], sends it,
    /// and maps SDK errors to [`LlmError`] variants.  When
    /// `req.response_schema` is set, injects a `ToolConfiguration` that forces
    /// the model to emit the schema-conformant JSON as a `toolUse` block;
    /// the `toolUse.input` is extracted and returned as `LlmResponse.text`.
    /// Test: called by `complete`; error-mapping tested in unit tests.
    async fn call_once(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let start = Instant::now();

        // Build system blocks and conversation messages from the LlmRequest.
        let mut system_blocks: Vec<SystemContentBlock> = Vec::new();
        if !req.system.is_empty() {
            system_blocks.push(SystemContentBlock::Text(req.system.clone()));
        }

        let mut converse_messages: Vec<Message> = Vec::new();
        for msg in &req.messages {
            let role = if msg.role == "assistant" {
                ConversationRole::Assistant
            } else {
                ConversationRole::User
            };
            let bedrock_msg = Message::builder()
                .role(role)
                .content(ContentBlock::Text(msg.content.clone()))
                .build()
                .map_err(|e| LlmError::Validation(format!("build Bedrock Message: {e}")))?;
            converse_messages.push(bedrock_msg);
        }

        if converse_messages.is_empty() {
            return Err(LlmError::Validation(
                "LlmRequest contains no user/assistant messages".to_string(),
            ));
        }

        let inference = InferenceConfiguration::builder()
            .max_tokens(req.max_tokens as i32)
            .temperature(req.temperature)
            .build();

        let mut sdk_req = self
            .client
            .converse()
            .model_id(&req.model)
            .inference_config(inference)
            .set_messages(Some(converse_messages));

        if !system_blocks.is_empty() {
            sdk_req = sdk_req.set_system(Some(system_blocks));
        }

        // When a response_schema is set, inject tool-use forcing.
        if let Some(ref schema) = req.response_schema {
            let tool_config = build_tool_config(&schema.name, &schema.schema)?;
            sdk_req = sdk_req.tool_config(tool_config);
        }

        let resp = sdk_req.send().await.map_err(|sdk_err| {
            let msg = sdk_err.to_string();
            let lower = msg.to_lowercase();
            // Map SDK errors to LlmError variants using the error message text.
            if lower.contains("resourcenotfound") || lower.contains("no such model") {
                LlmError::ModelNotFound(format!("model={}: {msg}", req.model))
            } else if lower.contains("accessdenied")
                || lower.contains("unauthorized")
                || lower.contains("credential")
                || lower.contains("not authorized")
            {
                LlmError::AccessDenied(format!(
                    "AWS Bedrock access denied (model={}, region={}): {msg}. \
                     Ensure AWS credentials are configured and the account has \
                     bedrock:InvokeModel permission.",
                    req.model, self.region
                ))
            } else if lower.contains("validationexception") || lower.contains("validation") {
                LlmError::Validation(msg)
            } else if lower.contains("throttlingexception")
                || lower.contains("throttled")
                || lower.contains("rate")
            {
                LlmError::RateLimited
            } else if lower.contains("serviceunavailable")
                || lower.contains("internalserver")
                || lower.contains("modelnotready")
                    && (lower.contains("creating") || lower.contains("failed"))
            {
                LlmError::Upstream {
                    status: 503,
                    body: msg,
                }
            } else if lower.contains("modelnotready") || lower.contains("not in active") {
                LlmError::ModelNotReady(msg)
            } else {
                LlmError::Transport(format!(
                    "Bedrock Converse SDK error (model={}, region={}): {msg}",
                    req.model, self.region
                ))
            }
        })?;

        let latency_ms = start.elapsed().as_millis() as u64;

        // Extract text: prefer toolUse.input (structured) over plain text.
        let text = if req.response_schema.is_some() {
            // When tool-use forcing is active, the model MUST emit a ToolUse
            // block.  Extract the input JSON directly.  If the block is absent
            // (unexpected), fall back to plain text extraction.
            tool_use::extract_tool_use_json(&resp)
                .or_else(|| extract_converse_text(&resp))
                .unwrap_or_default()
        } else {
            extract_converse_text(&resp).unwrap_or_default()
        };

        let (input_tokens, output_tokens) = extract_token_usage(&resp);
        let cost_usd = estimate_bedrock_cost_usd(&req.model, input_tokens, output_tokens);

        Ok(LlmResponse {
            text,
            model: req.model.clone(),
            input_tokens,
            output_tokens,
            latency_ms,
            cost_usd,
        })
    }
}

#[async_trait]
impl LlmProvider for BedrockProvider {
    fn name(&self) -> &str {
        "bedrock"
    }

    /// Execute a Bedrock Converse call with bounded retry for transient errors.
    ///
    /// Why: Bedrock can return transient 5xx or throttling errors; retrying up
    /// to 3 times with exponential backoff recovers most transient failures
    /// without hiding config/lifecycle problems.
    /// What: calls `call_once`; retries up to [`MAX_RETRIES`] times for errors
    /// where `is_retryable()` is true; immediately returns all other errors.
    /// Test: `bedrock_converse_request_construction` (unit, no real AWS calls).
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        debug!(
            model = %req.model,
            provider = "bedrock",
            region = %self.region,
            structured = req.response_schema.is_some(),
            "bedrock complete request"
        );

        let mut attempt = 0u32;
        loop {
            match self.call_once(&req).await {
                Ok(resp) => {
                    debug!(
                        model = %resp.model,
                        input_tokens = resp.input_tokens,
                        output_tokens = resp.output_tokens,
                        latency_ms = resp.latency_ms,
                        cost_usd = resp.cost_usd,
                        "bedrock complete response"
                    );
                    return Ok(resp);
                }
                Err(err) if err.is_retryable() && attempt < MAX_RETRIES => {
                    attempt += 1;
                    let backoff_ms = 500u64 * (1u64 << attempt.min(6));
                    warn!(
                        attempt,
                        backoff_ms,
                        model = %req.model,
                        "bedrock transient error — retrying: {err}"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                }
                Err(err) => return Err(err),
            }
        }
    }
}

// ─── Response helpers ─────────────────────────────────────────────────────────

/// Extract joined text from a Converse response output.
///
/// Why: the Converse API wraps all content in typed `ContentBlock` variants;
/// we only care about `Text` blocks for review output.
/// What: iterates the output message's content blocks and joins `Text` blocks
/// with newlines.
/// Test: covered indirectly by `bedrock_converse_request_construction`.
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

/// Extract input and output token counts from the Converse response usage.
///
/// Why: the Converse response includes `usage.inputTokens` and
/// `usage.outputTokens`; we need these for cost estimation and telemetry.
/// What: returns `(input_tokens, output_tokens)` as `(u32, u32)`.
/// Returns `(0, 0)` if usage is absent (some model variants omit it).
/// Test: covered by `bedrock_cost_estimate_sonnet`.
fn extract_token_usage(
    resp: &aws_sdk_bedrockruntime::operation::converse::ConverseOutput,
) -> (u32, u32) {
    resp.usage()
        .map(|u| {
            (
                u.input_tokens().max(0) as u32,
                u.output_tokens().max(0) as u32,
            )
        })
        .unwrap_or((0, 0))
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Tests extracted to bedrock/tests.rs to keep this file under the 500-line cap.

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
