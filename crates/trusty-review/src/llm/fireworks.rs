//! Fireworks.ai LLM provider.
//!
//! Why: Fireworks hosts fast open-weight models behind an OpenAI-compatible
//! `/chat/completions` endpoint and supports constrained-generation structured
//! output — the property trusty-review's pipeline depends on to avoid the
//! silent fail-safe APPROVE problem.  Routing to Fireworks directly (rather
//! than through OpenRouter) removes a middleman and matches trusty-code's
//! `fireworks/*` slug convention.
//!
//! What: `FireworksProvider` takes an API key (`FIREWORKS_API_KEY` via
//! `ReviewConfig::fireworks_api_key`) and a bare provider-native model id
//! (`accounts/fireworks/models/<model>` — the `fireworks/` routing prefix is
//! stripped by `resolve_provider_and_model` before construction).  `complete`
//! POSTs a non-streaming OpenAI-compatible request; when
//! `LlmRequest.response_schema` is set it sends `response_format: { type:
//! "json_schema", json_schema: { name, schema } }`.  Unlike OpenRouter, the
//! Fireworks API documents NO `strict` field — the schema itself is enforced
//! by constrained generation, so `strict` is intentionally omitted from the
//! wire format (https://docs.fireworks.ai/structured-responses/
//! structured-response-formatting).
//!
//! Caveats from the Fireworks docs, honored by the pipeline already:
//!   - Prompts must instruct the model to produce JSON (all trusty-review
//!     structured prompts do) or the model may emit whitespace until the
//!     token limit.
//!   - On reasoning models (e.g. kimi-k2p6), reasoning goes to a separate
//!     `reasoning_content` field the pipeline discards — but it still burns
//!     the `max_tokens` budget, so an unmitigated structured call at the
//!     verifier's tight ceiling truncates mid-reasoning and yields no JSON.
//!     Structured calls therefore send `reasoning_effort: "none"` (live-
//!     verified 2026-07-15: clean enum-conforming JSON in 85 completion
//!     tokens at a 128-token ceiling; the documented Anthropic-style
//!     `thinking: {"type": "disabled"}` works equally).  Override via
//!     `TRUSTY_REVIEW_FIREWORKS_REASONING_EFFORT` (`low`/`medium`/`high`
//!     to re-enable reasoning, `default` to omit the field entirely).
//!
//! The base URL is a constructor field (default `FIREWORKS_BASE_URL`,
//! overridable via `TRUSTY_REVIEW_FIREWORKS_BASE_URL`) so tests can do a real
//! mock-server round-trip — mirroring trusty-code's `TCODE_FIREWORKS_BASE_URL`
//! pattern.
//!
//! Test: `fireworks_tests.rs` — construction, cost estimation, wire-format
//! shape (incl. the no-`strict` invariant), a mock-server round-trip, and an
//! `#[ignore]`d live smoke test that proves enum enforcement on the real API.

use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::{LlmProvider, LlmRequest, LlmResponse, error::LlmError};

/// Default Fireworks API base URL (chat completions lives at
/// `<base>/chat/completions`).  Kept in sync with
/// `trusty_common::inference::providers::fireworks::FIREWORKS_BASE_URL`
/// (not imported: that module is behind the `inference-client` feature,
/// which trusty-review does not enable).
pub const FIREWORKS_BASE_URL: &str = "https://api.fireworks.ai/inference/v1";
/// Env var overriding the base URL (tests / proxies).
pub const FIREWORKS_BASE_URL_ENV: &str = "TRUSTY_REVIEW_FIREWORKS_BASE_URL";
/// Env var overriding the `reasoning_effort` sent on structured-output calls.
///
/// Unset ⇒ `"none"` (reasoning disabled — the pipeline discards
/// `reasoning_content`, so reasoning tokens on verdict calls are pure cost and
/// truncate tight budgets like the verifier's).  Set `low`/`medium`/`high` to
/// re-enable reasoning, or `default` to omit the field from the request.
pub const FIREWORKS_REASONING_EFFORT_ENV: &str = "TRUSTY_REVIEW_FIREWORKS_REASONING_EFFORT";
/// Default `reasoning_effort` for structured-output calls.
const DEFAULT_STRUCTURED_REASONING_EFFORT: &str = "none";
/// Sentinel env value meaning "omit the `reasoning_effort` field entirely".
const REASONING_EFFORT_OMIT: &str = "default";
const CONNECT_TIMEOUT_SECS: u64 = 10;
const READ_TIMEOUT_SECS: u64 = 120;

// ─── Wire types (non-streaming) ───────────────────────────────────────────────

