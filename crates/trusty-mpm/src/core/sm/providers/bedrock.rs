//! AWS Bedrock Converse provider for the SM (DOC-14 §5.1; `bedrock` feature).
//!
//! Why: AWS-resident operators can use Bedrock-hosted models (IAM auth, private
//! VPC, no third-party SaaS egress) without an OpenRouter or Anthropic key.
//! This is the SM's third provider, ported from trusty-review's
//! `llm/bedrock/mod.rs` (text path only — the SM does not need the tool-use
//! plumbing). It lives behind the `bedrock` cargo feature so the heavy
//! `aws-sdk-bedrockruntime` dependency is opt-in (§5.1).
//! What: [`BedrockProvider`] calls `Converse` (non-streaming), maps
//! [`LlmRequest`] → Bedrock request, extracts text + usage, measures latency,
//! computes cost, and retries bounded times on transient errors. Config errors
//! (ModelNotFound/AccessDenied/Validation) never retry and always alarm. The
//! `us.`/`eu.`/… cross-region inference-profile prefix is required and validated
//! up front so a bare foundation-model id fails early as [`SmLlmError::Validation`].
//! Test: `bedrock_region_resolution`, `bedrock_prefix_validation`,
//! `bedrock_provider_stores_model_and_region`,
//! `bedrock_no_credentials_returns_error`,
//! `bedrock_empty_messages_is_validation_error` in tests; cost estimation is
//! covered centrally in `pricing_tests.rs`.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_bedrockruntime::Client as BedrockClient;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, InferenceConfiguration, Message, SystemContentBlock,
};
use tracing::{debug, warn};

use super::{LlmProvider, LlmRequest, LlmResponse, error::SmLlmError, pricing};

/// Region env var: trusty-specific override (highest precedence).
const ENV_REGION_TRUSTY: &str = "TRUSTY_AWS_REGION";
/// Region env var: standard AWS fallback.
const ENV_REGION_AWS: &str = "AWS_REGION";
/// Default AWS region when neither env var is set.
const DEFAULT_REGION: &str = "us-east-1";
/// Required cross-region inference-profile prefixes (§5.1).
const INFERENCE_PROFILE_PREFIXES: &[&str] = &["us.", "eu.", "ap.", "jp.", "global."];
/// Retry attempts for transient errors.
const MAX_RETRIES: u32 = 3;

// ─── Region & validation ───────────────────────────────────────────────────────

/// Resolve the AWS region: `explicit` > `TRUSTY_AWS_REGION` > `AWS_REGION` >
/// `us-east-1`.
///
/// Why: operators may set either env var; the trusty-specific one wins (§5.1).
/// What: returns the first non-empty value in precedence order.
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

/// Validate that `model_id` carries a cross-region inference-profile prefix.
///
/// Why: Bedrock rejects bare foundation-model ids at runtime; we surface that
/// at construction so operators see it immediately (§5.1).
/// What: `Ok(())` if any [`INFERENCE_PROFILE_PREFIXES`] matches; else
/// [`SmLlmError::Validation`].
/// Test: `bedrock_prefix_validation`.
fn validate_model_id(model_id: &str) -> Result<(), SmLlmError> {
    if INFERENCE_PROFILE_PREFIXES
        .iter()
        .any(|p| model_id.starts_with(p))
    {
        return Ok(());
    }
    Err(SmLlmError::Validation(format!(
        "Bedrock model id {model_id:?} must start with a cross-region inference-profile \
         prefix (us., eu., ap., jp., or global.). Example: \"us.anthropic.claude-sonnet-4-6\"."
    )))
}

// ─── Provider ──────────────────────────────────────────────────────────────────

/// AWS Bedrock Converse provider for the SM.
///
/// Why: satisfies [`LlmProvider`] over Bedrock so the SM works with IAM-based
/// auth and no SaaS API key (§5.1).
/// What: holds a `BedrockClient`, the bare model id, and the resolved region.
/// `complete` calls `Converse`, extracts text + usage, retries transient
/// errors up to [`MAX_RETRIES`].
/// Test: `bedrock_provider_stores_model_and_region`,
/// `bedrock_no_credentials_returns_error`.
pub struct BedrockProvider {
    client: BedrockClient,
    /// The bare (validated) model id, e.g. `us.anthropic.claude-sonnet-4-6`.
    pub model: String,
    region: String,
}

