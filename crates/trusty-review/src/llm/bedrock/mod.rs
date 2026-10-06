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
//! (the named tool), or sets `toolChoice = auto` plus one system-prompt line
//! for a model that rejects forcing (#9292, see
//! `tool_use::supports_forced_tool_choice`).  The model's `toolUse.input`
//! JSON is extracted and returned as `LlmResponse.text` — clean, directly
//! deserializable JSON with no fence-stripping required.
//!
//! `temperature` is omitted for a model that rejects it (#9304, see
//! `accepts_temperature`).
//!
//! Region resolution: an explicit region > the region inside a Bedrock model
//! ARN > `TRUSTY_AWS_REGION` > `AWS_REGION` > `us-east-1`.
//! Credentials: standard AWS credential chain (env vars, `~/.aws/credentials`,
//! instance metadata/IMDS, SSO) — no API key needed.
//!
//! Model-id validation: the `us.` cross-region inference-profile prefix is
//! required.  Bedrock will reject a bare foundation-model id (e.g.
//! `anthropic.claude-sonnet-4-6`) with a ValidationException; we surface this
//! early as [`LlmError::Validation`] so operators see it immediately. A
//! well-formed Bedrock model ARN is accepted too (#9200, see [`arn`]).
//!
//! Test: `bedrock_region_resolution`, `bedrock_us_prefix_validation`,
//! `bedrock_cost_estimate_*`, `bedrock_converse_request_construction`,
//! `bedrock_no_credentials_returns_error`,
//! `bedrock_request_includes_tool_config_when_schema_set` (all unit-level,
//! no real AWS calls).

pub(crate) mod arn;
// #9310: opt-in raw capture of reviewer replies.
mod capture;
pub mod pricing;
mod request_metadata;
pub mod tool_use;

pub use pricing::{estimate_bedrock_cost_usd, normalize_model_family};
pub use tool_use::{build_tool_config, document_to_json_string, json_to_document};

use std::time::Instant;

use async_trait::async_trait;
use aws_sdk_bedrockruntime::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, InferenceConfiguration, Message, SystemContentBlock,
};
use tracing::{debug, warn};
use trusty_common::inference::BedrockAdapter;

use super::{LlmProvider, LlmRequest, LlmResponse, error::LlmError};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Required prefix for Bedrock cross-region inference profiles.
///
/// Why: bare foundation-model ids (e.g. `anthropic.claude-sonnet-4-6`) fail
/// at runtime with a ValidationException.  Cross-region inference profiles
/// (`us.anthropic.*`, `eu.anthropic.*`, etc.) route to the best-available
/// region and are the recommended way to invoke Anthropic models on Bedrock.
///
/// #6114 moved the list itself to `trusty-common`, where the shared model-shape
/// inference reads the same prefixes to decide that an id is Bedrock's. Two
/// copies would let this validator and that inference disagree about the same
/// string; this alias keeps the crate-local spelling with one definition behind
/// it.
pub(crate) const INFERENCE_PROFILE_PREFIXES: &[&str] =
    trusty_common::inference::BEDROCK_INFERENCE_PROFILE_PREFIXES;

/// Retry attempts for transient errors (Transport, RateLimited, Upstream 5xx).
const MAX_RETRIES: u32 = 3;

// ─── Region resolution ────────────────────────────────────────────────────────

// #5469: the region precedence walk (explicit > TRUSTY_AWS_REGION > AWS_REGION >
// us-east-1) lives in the shared Bedrock adapter now; this crate re-exports the
// resolver so the public path callers already use keeps resolving.
pub use trusty_common::inference::bedrock::resolve_bedrock_region;

// ─── Model id validation ──────────────────────────────────────────────────────

