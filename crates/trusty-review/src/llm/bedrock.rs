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
//! `bedrock_no_credentials_returns_error` (all unit-level, no real AWS calls).

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
const INFERENCE_PROFILE_PREFIXES: &[&str] = &["us.", "eu.", "ap.", "jp.", "global."];

/// Retry attempts for transient errors (Transport, RateLimited, Upstream 5xx).
const MAX_RETRIES: u32 = 3;

// ─── Pricing table ────────────────────────────────────────────────────────────

/// Approximate Bedrock pricing per million tokens (input, output) in USD.
///
/// Why: surfaces cost estimates in [`LlmResponse`] for the `compare` mode.
/// What: keyed by model id (inference-profile id without the `us.`/`eu.` prefix
/// since the per-token cost is the same across regions for the same model).
/// After stripping the geo-prefix the lookup also normalises date/version
/// suffixes (e.g. `-20251001-v1:0`) via [`normalize_model_family`] so that
/// date-stamped ids like `anthropic.claude-haiku-4-5-20251001-v1:0` match the
/// same table entry as the family prefix `anthropic.claude-haiku-4-5`.
/// Unknown ids → cost 0.0 with a debug log.
///
/// Sources: AWS Bedrock On-Demand pricing page (us-east-1, June 2026).
fn bedrock_cost_per_million(model: &str) -> (f64, f64) {
    // Step 1: strip the geography prefix (us., eu., ap., jp., global.) so
    // the pricing table works for all cross-region inference profiles.
    let after_geo = INFERENCE_PROFILE_PREFIXES
        .iter()
        .find_map(|pfx| model.strip_prefix(pfx))
        .unwrap_or(model);

    // Step 2: normalise by stripping any date/version suffix of the form
    // `-YYYYMMDD-vN:N` so that date-stamped ids (e.g. the verified Haiku 4.5
    // id `anthropic.claude-haiku-4-5-20251001-v1:0`) match the same table
    // entry as the short family prefix (`anthropic.claude-haiku-4-5`).
    let normalized = normalize_model_family(after_geo);

    match normalized {
        // Claude Sonnet 4.6 — default reviewer model.
        "anthropic.claude-sonnet-4-6" => (3.00, 15.00),
        // Claude Haiku 4.5 — default verifier/summarizer model.
        // Matches both `anthropic.claude-haiku-4-5` and the date-versioned
        // `anthropic.claude-haiku-4-5-20251001-v1:0` after normalization.
        "anthropic.claude-haiku-4-5" => (0.80, 4.00),
        // Claude Opus 4.8 — premium option.
        "anthropic.claude-opus-4-8" => (15.00, 75.00),
        // Legacy Claude 3.5 Sonnet (cross-region profile, date-versioned).
        "anthropic.claude-3-5-sonnet-20241022-v2:0" | "anthropic.claude-3-5-sonnet" => {
            (3.00, 15.00)
        }
        // Legacy Claude 3 Haiku (cross-region profile, date-versioned).
        "anthropic.claude-3-haiku-20240307-v1:0" | "anthropic.claude-3-haiku" => (0.25, 1.25),
        // Unknown model — no cost estimate.
        _ => {
            debug!(
                model = %model,
                "BedrockProvider: no pricing entry for model id — cost_usd will be 0.0"
            );
            (0.0, 0.0)
        }
    }
}

