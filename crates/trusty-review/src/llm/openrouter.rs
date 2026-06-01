//! OpenRouter LLM provider.
//!
//! Why: wraps `trusty_common::chat::OpenRouterProvider` (which speaks the
//! OpenAI-compatible streaming `/v1/chat/completions` endpoint) to satisfy the
//! `LlmProvider` trait.  The common provider is streaming-only; we drain the
//! stream to produce the full response text required by `LlmProvider::complete`.
//!
//! What: `TrustyReviewOpenRouterProvider` reads `OPENROUTER_API_KEY`, takes a
//! model id at construction time, and implements `LlmProvider::complete` by
//! draining the `ChatEvent` channel into a string, capturing token usage from
//! a non-streaming `/v1/chat/completions` call (see implementation note
//! below), and returning `LlmResponse` with wall-clock latency.
//!
//! Implementation note on token usage: OpenRouter's SSE streaming response
//! does not include `usage` in every delta frame.  Some models send a final
//! frame with `usage.prompt_tokens` / `usage.completion_tokens`; others omit
//! it entirely.  For Stage 1 we make a non-streaming POST to
//! `/v1/chat/completions` (with `stream: false`) which reliably returns usage
//! in the response body.  This avoids the complexity of two calls or of
//! parsing streamed usage frames.
//!
//! Test: `complete_builds_correct_request` verifies the request structure
//! against a mock HTTP server (no real network calls in tests).

use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::{LlmProvider, LlmRequest, LlmResponse, error::LlmError};

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const CONNECT_TIMEOUT_SECS: u64 = 10;
const READ_TIMEOUT_SECS: u64 = 120;
const HTTP_REFERER: &str = "https://github.com/bobmatnyc/trusty-tools";
const X_TITLE: &str = "trusty-review";

// ─── Wire types (non-streaming) ───────────────────────────────────────────────

/// OpenRouter `response_format` field for structured JSON output.
///
/// Why: when `response_schema` is set in `LlmRequest`, we send this field
/// to force the model to emit a JSON object conforming to the schema.
/// OpenRouter passes the `json_schema` type through to providers that support
/// it (e.g. Anthropic via the structured outputs API).
/// What: `type_` is always `"json_schema"`; `json_schema` holds the name,
/// `strict` flag, and the schema value.
/// Test: `complete_with_schema_sends_response_format` in tests module.
#[derive(Debug, Serialize)]
struct OrcResponseFormat<'a> {
    #[serde(rename = "type")]
    type_: &'static str,
    json_schema: OrcJsonSchema<'a>,
}

/// The `json_schema` sub-object in `response_format`.
#[derive(Debug, Serialize)]
struct OrcJsonSchema<'a> {
    name: &'a str,
    strict: bool,
    schema: &'a serde_json::Value,
}

/// OpenRouter non-streaming request body.
#[derive(Debug, Serialize)]
struct OrcRequest<'a> {
    model: &'a str,
    messages: &'a [OrcMessage],
    stream: bool,
    temperature: f32,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<OrcResponseFormat<'a>>,
}

/// Single message in the OpenRouter request.
#[derive(Debug, Serialize)]
struct OrcMessage {
    role: String,
    content: String,
}

