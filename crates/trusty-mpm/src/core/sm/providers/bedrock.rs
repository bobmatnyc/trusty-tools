//! AWS Bedrock Converse provider for the SM (DOC-14 §5.1; `bedrock` feature).
//!
//! Why: AWS-resident operators can use Bedrock-hosted models (IAM auth, private
//! VPC, no third-party SaaS egress) without an OpenRouter or Anthropic key.
//! This is the SM's third provider. As of #2411 the Bedrock *wire* integration
//! (region resolution, the AWS credential chain, and the Converse
//! message/usage/response conversion) is NO LONGER a private aws-sdk port here:
//! it lives once in the shared `trusty_common::inference::bedrock` Converse
//! adapter (#2407). This module is the thin bridge that remains — it wraps that
//! shared [`BedrockAdapter`] and re-applies the SM-specific POLICY the commons
//! adapter deliberately does not carry: the required cross-region
//! inference-profile prefix validation (§5.1), the typed [`SmLlmError`]
//! classification the resolver's retry/alarm loop depends on (§5.3), bounded
//! retry with backoff, and per-call cost/latency telemetry (§5.5).
//! What: [`BedrockProvider`] holds a lazily-constructed [`BedrockAdapter`] and
//! maps [`LlmRequest`] → shared [`ChatRequest`], delegates the Converse call,
//! maps a shared [`InferenceError`] back to [`SmLlmError`] via [`map_sdk_error`]
//! (unchanged substring classifier — the commons `DisplayErrorContext` string
//! carries the same AWS error-class markers the old raw SDK string did), extracts
//! text + usage, measures latency, computes cost, and retries bounded times on
//! transient errors. Config errors (ModelNotFound/AccessDenied/Validation) never
//! retry and always alarm. The `us.`/`eu.`/… inference-profile prefix is required
//! and validated up front so a bare foundation-model id fails early as
//! [`SmLlmError::Validation`].
//! Test: `bedrock_region_resolution`, `bedrock_prefix_validation`,
//! `bedrock_provider_stores_model_and_region`, `bedrock_empty_messages_is_validation_error`,
//! and `map_sdk_error_*` in `bedrock_tests.rs`; the live Converse wire path is
//! covered by the shared adapter's `#[ignore]`-gated coverage in
//! `trusty_common::inference::bedrock`. Cost estimation is covered centrally in
//! `pricing_tests.rs`.
//!
//! [`InferenceError`]: trusty_common::inference::InferenceError

use std::time::{Duration, Instant};

use async_trait::async_trait;
use tracing::{debug, warn};
use trusty_common::inference::{
    BedrockAdapter, ChatMessage as InferenceChatMessage, ChatRequest, InferenceAdapter,
};

use super::{LlmProvider, LlmRequest, LlmResponse, error::SmLlmError, pricing};

/// Required cross-region inference-profile prefixes (§5.1).
const INFERENCE_PROFILE_PREFIXES: &[&str] = &["us.", "eu.", "ap.", "jp.", "global."];
/// Retry attempts for transient errors.
const MAX_RETRIES: u32 = 3;

// ─── Validation ────────────────────────────────────────────────────────────────

/// Validate that `model_id` carries a cross-region inference-profile prefix.
///
/// Why: Bedrock rejects bare foundation-model ids at runtime; we surface that
/// at construction so operators see it immediately (§5.1). The shared commons
/// adapter deliberately does NOT enforce this (it serves every consumer), so the
/// SM keeps its own up-front check here.
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
/// auth and no SaaS API key (§5.1), while the Converse wire mechanics live in
/// the shared `trusty_common::inference::bedrock` adapter (#2411 dedup).
/// What: holds the shared [`BedrockAdapter`] (which owns the resolved region and
/// a lazily-built AWS client — construction touches no AWS credentials, #2245)
/// and the bare, validated model id. `complete` maps to the shared request,
/// delegates the Converse call, extracts text + usage, and retries transient
/// errors up to [`MAX_RETRIES`].
/// Test: `bedrock_provider_stores_model_and_region`.
#[derive(Debug)]
pub struct BedrockProvider {
    /// Shared Converse adapter (owns region + lazy AWS client).
    inner: BedrockAdapter,
    /// The bare (validated) model id, e.g. `us.anthropic.claude-sonnet-4-6`.
    pub model: String,
}