/// Validate that `model_id` has a cross-region inference-profile prefix, or is
/// a Bedrock model ARN.
///
/// Why: Bedrock will reject bare foundation-model ids at runtime with a
/// ValidationException; we surface the error at construction time so operators
/// see it immediately (same behaviour as `us.`-prefix validation in
/// trusty-analyze).
/// What: returns `Ok(())` if any `INFERENCE_PROFILE_PREFIXES` matches or
/// [`arn::parse`] accepts the id; `Err(LlmError::Validation)` otherwise.
/// Test: `bedrock_us_prefix_validation`,
/// `application_inference_profile_arn_routes_to_bedrock`,
/// `inference_profile_arn_routes_to_bedrock`, `foundation_model_arn_routes_to_bedrock`.
fn validate_model_id(model_id: &str) -> Result<(), LlmError> {
    let has_profile_prefix = INFERENCE_PROFILE_PREFIXES
        .iter()
        .any(|pfx| model_id.starts_with(pfx));
    // #9200: a Bedrock model ARN names its resource and region in full.
    if has_profile_prefix || arn::parse(model_id).is_some() {
        return Ok(());
    }
    // #9200: the id may be a near-ARN; never echo its account id.
    let shown = arn::mask_account_ids(model_id);
    Err(LlmError::Validation(format!(
        "Bedrock model id {shown:?} must start with a cross-region inference-profile \
         prefix (us., eu., ap., jp., or global.), or be a Bedrock model ARN. \
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
/// What: holds the shared [`BedrockAdapter`] — which owns region resolution and
/// the lazily-built Converse client — plus the default model id.  `complete`
/// calls `Converse` (non-streaming) on the adapter's client, extracts text +
/// token usage from the response, measures latency, computes cost, and retries
/// up to [`MAX_RETRIES`] times for transient errors.  When `response_schema` is
/// set, the `Converse` call includes a `ToolConfiguration` that forces the model
/// to call the named tool; the `toolUse.input` JSON is returned as
/// `LlmResponse.text`.
/// Test: `bedrock_converse_request_construction`,
/// `bedrock_no_credentials_returns_error`,
/// `bedrock_request_includes_tool_config_when_schema_set`.
pub struct BedrockProvider {
    /// #5469: region resolution and Converse-client construction both come from
    /// the shared adapter; this crate keeps only the review-specific mapping.
    adapter: BedrockAdapter,
    /// Default model id; used in error messages and as fallback when the request
    /// does not override the model.
    pub model: String,
}

impl BedrockProvider {
    /// Construct a `BedrockProvider` using the standard AWS credential chain.
    ///
    /// Why: the AWS SDK's default chain handles env vars
    /// (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`),
    /// `~/.aws/credentials` profiles, IMDS v2, and SSO — covering both local
    /// dev and production deployments without code changes.
    /// What: [`Self::new_in_region`] with no explicit region, so the region is
    /// the one inside a Bedrock model ARN, else the ambient walk
    /// (`TRUSTY_AWS_REGION` > `AWS_REGION` > `us-east-1`).  Returns
    /// `LlmError::Validation` if the model id is invalid.  #5469: synchronous,
    /// because the adapter builds its AWS client lazily on the first `Converse`
    /// call.
    /// Test: `bedrock_us_prefix_validation` (validation path, no network);
    /// real-credentials path tested in ignored integration tests.
    pub fn new(model: impl Into<String>) -> Result<Self, LlmError> {
        Self::new_in_region(model, None)
    }

    /// Construct a `BedrockProvider`, optionally pinning the AWS region.
    ///
    /// Why: a Bedrock model ARN carries the region its resource lives in, and
    /// Converse resolves an ARN only in that region, so an ambient
    /// `AWS_REGION` set for other work must not send the call elsewhere
    /// (#9200). A caller that passes a region on purpose still wins: that is a
    /// per-call decision, where the env vars are machine-wide defaults.
    /// What: validates the model id, then resolves the region as `region` (when
    /// non-empty) > the ARN's region field > `TRUSTY_AWS_REGION` >
    /// `AWS_REGION` > `us-east-1`. A plain model id has no ARN tier, so with
    /// `region = None` it resolves exactly as [`Self::new`] did before ARNs.
    /// Test: `arn_region_wins_over_ambient_and_explicit_region_wins_over_arn`.
    pub fn new_in_region(model: impl Into<String>, region: Option<&str>) -> Result<Self, LlmError> {
        let model = model.into();
        validate_model_id(&model)?;
        // #9200: explicit > ARN region > env walk inside `BedrockAdapter::new`.
        let explicit = region.filter(|r| !r.trim().is_empty());
        let region = explicit.or_else(|| arn::parse(&model).map(|a| a.region));
        Ok(Self {
            adapter: BedrockAdapter::new(region),
            model,
        })
    }

    /// Construct from a pre-built Converse client (for testing).
    ///
    /// Why: tests can inject a client built with `no_credentials()` to verify
    /// provider logic without touching AWS.
    /// What: wraps the client in a [`BedrockAdapter`] via
    /// `BedrockAdapter::with_client`, which pins `region` verbatim; skips
    /// model-id validation so tests can pass any id.
    /// Test: used by `bedrock_provider_stores_model_and_region` and
    /// `bedrock_no_credentials_returns_error`.
    #[cfg(test)]
    pub fn from_client(
        client: aws_sdk_bedrockruntime::Client,
        model: impl Into<String>,
        region: impl Into<String>,
    ) -> Self {
        Self {
            adapter: BedrockAdapter::with_client(region, client),
            model: model.into(),
        }
    }

    /// The AWS region the client is configured for.
    pub fn region(&self) -> &str {
        self.adapter.region()
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
        // #5469: the adapter builds its AWS client on first use; take it before
        // the latency clock starts so the one-off config load is not billed as
        // model latency.
        let client = self.adapter.client().await.map_err(|e| {
            LlmError::Transport(format!(
                "Bedrock client construction failed (region={}): {e}",
                self.adapter.region()
            ))
        })?;

        let start = Instant::now();

        // #6123: one resolver decides the model for all three providers; this
        // one already read the request, and now says so in the same words.
        let model = req.effective_model(&self.model);

        // Build system blocks and conversation messages from the LlmRequest.
        let system_blocks = system_blocks(req, model);

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

        let inference = inference_config(req, model);

        let mut sdk_req = client
            .converse()
            .model_id(model)
            .inference_config(inference)
            .set_messages(Some(converse_messages));

        if !system_blocks.is_empty() {
            sdk_req = sdk_req.set_system(Some(system_blocks));
        }

        // Tag the request for invocation-log cost attribution: caller,
        // crate_version, and role when the schema names it. This is the only
        // Converse send site. Test: `converse_request_carries_caller_and_role_metadata`.
        for (key, value) in request_metadata::request_metadata(req) {
            sdk_req = sdk_req.request_metadata(key, value);
        }

        // When a response_schema is set, inject tool-use forcing — or `auto`
        // for a model that rejects forcing (#9292).
        if let Some(ref schema) = req.response_schema {
            let tool_config =
                tool_use::build_tool_config_for_model(model, &schema.name, &schema.schema)?;
            sdk_req = sdk_req.tool_config(tool_config);
        }

        let resp = sdk_req
            .send()
            .await
            .map_err(|sdk_err| map_converse_error(&sdk_err, model, self.adapter.region()))?;

        let latency_ms = start.elapsed().as_millis() as u64;

        // #9310: read per call, so capture needs no restart; off when unset.
        let capture = capture::capture_dir(&|key| std::env::var(key).ok());
        Ok(llm_response(
            req,
            model,
            &resp,
            latency_ms,
            capture.as_deref(),
        ))
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
        // #9200: log the model with any ARN account id masked.
        let shown = arn::mask_account_ids(req.effective_model(&self.model));
        debug!(
            model = %shown,
            provider = "bedrock",
            region = %self.adapter.region(),
            structured = req.response_schema.is_some(),
            "bedrock complete request"
        );

        let mut attempt = 0u32;
        loop {
            match self.call_once(&req).await {
                Ok(resp) => {
                    debug!(
                        model = %shown,
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
                        model = %shown,
                        "bedrock transient error — retrying: {err}"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                }
                Err(err) => return Err(err),
            }
        }
    }
}

// ─── Error rendering ──────────────────────────────────────────────────────────

/// Map a failed Converse send to an [`LlmError`], with ARN account ids masked.
///
/// Why: extracted from `call_once` so the mapping is testable without AWS, and
/// so every message it builds passes one mask: an ARN model id, and any ARN
/// AWS quotes back, would otherwise put an account id in the error (#9200).
/// What: renders the SDK error via [`describe_sdk_error`], masks it and the
/// model id with [`arn::mask_account_ids`], and classifies the message text
/// into a variant exactly as `call_once` did before the extraction.
/// Test: `converse_errors_mask_the_arn_account_id`.
fn map_converse_error<E, R>(sdk_err: &SdkError<E, R>, model: &str, region: &str) -> LlmError
where
    E: ProvideErrorMetadata + std::fmt::Display,
{
    // #9200: the unmasked id still goes to AWS; only this text is masked.
    let model = arn::mask_account_ids(model);
    // #6912: SdkError's own Display flattens a service error to the bare
    // word "service error"; read the AWS code and message instead.
    let msg = arn::mask_account_ids(&describe_sdk_error(sdk_err)).into_owned();
    let lower = msg.to_lowercase();
    // Map SDK errors to LlmError variants using the error message text.
    if lower.contains("resourcenotfound") || lower.contains("no such model") {
        LlmError::ModelNotFound(format!("model={model}: {msg}"))
    } else if lower.contains("accessdenied")
        || lower.contains("unauthorized")
        || lower.contains("credential")
        || lower.contains("not authorized")
    {
        LlmError::AccessDenied(format!(
            "AWS Bedrock access denied (model={model}, region={region}): {msg}. \
             Ensure AWS credentials are configured and the account has \
             bedrock:InvokeModel permission."
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
            "Bedrock Converse SDK error (model={model}, region={region}): {msg}"
        ))
    }
}

/// Render an `SdkError` with the AWS error code and message attached.
///
/// Why: `SdkError`'s own `Display` writes one fixed word per variant and never
/// consults the modeled error, so every Bedrock service failure reached the
/// operator as the literal "service error" — a wrong region, a missing
/// credential, and an unapproved model all read identically. See #6912.
/// What: for the `ServiceError` variant, reads the AWS error metadata and
/// returns `"<code>: <message>"`. Metadata with no message falls back to the
/// modeled error's own `Display`; metadata with no code falls back to it
/// entirely, since a generated Bedrock exception already spells its code there.
/// Every other variant (construction, timeout, dispatch, response) keeps
/// `SdkError`'s existing rendering — those carry no metadata to read.
/// Test: `bedrock_service_error_surfaces_code_and_message`,
/// `bedrock_service_error_without_metadata_falls_back_to_display`,
/// `bedrock_timeout_error_keeps_sdk_rendering`.
fn describe_sdk_error<E, R>(sdk_err: &SdkError<E, R>) -> String
where
    E: ProvideErrorMetadata + std::fmt::Display,
{
    match sdk_err {
        SdkError::ServiceError(ctx) => {
            let service_err = ctx.err();
            let meta = service_err.meta();
            match (meta.code(), meta.message()) {
                (Some(code), Some(message)) => format!("{code}: {message}"),
                (Some(code), None) => format!("{code}: {service_err}"),
                (None, _) => service_err.to_string(),
            }
        }
        other => other.to_string(),
    }
}

// ─── Request helpers ──────────────────────────────────────────────────────────

/// The Converse system blocks for `req` sent to `model`.
///
/// Why: a model that rejects a forced `toolChoice` gets `auto` instead, and
/// `auto` alone no longer requires a tool call, so the request asks for it in
/// the system prompt (#9292).
/// What: `req.system` as one block when non-empty; then, only for a
/// structured request to a model that rejects forcing, one more block holding
/// [`tool_use::auto_tool_instruction`]. Any other request gets exactly the
/// blocks it had before #9292.
/// Test: `system_blocks_add_the_tool_line_only_for_auto_models`.
fn system_blocks(req: &LlmRequest, model: &str) -> Vec<SystemContentBlock> {
    let mut blocks = Vec::new();
    if !req.system.is_empty() {
        blocks.push(SystemContentBlock::Text(req.system.clone()));
    }
    if let Some(schema) = &req.response_schema
        && !tool_use::supports_forced_tool_choice(model)
    {
        blocks.push(SystemContentBlock::Text(tool_use::auto_tool_instruction(
            &schema.name,
        )));
    }
    blocks
}

/// Model families whose Bedrock Converse API rejects `temperature` with
/// `ValidationException` (#9304).
const NO_TEMPERATURE_FAMILIES: &[&str] = &["claude-sonnet-5-5", "claude-opus-5-5"];

/// Whether `model` accepts a `temperature` on Bedrock Converse.
///
/// Why: Opus 5.5 rejects `temperature` ("`temperature` is deprecated for this
/// model"), and Sonnet 5.5 rejects any non-default value, so every review
/// call to either failed (#9304).
/// What: `false` only for a family in [`NO_TEMPERATURE_FAMILIES`], read with
/// the #9292 parser [`tool_use::bedrock_model_family`]. An id that parser
/// cannot read — an application-inference-profile ARN — keeps `temperature`:
/// for a model that accepts it, dropping it silently swaps the configured
/// value for the model default, while a wrong `true` fails loudly with the
/// same `ValidationException` this fix removes.
/// Test: `inference_config_omits_temperature_only_for_claude_5_5`.
fn accepts_temperature(model: &str) -> bool {
    tool_use::bedrock_model_family(model).is_none_or(|f| !NO_TEMPERATURE_FAMILIES.contains(&f))
}

/// The Converse inference configuration for `req` sent to `model`.
///
/// What: `max_tokens` always; `temperature` only when
/// [`accepts_temperature`] holds.
/// Test: `inference_config_omits_temperature_only_for_claude_5_5`,
/// `inference_config_temperature_covers_every_compare_candidate`.
fn inference_config(req: &LlmRequest, model: &str) -> InferenceConfiguration {
    // #9304: the 5.5 families reject `temperature`; omit the field for them.
    let temperature = accepts_temperature(model).then_some(req.temperature);
    InferenceConfiguration::builder()
        .max_tokens(req.max_tokens as i32)
        .set_temperature(temperature)
        .build()
}

// ─── Response helpers ─────────────────────────────────────────────────────────

/// The `LlmResponse` for one Converse reply, capturing it when `capture` is set.
///
/// Why: one place builds the response, so the opt-in capture (#9310) can be
/// shown not to change it.
/// What: the parser text from [`response_text`], token usage, cost, and the
/// lowercase stop reason (#1357); `warn!`s the block kinds of a structured
/// reply with no `toolUse` block. With `capture` set, a reviewer reply is also
/// written by `capture::capture_reply`, which never fails the call.
/// Test: `capture_write_failure_leaves_the_response_identical`,
/// `capture_on_writes_one_private_file_per_reviewer_call`.
fn llm_response(
    req: &LlmRequest,
    model: &str,
    resp: &aws_sdk_bedrockruntime::operation::converse::ConverseOutput,
    latency_ms: u64,
    capture: Option<&std::path::Path>,
) -> LlmResponse {
    let text = response_text(resp, req.response_schema.is_some());

    let (input_tokens, output_tokens) = extract_token_usage(resp);
    let cost_usd = estimate_bedrock_cost_usd(model, input_tokens, output_tokens);

    // Bedrock Converse surfaces a stop reason (`end_turn`, `max_tokens`, …);
    // thread it through as the PRIMARY truncation signal (#1357).  `max_tokens`
    // is the truncation case the runner keys off.
    let finish_reason = Some(resp.stop_reason().as_str().trim().to_ascii_lowercase());

    // #9310: a structured reply with no toolUse block reaches the parser as
    // text; log its block kinds, which the runner's record cannot see.
    if req.response_schema.is_some() {
        let blocks = tool_use::reply_block_kinds(resp);
        if !blocks.contains(&"tool_use") {
            warn!(
                model = %arn::mask_account_ids(model),
                blocks = ?blocks,
                stop = finish_reason.as_deref().unwrap_or("none"),
                output_tokens,
                text_chars = text.chars().count(),
                "structured Bedrock reply carried no toolUse block; the caller parses its text (#9310)"
            );
        }
    }

    if let Some(dir) = capture {
        capture::capture_reply(dir, req, model, resp, &text);
    }
    LlmResponse {
        text,
        model: model.to_string(),
        input_tokens,
        output_tokens,
        latency_ms,
        cost_usd,
        finish_reason,
    }
}

/// The reply text the pipeline parses: the `toolUse.input` JSON for a
/// structured request, else the joined text blocks.
///
/// Why: a forced call always answers with a `toolUse` block, but an `auto`
/// call (#9292) may answer in prose; that prose must reach the caller's
/// parser, which parses JSON text or fails closed, never an empty string
/// read as success.
/// What: when `structured`, [`tool_use::extract_tool_use_json`], falling back
/// to [`extract_converse_text`]; otherwise the text alone. Empty when neither
/// yields anything.
/// Test: `auto_mode_free_text_reply_reaches_the_review_parser`.
fn response_text(
    resp: &aws_sdk_bedrockruntime::operation::converse::ConverseOutput,
    structured: bool,
) -> String {
    let tool_json = if structured {
        tool_use::extract_tool_use_json(resp)
    } else {
        None
    };
    tool_json
        .or_else(|| extract_converse_text(resp))
        .unwrap_or_default()
}

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