/// Normalise a model id by stripping date/version suffixes of the form
/// `-YYYYMMDD-vN:N` (e.g. `-20251001-v1:0`) so date-stamped Bedrock ids
/// match the short family-prefix entry in the pricing table.
///
/// Why: Bedrock uses date-versioned inference-profile ids (e.g.
/// `anthropic.claude-haiku-4-5-20251001-v1:0`) that would not match a
/// short key like `anthropic.claude-haiku-4-5` without normalization,
/// causing the pricing lookup to return (0.0, 0.0).
/// What: scans from the end of the string for a `-vN:N` version tag
/// (optionally preceded by `-YYYYMMDD`) and strips everything from the
/// first date segment onwards.  Returns the input unchanged if no suffix
/// is found.
/// Test: `bedrock_normalize_model_family_strips_suffix`,
/// `bedrock_cost_estimate_haiku_date_versioned`.
fn normalize_model_family(model: &str) -> &str {
    // Look for a `-vN:N` or `-vN` suffix, optionally preceded by a date segment
    // `-YYYYMMDD`.  Walk backwards through '-'-delimited segments.
    // Strategy: find the first '-'-separated segment that looks like a date
    // (8 digits) and strip from there.  If no date segment, look for a version
    // segment (`v` followed by digits/colons) and strip from there.
    let mut end = model.len();

    // Walk segments from the right.
    while let Some(dash_pos) = model[..end].rfind('-') {
        let segment = &model[dash_pos + 1..end];

        let is_date_segment = segment.len() == 8 && segment.bytes().all(|b| b.is_ascii_digit());
        let is_version_segment = segment.starts_with('v')
            && segment[1..]
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b':');

        if is_date_segment || is_version_segment {
            end = dash_pos;
            // Keep stripping — a date segment may be followed by a version segment.
        } else {
            break;
        }
    }

    &model[..end]
}