impl BedrockProvider {
    /// Construct using the standard AWS credential chain.
    ///
    /// Why: the SDK default chain covers env vars, `~/.aws/credentials`, IMDS,
    /// and SSO without code changes.
    /// What: validates the model id, resolves the region, loads AWS config, and
    /// builds the client. Returns [`SmLlmError::Validation`] on a bad model id.
    /// Async because credential loading may touch the filesystem / IMDS.
    /// Test: `bedrock_prefix_validation` (validation path; no network).
    pub async fn new(model: impl Into<String>, region: Option<&str>) -> Result<Self, SmLlmError> {
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

    /// Construct from a pre-built client (testing only; skips validation).
    ///
    /// Why: tests inject a `no_credentials()` client to drive provider logic
    /// without AWS.
    /// What: stores the client verbatim.
    /// Test: `bedrock_provider_stores_model_and_region`,
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

    /// Execute a single Converse call.
    ///
    /// Why: extracted so retry logic in `complete` is visible/testable.
    /// What: builds a `Converse` request from `req`, sends it, and maps SDK
    /// errors to [`SmLlmError`] by inspecting the message text.
    /// Test: error-mapping exercised via `bedrock_no_credentials_returns_error`.
    async fn call_once(&self, req: &LlmRequest) -> Result<LlmResponse, SmLlmError> {
        let start = Instant::now();

        let mut system_blocks: Vec<SystemContentBlock> = Vec::new();
        if !req.system.is_empty() {
            system_blocks.push(SystemContentBlock::Text(req.system.clone()));
        }

        let mut messages: Vec<Message> = Vec::new();
        for m in &req.messages {
            let role = if m.role == "assistant" {
                ConversationRole::Assistant
            } else {
                ConversationRole::User
            };
            let msg = Message::builder()
                .role(role)
                .content(ContentBlock::Text(m.content.clone()))
                .build()
                .map_err(|e| SmLlmError::Validation(format!("build Bedrock Message: {e}")))?;
            messages.push(msg);
        }
        if messages.is_empty() {
            return Err(SmLlmError::Validation(
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
            .set_messages(Some(messages));
        if !system_blocks.is_empty() {
            sdk_req = sdk_req.set_system(Some(system_blocks));
        }

        let resp = sdk_req
            .send()
            .await
            .map_err(|e| map_sdk_error(e.to_string(), &req.model, &self.region))?;

        let latency_ms = start.elapsed().as_millis() as u64;
        let text = extract_converse_text(&resp).unwrap_or_default();
        let (input_tokens, output_tokens) = extract_token_usage(&resp);
        let cost_usd = pricing::estimate_cost_usd(&req.model, input_tokens, output_tokens);

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

/// Map a Bedrock SDK error string to the right [`SmLlmError`] variant.
///
/// Why: the AWS SDK surfaces typed errors as strings here; classifying them
/// keeps retry/alarm policy correct (§5.3).
/// What: substring-matches the lowercased message to the matching variant,
/// defaulting unknown errors to retryable `Transport`.
/// Test: exercised by `bedrock_no_credentials_returns_error`.
fn map_sdk_error(msg: String, model: &str, region: &str) -> SmLlmError {
    let lower = msg.to_lowercase();
    if lower.contains("resourcenotfound") || lower.contains("no such model") {
        SmLlmError::ModelNotFound(format!("model={model}: {msg}"))
    } else if lower.contains("accessdenied")
        || lower.contains("unauthorized")
        || lower.contains("credential")
        || lower.contains("not authorized")
        || lower.contains("no credentials")
    {
        SmLlmError::AccessDenied(format!(
            "AWS Bedrock access denied (model={model}, region={region}): {msg}"
        ))
    } else if lower.contains("validationexception") || lower.contains("validation") {
        SmLlmError::Validation(msg)
    } else if lower.contains("throttling") || lower.contains("throttled") || lower.contains("rate")
    {
        SmLlmError::RateLimited
    } else if lower.contains("serviceunavailable") || lower.contains("internalserver") {
        SmLlmError::Upstream {
            status: 503,
            body: msg,
        }
    } else if lower.contains("modelnotready") || lower.contains("not in active") {
        SmLlmError::ModelNotReady(msg)
    } else {
        SmLlmError::Transport(format!(
            "Bedrock Converse SDK error (model={model}, region={region}): {msg}"
        ))
    }
}

#[async_trait]
impl LlmProvider for BedrockProvider {
    fn name(&self) -> &str {
        "bedrock"
    }

    /// Execute a Converse call with bounded retry for transient errors.
    ///
    /// Why: Bedrock returns transient 5xx/throttling; bounded exponential
    /// backoff recovers most without hiding config errors (§5.3).
    /// What: calls `call_once`; retries up to [`MAX_RETRIES`] while
    /// `is_retryable()`; returns other errors immediately. Logs cost/usage to
    /// stderr.
    /// Test: `bedrock_no_credentials_returns_error` (error path),
    /// `bedrock_empty_messages_is_validation_error` (validation path).
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, SmLlmError> {
        debug!(
            provider = "bedrock",
            model = %req.model,
            region = %self.region,
            "sm bedrock complete request"
        );
        let mut attempt = 0u32;
        loop {
            match self.call_once(&req).await {
                Ok(resp) => {
                    debug!(
                        provider = "bedrock",
                        model = %resp.model,
                        input_tokens = resp.input_tokens,
                        output_tokens = resp.output_tokens,
                        latency_ms = resp.latency_ms,
                        cost_usd = resp.cost_usd,
                        "sm bedrock complete response"
                    );
                    return Ok(resp);
                }
                Err(err) if err.is_retryable() && attempt < MAX_RETRIES => {
                    attempt += 1;
                    let backoff_ms = 500u64 * (1u64 << attempt.min(6));
                    warn!(attempt, backoff_ms, model = %req.model, "sm bedrock retry: {err}");
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                }
                Err(err) => return Err(err),
            }
        }
    }
}

// ─── Response helpers ───────────────────────────────────────────────────────────

/// Join the `Text` content blocks of a Converse response.
///
/// Why: Converse wraps content in typed blocks; the SM only needs the text.
/// What: joins `Text` blocks with newlines; `None` when empty.
/// Test: covered by `bedrock_no_credentials_returns_error`.
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

/// Extract `(input_tokens, output_tokens)` from the Converse usage.
///
/// Why: needed for cost estimation and telemetry (§5.5).
/// What: reads `usage.inputTokens`/`outputTokens`; `(0, 0)` when absent.
/// Test: covered by `bedrock_no_credentials_returns_error`.
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

#[cfg(test)]
#[path = "bedrock_tests.rs"]
mod tests;