impl BedrockProvider {
    /// Construct using the standard AWS credential chain.
    ///
    /// Why: the SDK default chain (resolved lazily by the shared adapter on the
    /// first call) covers env vars, `~/.aws/credentials`, IMDS, and SSO without
    /// code changes. Region resolution (`region` > `TRUSTY_AWS_REGION` >
    /// `AWS_REGION` > `us-east-1`) is the shared adapter's, unchanged from the
    /// SM's prior local copy.
    /// What: validates the model id, then builds a [`BedrockAdapter`] pinned to
    /// the resolved region. Returns [`SmLlmError::Validation`] on a bad model id.
    /// Stays `async` for call-site compatibility (the resolver `.await`s it);
    /// the AWS client build is deferred to the first `complete`.
    /// Test: `bedrock_prefix_validation` (validation path; no network),
    /// `bedrock_provider_stores_model_and_region`.
    pub async fn new(model: impl Into<String>, region: Option<&str>) -> Result<Self, SmLlmError> {
        let model = model.into();
        validate_model_id(&model)?;
        Ok(Self {
            inner: BedrockAdapter::new(region),
            model,
        })
    }

    /// The AWS region the client is configured for.
    ///
    /// Why: exposed for diagnostics / telemetry.
    /// What: delegates to the shared adapter's resolved region.
    /// Test: `bedrock_provider_stores_model_and_region`, `bedrock_region_resolution`.
    pub fn region(&self) -> &str {
        self.inner.region()
    }

    /// Execute a single Converse call via the shared adapter.
    ///
    /// Why: extracted so retry logic in `complete` is visible/testable.
    /// What: rejects an empty message list up front (deterministic client-side
    /// [`SmLlmError::Validation`]), converts `req` into the shared [`ChatRequest`]
    /// (system prompt prepended as a `system`-role message the commons converter
    /// diverts into Converse's system array; `assistant` role → assistant, all
    /// other roles → user, matching the SM's prior mapping), delegates to the
    /// shared adapter, and maps a shared [`InferenceError`] back to a typed
    /// [`SmLlmError`] via [`map_sdk_error`].
    /// Test: `bedrock_empty_messages_is_validation_error`; error-mapping via
    /// `map_sdk_error_*`.
    async fn call_once(&self, req: &LlmRequest) -> Result<LlmResponse, SmLlmError> {
        let start = Instant::now();

        if req.messages.is_empty() {
            return Err(SmLlmError::Validation(
                "LlmRequest contains no user/assistant messages".to_string(),
            ));
        }

        let mut messages: Vec<InferenceChatMessage> = Vec::with_capacity(req.messages.len() + 1);
        if !req.system.is_empty() {
            messages.push(InferenceChatMessage::system(req.system.clone()));
        }
        for m in &req.messages {
            let msg = if m.role == "assistant" {
                InferenceChatMessage::assistant(m.content.clone())
            } else {
                InferenceChatMessage::user(m.content.clone())
            };
            messages.push(msg);
        }

        let mut chat_req = ChatRequest::new(req.model.clone(), messages);
        chat_req.temperature = Some(req.temperature);
        chat_req.max_tokens = Some(req.max_tokens);

        let resp = self
            .inner
            .chat(&chat_req)
            .await
            .map_err(|e| map_sdk_error(e.to_string(), &req.model, self.region()))?;

        let latency_ms = start.elapsed().as_millis() as u64;
        let text = resp.first_text().unwrap_or_default();
        let usage = resp.usage();
        let (input_tokens, output_tokens) = (usage.prompt_tokens, usage.completion_tokens);
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

/// Map a Bedrock error string to the right [`SmLlmError`] variant.
///
/// Why: the resolver's retry/alarm loop (§5.3) must distinguish config errors
/// (never retry, always alarm) from transient ones (retry with backoff). The
/// shared adapter collapses every Converse failure into `InferenceError::Provider`,
/// but its message wraps the full AWS source chain via `DisplayErrorContext`, so
/// the same AWS error-class markers the SM matched on the raw SDK string are
/// still present — this substring classifier is unchanged from the pre-#2411
/// local port.
/// What: substring-matches the lowercased message to the matching variant,
/// defaulting unknown errors to retryable `Transport`.
/// Test: `map_sdk_error_classifies_*`.
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
    /// Test: `bedrock_empty_messages_is_validation_error` (validation path);
    /// retry/error classification via `map_sdk_error_*`.
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, SmLlmError> {
        debug!(
            provider = "bedrock",
            model = %req.model,
            region = %self.region(),
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

#[cfg(test)]
#[path = "bedrock_tests.rs"]
mod tests;