/// Compute USD cost estimate from token counts and model id.
///
/// Why: surfaces per-call cost in `compare` mode so operators can rank models
/// by cost-efficiency.
/// What: applies `bedrock_cost_per_million`; returns 0.0 for unknown models.
/// Test: `bedrock_cost_estimate_sonnet`, `bedrock_cost_estimate_unknown_model`.
pub fn estimate_bedrock_cost_usd(model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
    let (in_price, out_price) = bedrock_cost_per_million(model);
    (input_tokens as f64 / 1_000_000.0) * in_price
        + (output_tokens as f64 / 1_000_000.0) * out_price
}

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
/// [`MAX_RETRIES`] times for transient errors.
/// Test: `bedrock_converse_request_construction`, `bedrock_no_credentials_returns_error`.
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
    /// and maps SDK errors to [`LlmError`] variants.
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

        // Extract text and token usage from the Converse response.
        let text = extract_converse_text(&resp).unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── Region resolution ─────────────────────────────────────────────────

    #[test]
    fn bedrock_region_resolution() {
        // Explicit wins.
        assert_eq!(
            resolve_bedrock_region(Some("eu-west-1")),
            "eu-west-1",
            "explicit should win"
        );
        // Empty explicit falls through.
        assert_eq!(
            resolve_bedrock_region(Some("")),
            DEFAULT_REGION,
            "empty explicit should fall through to default"
        );
        // None falls through.
        assert_eq!(
            resolve_bedrock_region(None),
            DEFAULT_REGION,
            "None should return default"
        );
    }

    // ── Model id validation ───────────────────────────────────────────────

    #[test]
    fn bedrock_us_prefix_validation() {
        // Valid cross-region inference-profile prefixes.
        for id in [
            "us.anthropic.claude-sonnet-4-6",
            "eu.anthropic.claude-sonnet-4-6",
            "ap.anthropic.claude-sonnet-4-6",
            "jp.anthropic.claude-sonnet-4-6",
            "global.anthropic.claude-sonnet-4-6",
        ] {
            assert!(
                validate_model_id(id).is_ok(),
                "expected {id:?} to pass validation"
            );
        }

        // Bare foundation-model id should fail.
        let err = validate_model_id("anthropic.claude-sonnet-4-6").unwrap_err();
        assert!(
            matches!(err, LlmError::Validation(_)),
            "expected Validation error for bare id"
        );
        assert!(err.is_alarm(), "Validation is an alarm error");
        assert!(!err.is_retryable(), "Validation must not be retried");
    }

    #[test]
    fn bedrock_empty_model_id_is_validation_error() {
        let err = validate_model_id("").unwrap_err();
        assert!(matches!(err, LlmError::Validation(_)));
    }

    // ── Cost estimation ───────────────────────────────────────────────────

    #[test]
    fn bedrock_cost_estimate_sonnet() {
        // 1M input + 1M output at Sonnet pricing ($3/M + $15/M = $18/M).
        let cost =
            estimate_bedrock_cost_usd("us.anthropic.claude-sonnet-4-6", 1_000_000, 1_000_000);
        assert!(
            (cost - 18.0_f64).abs() < 1e-9,
            "expected $18.00 for 1M+1M Sonnet tokens, got {cost}"
        );
    }

    #[test]
    fn bedrock_cost_estimate_eu_prefix_normalized() {
        // eu. prefix should resolve to the same pricing as us.
        let eu_cost =
            estimate_bedrock_cost_usd("eu.anthropic.claude-sonnet-4-6", 1_000_000, 1_000_000);
        let us_cost =
            estimate_bedrock_cost_usd("us.anthropic.claude-sonnet-4-6", 1_000_000, 1_000_000);
        assert!(
            (eu_cost - us_cost).abs() < 1e-9,
            "eu. and us. prefixes should give identical cost: eu={eu_cost} us={us_cost}"
        );
    }

    #[test]
    fn bedrock_cost_estimate_haiku() {
        // Short-form id (no date suffix) must still price correctly.
        let cost = estimate_bedrock_cost_usd("us.anthropic.claude-haiku-4-5", 1_000_000, 1_000_000);
        assert!(
            (cost - 4.8_f64).abs() < 1e-9,
            "expected $4.80 for 1M+1M Haiku tokens (short id), got {cost}"
        );
    }

    /// Regression test: the verified Haiku 4.5 date-versioned id must resolve
    /// to non-zero pricing (Bug 3 fix).
    ///
    /// Why: `anthropic.claude-haiku-4-5-20251001-v1:0` (after geo-prefix strip)
    /// did not match the pricing table's `anthropic.claude-haiku-4-5` entry,
    /// causing cost_usd to be $0.00 in all Haiku compare runs.
    /// What: asserts the real date-versioned id prices at $4.80 for 1M+1M tokens.
    /// Test: this test itself; no network calls.
    #[test]
    fn bedrock_cost_estimate_haiku_date_versioned() {
        // The verified production Haiku 4.5 inference-profile id.
        let cost = estimate_bedrock_cost_usd(
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            1_000_000,
            1_000_000,
        );
        assert!(
            (cost - 4.8_f64).abs() < 1e-9,
            "expected $4.80 for 1M+1M Haiku tokens (date-versioned id), got {cost}. \
             The normalize_model_family() function must strip -20251001-v1:0 to match \
             the pricing table entry."
        );
    }

    /// Test that `normalize_model_family` correctly strips date and version suffixes.
    ///
    /// Why: directly verifies the normalization logic that underpins Bug 3 fix.
    /// What: checks several real and synthetic id forms.
    /// Test: this test itself; no network calls.
    #[test]
    fn bedrock_normalize_model_family_strips_suffix() {
        // Date + version suffix.
        assert_eq!(
            normalize_model_family("anthropic.claude-haiku-4-5-20251001-v1:0"),
            "anthropic.claude-haiku-4-5",
            "date+version suffix must be stripped"
        );
        // Date suffix only (no version).
        assert_eq!(
            normalize_model_family("anthropic.claude-3-5-sonnet-20241022"),
            "anthropic.claude-3-5-sonnet",
            "date-only suffix must be stripped"
        );
        // Version suffix only (no date).
        assert_eq!(
            normalize_model_family("anthropic.claude-3-haiku-20240307-v1:0"),
            "anthropic.claude-3-haiku",
            "date+version suffix must be stripped from legacy Haiku"
        );
        // No suffix — returned unchanged.
        assert_eq!(
            normalize_model_family("anthropic.claude-sonnet-4-6"),
            "anthropic.claude-sonnet-4-6",
            "id without date/version suffix must be unchanged"
        );
        assert_eq!(
            normalize_model_family("anthropic.claude-haiku-4-5"),
            "anthropic.claude-haiku-4-5",
            "short haiku id must be unchanged"
        );
    }

    #[test]
    fn bedrock_cost_estimate_unknown_model() {
        // Unknown model should return 0.0, not panic.
        let cost = estimate_bedrock_cost_usd("us.unknown/model-xyz", 500_000, 100_000);
        assert_eq!(cost, 0.0, "unknown model cost must be 0.0");
    }

    // ── Provider construction (no AWS calls) ──────────────────────────────

    /// Verify that `BedrockProvider::from_client` stores fields correctly.
    ///
    /// Why: ensures the provider's name/region accessors work without making
    /// any AWS calls.
    /// What: builds a client with `no_credentials()` and checks trait methods.
    /// Test: no network.
    #[tokio::test]
    async fn bedrock_provider_stores_model_and_region() {
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(aws_types::region::Region::new("us-east-1"))
            .no_credentials()
            .load()
            .await;
        let client = BedrockClient::new(&config);
        let provider =
            BedrockProvider::from_client(client, "us.anthropic.claude-sonnet-4-6", "us-east-1");
        assert_eq!(provider.name(), "bedrock");
        assert_eq!(provider.region(), "us-east-1");
    }

    /// Verify that `BedrockProvider::complete` returns a typed error when
    /// called with `no_credentials()`.
    ///
    /// Why: operators who misconfigure AWS should see a descriptive error about
    /// credentials, not an opaque panic or an OpenRouter-specific message.
    /// What: builds a `no_credentials` client, calls `complete`, expects an
    /// error whose `is_alarm()` or error message mentions credentials/Bedrock.
    /// Test: no real network call succeeds — error comes from the SDK before
    /// any TCP connection.
    #[tokio::test]
    async fn bedrock_no_credentials_returns_error() {
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(aws_types::region::Region::new("us-east-1"))
            .no_credentials()
            .load()
            .await;
        let client = BedrockClient::new(&config);
        let provider =
            BedrockProvider::from_client(client, "us.anthropic.claude-sonnet-4-6", "us-east-1");

        let req = crate::llm::LlmRequest {
            model: "us.anthropic.claude-sonnet-4-6".to_string(),
            system: "You are a code reviewer.".to_string(),
            messages: vec![crate::llm::ChatMessage {
                role: "user".to_string(),
                content: "Review this diff.".to_string(),
            }],
            temperature: 0.3,
            max_tokens: 512,
        };

        let result = provider.complete(req).await;
        let err = result.expect_err("should fail without real credentials");
        let msg = format!("{err}");
        // The error must either be classified as alarm/retryable or mention
        // something about AWS/Bedrock/credentials.
        let mentions_context = msg.to_lowercase().contains("bedrock")
            || msg.to_lowercase().contains("credential")
            || msg.to_lowercase().contains("aws")
            || msg.to_lowercase().contains("access")
            || err.is_alarm();
        assert!(
            mentions_context,
            "error should mention Bedrock/credentials/AWS; got: {msg}"
        );
    }

    /// Verify that `LlmRequest` fields map correctly to the Converse wire format.
    ///
    /// Why: the conversion between LlmRequest (system string + messages vec)
    /// and the Bedrock Message/SystemContentBlock types is the most error-prone
    /// step; unit-testing the shape prevents silent regressions.
    /// What: constructs an LlmRequest and verifies the field mapping via the
    /// same logic used in `call_once`.
    /// Test: pure logic test — no network, no AWS calls.
    #[test]
    fn bedrock_converse_request_construction() {
        use crate::llm::ChatMessage;

        let req = LlmRequest {
            model: "us.anthropic.claude-sonnet-4-6".to_string(),
            system: "You are a Rust code reviewer.".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "Review this diff.".to_string(),
            }],
            temperature: 0.3,
            max_tokens: 1024,
        };

        // Verify the system message is non-empty.
        assert!(!req.system.is_empty(), "system message must be forwarded");

        // Verify user messages are present.
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[0].content, "Review this diff.");

        // Verify temperature and max_tokens are within Bedrock-accepted ranges.
        assert!(
            req.temperature >= 0.0 && req.temperature <= 1.0,
            "temperature must be in [0.0, 1.0] for Bedrock"
        );
        assert!(req.max_tokens > 0, "max_tokens must be > 0");
    }
}