/// Fireworks `response_format` field for structured JSON output.
///
/// Why: when `response_schema` is set in `LlmRequest`, this forces the model
/// to emit a JSON object conforming to the schema via Fireworks'
/// constrained-generation JSON mode.
/// What: `type_` is always `"json_schema"`; `json_schema` holds the name and
/// schema.  NO `strict` field — Fireworks does not document one (enforcement
/// is inherent), and sending undocumented fields risks a validation rejection.
/// Test: `complete_with_schema_sends_response_format_without_strict`.
#[derive(Debug, Serialize)]
struct FwResponseFormat<'a> {
    #[serde(rename = "type")]
    type_: &'static str,
    json_schema: FwJsonSchema<'a>,
}

/// The `json_schema` sub-object in `response_format`.
#[derive(Debug, Serialize)]
struct FwJsonSchema<'a> {
    name: &'a str,
    schema: &'a serde_json::Value,
}

/// Fireworks non-streaming request body (OpenAI-compatible).
#[derive(Debug, Serialize)]
struct FwRequest<'a> {
    model: &'a str,
    messages: &'a [FwMessage],
    stream: bool,
    temperature: f32,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<FwResponseFormat<'a>>,
    /// Reasoning-token control for reasoning models (kimi-k2p6 etc.).  Set to
    /// `"none"` on structured calls so reasoning doesn't burn the `max_tokens`
    /// budget before the constrained JSON is emitted (see module docs);
    /// omitted (`None`) on free-text calls and when the operator opts out via
    /// `TRUSTY_REVIEW_FIREWORKS_REASONING_EFFORT=default`.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
}

/// Single message in the Fireworks request.
#[derive(Debug, Serialize)]
struct FwMessage {
    role: String,
    content: String,
}