/// OpenRouter non-streaming response body (only fields we use).
#[derive(Debug, Deserialize)]
struct OrcResponse {
    choices: Vec<OrcChoice>,
    #[serde(default)]
    usage: Option<OrcUsage>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OrcChoice {
    message: OrcChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct OrcChoiceMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OrcUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

// ─── Pricing table ────────────────────────────────────────────────────────────

/// Approximate cost per million tokens for known GPT-5-class models.
///
/// Why: enables cost estimation in `LlmResponse` for the `compare` mode that
/// ranks models by speed/cost/effectiveness.
/// What: `(input_cost_per_m, output_cost_per_m)` in USD.  Values are per the
/// OpenRouter model pricing page (June 2026) for the version-stamped slugs.
/// Unknown model ids fall back to zero (no estimate).
///
/// Source: OpenRouter model pricing page, June 2026.
/// IMPORTANT: OpenRouter slugs are version-stamped; update this table when
/// new model versions replace old ones or pricing changes.
fn cost_per_million(model: &str) -> (f64, f64) {
    match model {
        // GPT-5.5 Pro — top-tier, high cost.
        "openai/gpt-5.5-pro-20260423" => (30.00, 180.00),
        // GPT-5.5 — high quality, mid-premium cost.
        "openai/gpt-5.5-20260423" => (5.00, 30.00),
        // GPT-5.4 — standard quality.
        "openai/gpt-5.4-20260305" => (2.50, 15.00),
        // GPT-5.4 Mini — cost-effective default reviewer.
        "openai/gpt-5.4-mini-20260317" => (0.75, 4.50),
        // GPT-5.4 Nano — cheapest; default verifier and summarizer.
        "openai/gpt-5.4-nano-20260317" => (0.20, 1.25),
        // Unknown model — no cost estimate.
        _ => (0.0, 0.0),
    }
}

/// Compute USD cost estimate from token counts and model pricing.
///
/// Why: surfaces cost per call so `compare` mode can rank by cost-efficiency.
/// What: applies `cost_per_million` pricing table; returns 0.0 for unknown
/// models.
/// Test: `cost_estimate_for_known_model` and `cost_estimate_for_unknown_model`.
pub fn estimate_cost_usd(model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
    let (in_price, out_price) = cost_per_million(model);
    (input_tokens as f64 / 1_000_000.0) * in_price
        + (output_tokens as f64 / 1_000_000.0) * out_price
}

// ─── Provider implementation ──────────────────────────────────────────────────

/// OpenRouter LLM provider for trusty-review.
///
/// Why: satisfies `LlmProvider` using the OpenRouter API; wraps the wire
/// protocol rather than reusing the streaming `trusty_common::chat::
/// OpenRouterProvider` so we can reliably capture token usage from the
/// non-streaming response.
/// What: takes `api_key` and `model` at construction time.  `complete` POSTs
/// a non-streaming request to OpenRouter, captures the response text, token
/// usage, and wall-clock latency, then maps HTTP / network errors to the
/// appropriate `LlmError` variant.
/// Test: `complete_builds_correct_request` uses a mock server; see test module.
#[derive(Debug)]
pub struct OpenRouterProvider {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl OpenRouterProvider {
    /// Construct a provider for the given model and API key.
    ///
    /// Why: callers obtain the api_key from `ReviewConfig::openrouter_api_key`
    /// and the model from a resolved `RoleConfig::model`.
    /// What: builds a `reqwest::Client` with connect and read timeouts;
    /// returns `LlmError::AccessDenied` if the key is empty.
    /// Test: `new_returns_error_on_empty_key`.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self, LlmError> {
        let api_key = api_key.into();
        if api_key.is_empty() {
            return Err(LlmError::AccessDenied(
                "OPENROUTER_API_KEY is empty".to_string(),
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
            client,
        })
    }

    /// Construct from `ReviewConfig`, reading the API key from the config.
    ///
    /// Why: convenience constructor so callers don't repeat `config.openrouter_api_key`.
    /// What: delegates to `new`.
    /// Test: covered by integration tests that construct from config.
    pub fn from_config(
        config: &crate::config::ReviewConfig,
        model: impl Into<String>,
    ) -> Result<Self, LlmError> {
        Self::new(config.openrouter_api_key.clone(), model)
    }
}

#[async_trait]
impl LlmProvider for OpenRouterProvider {
    fn name(&self) -> &str {
        "openrouter"
    }

    /// Execute a non-streaming completion request and return the full response.
    ///
    /// Why: the pipeline needs a full text response (not a stream) to extract
    /// findings and compute token usage.
    /// What: POSTs to `/v1/chat/completions` with `stream: false`, maps HTTP
    /// errors to `LlmError` variants, extracts text + token counts, measures
    /// wall-clock latency, and computes cost.  When `req.response_schema` is
    /// set, sends `response_format: { type: "json_schema", json_schema: { name,
    /// strict: true, schema } }` to force the model to emit structured JSON;
    /// the assistant message content will be the clean JSON object.
    /// Test: `complete_builds_correct_request`,
    /// `complete_with_schema_sends_response_format` (mock server in tests).
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        debug!(
            model = %self.model,
            structured = req.response_schema.is_some(),
            "openrouter complete request"
        );

        // Build message list: optional system message followed by user turns.
        let mut messages = Vec::new();
        if !req.system.is_empty() {
            messages.push(OrcMessage {
                role: "system".to_string(),
                content: req.system.clone(),
            });
        }
        for msg in &req.messages {
            messages.push(OrcMessage {
                role: msg.role.clone(),
                content: msg.content.clone(),
            });
        }

        // Build response_format when structured output is requested.
        let response_format = req.response_schema.as_ref().map(|s| OrcResponseFormat {
            type_: "json_schema",
            json_schema: OrcJsonSchema {
                name: &s.name,
                strict: true,
                schema: &s.schema,
            },
        });

        let body = OrcRequest {
            model: &self.model,
            messages: &messages,
            stream: false,
            temperature: req.temperature,
            max_tokens: req.max_tokens,
            response_format,
        };

        let start = Instant::now();

        let http_resp = self
            .client
            .post(OPENROUTER_URL)
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", HTTP_REFERER)
            .header("X-Title", X_TITLE)
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

        let orc: OrcResponse = http_resp.json().await.map_err(|e| {
            warn!("failed to parse OpenRouter response: {e}");
            LlmError::Upstream {
                status: status.as_u16(),
                body: e.to_string(),
            }
        })?;

        let text = orc
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();

        let (input_tokens, output_tokens) = orc
            .usage
            .map(|u| (u.prompt_tokens, u.completion_tokens))
            .unwrap_or((0, 0));

        let model_used = orc.model.unwrap_or_else(|| self.model.clone());
        let cost_usd = estimate_cost_usd(&model_used, input_tokens, output_tokens);

        Ok(LlmResponse {
            text,
            model: model_used,
            input_tokens,
            output_tokens,
            latency_ms,
            cost_usd,
        })
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

// ─── Unit tests ─────────────────────────────────────────────────────────────
// Tests extracted to openrouter_tests.rs to keep this file under the 500-line cap.

#[cfg(test)]
#[path = "openrouter_tests.rs"]
mod tests;