/// Fireworks non-streaming response body (only fields we use).
#[derive(Debug, Deserialize)]
struct FwResponse {
    choices: Vec<FwChoice>,
    #[serde(default)]
    usage: Option<FwUsage>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FwChoice {
    message: FwChoiceMessage,
    /// OpenAI-compatible completion reason: `"stop"`, `"length"` (hit
    /// `max_tokens` → truncated), etc.  Threaded into
    /// `LlmResponse.finish_reason` as the PRIMARY truncation signal (#1357).
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FwChoiceMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FwUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

// ─── Pricing table ────────────────────────────────────────────────────────────

/// Best-effort `(input, output)` USD-per-million pricing for Fireworks slugs.
///
/// Why: enables cost estimation in `LlmResponse` for `compare` mode, matching
/// the convention of `openrouter::estimate_cost_usd`.  Fireworks serverless
/// slugs (`accounts/fireworks/models/*`) are not version-stamped uniformly, so
/// this uses family/size substring matching (like the Gemini table in
/// `openrouter.rs`) rather than exact ids.
/// What: returns per-family pricing when the lowercased slug contains a
/// matching token; `(0.0, 0.0)` for unknown slugs so cost is simply
/// unestimated rather than wrong.
///
/// PRICING PROVENANCE:
///   Source:    Fireworks serverless pricing page —
///              https://fireworks.ai/pricing (per-million input/output USD).
///   Reference date: 2026-07-13 (best-effort snapshot; family substring
///   match, NOT per-slug exact pricing).
///   TODO: refresh pricing when new families are added to the compare set.
/// Test: `cost_estimate_*` in `fireworks_tests.rs`.
fn cost_per_million(model: &str) -> (f64, f64) {
    let m = model.to_ascii_lowercase();
    // Order matters: check the most specific family tokens first.
    if m.contains("kimi-k2") {
        (0.60, 2.50)
    } else if m.contains("deepseek-r1") {
        (3.00, 8.00)
    } else if m.contains("deepseek-v3") {
        (0.90, 0.90)
    } else if m.contains("405b") {
        (3.00, 3.00)
    } else if m.contains("70b") || m.contains("72b") {
        (0.90, 0.90)
    } else if m.contains("8b") || m.contains("7b") {
        (0.20, 0.20)
    } else {
        (0.0, 0.0)
    }
}

/// Compute USD cost estimate from token counts and model pricing.
///
/// Why: surfaces cost per call so `compare` mode can rank by cost-efficiency.
/// What: applies the `cost_per_million` family table; returns 0.0 for unknown
/// models.
/// Test: `cost_estimate_kimi`, `cost_estimate_unknown_model_is_zero`.
pub fn estimate_cost_usd(model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
    let (in_price, out_price) = cost_per_million(model);
    (input_tokens as f64 / 1_000_000.0) * in_price
        + (output_tokens as f64 / 1_000_000.0) * out_price
}

// ─── Provider implementation ──────────────────────────────────────────────────

/// Fireworks.ai LLM provider for trusty-review.
///
/// Why: satisfies `LlmProvider` against the Fireworks OpenAI-compatible API
/// so `fireworks/*` model slugs work for every role without an OpenRouter
/// middleman.
/// What: takes `api_key` and a bare `accounts/fireworks/models/*` model id at
/// construction time.  `complete` POSTs a non-streaming request, captures the
/// response text, token usage, and wall-clock latency, then maps HTTP /
/// network errors to the appropriate `LlmError` variant.
/// Test: `complete_round_trips_through_mock_server` exercises the real HTTP
/// path via the injectable base URL.
#[derive(Debug)]
pub struct FireworksProvider {
    api_key: String,
    model: String,
    base_url: String,
    /// `reasoning_effort` to send on structured-output calls; `None` = omit
    /// the field (operator set `TRUSTY_REVIEW_FIREWORKS_REASONING_EFFORT=default`).
    structured_reasoning_effort: Option<String>,
    client: reqwest::Client,
}

/// Resolve the structured-call `reasoning_effort` from the environment.
///
/// Why: reasoning models burn `max_tokens` on `reasoning_content` the pipeline
/// discards, truncating tight verdict budgets (module docs); `"none"` is the
/// live-verified mitigation, with an operator escape hatch.
/// What: reads `TRUSTY_REVIEW_FIREWORKS_REASONING_EFFORT` and delegates to the
/// pure [`reasoning_effort_from`] (kept env-free so tests never mutate process
/// env — the #1312 flaky-test hazard).
/// Test: via `reasoning_effort_from_*` in `fireworks_tests.rs`.
fn resolve_structured_reasoning_effort() -> Option<String> {
    reasoning_effort_from(
        std::env::var(FIREWORKS_REASONING_EFFORT_ENV)
            .ok()
            .as_deref(),
    )
}

/// Pure mapping from the raw env value to the structured-call effort.
///
/// Why: separated from the env read so the sentinel/default/passthrough cases
/// are unit-testable without process-env mutation.
/// What: `None`/blank ⇒ `Some("none")` (the protective default); the sentinel
/// `default` (case-insensitive) ⇒ `None` (omit the field); anything else is
/// passed through trimmed (`low`/`medium`/`high`).
/// Test: `reasoning_effort_from_unset_is_none_effort`,
/// `reasoning_effort_from_sentinel_omits`, `reasoning_effort_from_passthrough`.
fn reasoning_effort_from(raw: Option<&str>) -> Option<String> {
    match raw.map(str::trim) {
        Some(v) if v.eq_ignore_ascii_case(REASONING_EFFORT_OMIT) => None,
        Some(v) if !v.is_empty() => Some(v.to_string()),
        _ => Some(DEFAULT_STRUCTURED_REASONING_EFFORT.to_string()),
    }
}

impl FireworksProvider {
    /// Construct a provider for the given model and API key.
    ///
    /// Why: callers obtain the api_key from `ReviewConfig::fireworks_api_key`
    /// and the model from a resolved `RoleConfig::model` (prefix-stripped).
    /// What: resolves the base URL from `TRUSTY_REVIEW_FIREWORKS_BASE_URL`
    /// (default `FIREWORKS_BASE_URL`) and delegates to `with_base_url`.
    /// Returns `LlmError::AccessDenied` if the key is empty.
    /// Test: `new_returns_error_on_empty_key`.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self, LlmError> {
        let base_url = std::env::var(FIREWORKS_BASE_URL_ENV)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| FIREWORKS_BASE_URL.to_string());
        Self::with_base_url(api_key, model, base_url)
    }

    /// Construct a provider with an explicit base URL.
    ///
    /// Why: lets tests point the provider at a mock HTTP server for a real
    /// request/response round-trip — the coverage the hardcoded-URL OpenRouter
    /// provider cannot get.
    /// What: builds a `reqwest::Client` with connect and read timeouts;
    /// returns `LlmError::AccessDenied` if the key is empty.  A trailing `/`
    /// on `base_url` is trimmed so path joining is uniform.
    /// Test: `complete_round_trips_through_mock_server`.
    pub fn with_base_url(
        api_key: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, LlmError> {
        let api_key = api_key.into();
        if api_key.is_empty() {
            return Err(LlmError::AccessDenied(
                "FIREWORKS_API_KEY is empty".to_string(),
            ));
        }
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS))
            .timeout(std::time::Duration::from_secs(READ_TIMEOUT_SECS))
            .build()
            .map_err(|e| LlmError::Transport(format!("build reqwest client: {e}")))?;
        Ok(Self {
            api_key,
            model: model.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            structured_reasoning_effort: resolve_structured_reasoning_effort(),
            client,
        })
    }

    /// Construct from `ReviewConfig`, reading the API key from the config.
    ///
    /// Why: convenience constructor so callers don't repeat
    /// `config.fireworks_api_key`.
    /// What: delegates to `new`.
    /// Test: covered by integration tests that construct from config.
    pub fn from_config(
        config: &crate::config::ReviewConfig,
        model: impl Into<String>,
    ) -> Result<Self, LlmError> {
        Self::new(config.fireworks_api_key.clone(), model)
    }
}

#[async_trait]
impl LlmProvider for FireworksProvider {
    fn name(&self) -> &str {
        "fireworks"
    }

    /// Execute a non-streaming completion request and return the full response.
    ///
    /// Why: the pipeline needs a full text response (not a stream) to extract
    /// findings and compute token usage.
    /// What: POSTs to `<base>/chat/completions` with `stream: false`, maps
    /// HTTP errors to `LlmError` variants, extracts text + token counts,
    /// measures wall-clock latency, and computes cost.  When
    /// `req.response_schema` is set, sends `response_format: { type:
    /// "json_schema", json_schema: { name, schema } }` (no `strict` — see
    /// module docs) to force the model to emit structured JSON; the assistant
    /// message content will be the clean JSON object.
    /// Test: `complete_round_trips_through_mock_server`,
    /// `complete_with_schema_sends_response_format_without_strict`.
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        debug!(
            model = %self.model,
            structured = req.response_schema.is_some(),
            "fireworks complete request"
        );

        // Build message list: optional system message followed by user turns.
        let mut messages = Vec::new();
        if !req.system.is_empty() {
            messages.push(FwMessage {
                role: "system".to_string(),
                content: req.system.clone(),
            });
        }
        for msg in &req.messages {
            messages.push(FwMessage {
                role: msg.role.clone(),
                content: msg.content.clone(),
            });
        }

        // Build response_format when structured output is requested.
        let response_format = req.response_schema.as_ref().map(|s| FwResponseFormat {
            type_: "json_schema",
            json_schema: FwJsonSchema {
                name: &s.name,
                schema: &s.schema,
            },
        });

        // Structured calls disable reasoning so it can't burn the max_tokens
        // budget before the constrained JSON lands (module docs); free-text
        // calls leave the model's default behaviour untouched.
        let reasoning_effort = if response_format.is_some() {
            self.structured_reasoning_effort.as_deref()
        } else {
            None
        };

        let body = FwRequest {
            model: &self.model,
            messages: &messages,
            stream: false,
            temperature: req.temperature,
            max_tokens: req.max_tokens,
            response_format,
            reasoning_effort,
        };

        let start = Instant::now();

        let http_resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;

        let latency_ms = start.elapsed().as_millis() as u64;
        let status = http_resp.status();

        // Map HTTP error codes to LlmError variants.
        if !status.is_success() {
            let body_text = http_resp.text().await.unwrap_or_default();
            return Err(match status.as_u16() {
                401 | 403 => LlmError::AccessDenied(body_text),
                404 => LlmError::ModelNotFound(format!("model={}: {body_text}", self.model)),
                422 => LlmError::Validation(body_text),
                429 => LlmError::RateLimited,
                _ => LlmError::Upstream {
                    status: status.as_u16(),
                    body: body_text,
                },
            });
        }

        let fw: FwResponse = http_resp.json().await.map_err(|e| {
            warn!("failed to parse Fireworks response: {e}");
            LlmError::Upstream {
                status: status.as_u16(),
                body: e.to_string(),
            }
        })?;

        let first_choice = fw.choices.into_iter().next();
        // Capture finish_reason BEFORE consuming the message content (#1357).
        let finish_reason = first_choice
            .as_ref()
            .and_then(|c| c.finish_reason.clone())
            .map(|r| r.trim().to_ascii_lowercase());
        let text = first_choice
            .and_then(|c| c.message.content)
            .unwrap_or_default();

        let (input_tokens, output_tokens) = fw
            .usage
            .map(|u| (u.prompt_tokens, u.completion_tokens))
            .unwrap_or((0, 0));

        let model_used = fw.model.unwrap_or_else(|| self.model.clone());
        let cost_usd = estimate_cost_usd(&model_used, input_tokens, output_tokens);

        Ok(LlmResponse {
            text,
            model: model_used,
            input_tokens,
            output_tokens,
            latency_ms,
            cost_usd,
            finish_reason,
        })
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Tests live in fireworks_tests.rs to keep this file under the 500-line cap.

#[cfg(test)]
#[path = "fireworks_tests.rs"]
mod tests;
