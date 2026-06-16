//! Direct Anthropic `/v1/messages` provider for the SM (DOC-14 §5.1, D5.2).
//!
//! Why: the third SM provider talks to `api.anthropic.com` directly (lower
//! latency, no OpenRouter markup), which requires Anthropic's NATIVE wire
//! format: `system` is a top-level field, messages are role/content pairs, and
//! the response is a flat `content[]` array of typed blocks with a
//! `stop_reason` and a `usage` object — not OpenAI's nested `choices`. This is
//! the third provider §5.2 D5.2 calls for, porting trusty-agents'
//! `anthropic_native` request/response shape (without its async-openai tool
//! coupling, which the SM does not need for plain reasoning calls).
//! What: [`AnthropicProvider`] reads `ANTHROPIC_API_KEY`, a bare model id, and
//! a configurable base URL (default `https://api.anthropic.com`, overridable
//! via `ANTHROPIC_BASE_URL` and by tests). `complete` POSTs to `{base}/v1/messages`
//! with `x-api-key` + `anthropic-version: 2023-06-01`, maps HTTP errors to
//! [`SmLlmError`], and extracts text + token usage from the native response.
//! Test: `complete_roundtrips_against_mock`, `complete_maps_http_errors`,
//! `new_rejects_empty_key`, `build_body_places_system_top_level` in tests.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::{debug, warn};

use super::{LlmProvider, LlmRequest, LlmResponse, error::SmLlmError, pricing};

/// Default Anthropic API base (no trailing `/v1/messages`).
pub const DEFAULT_ANTHROPIC_BASE: &str = "https://api.anthropic.com";
/// Env var overriding the Anthropic base URL (§5.1).
pub const ENV_ANTHROPIC_BASE_URL: &str = "ANTHROPIC_BASE_URL";
/// Anthropic API version header value (§5.1).
const ANTHROPIC_VERSION: &str = "2023-06-01";
const CONNECT_TIMEOUT_SECS: u64 = 10;
const READ_TIMEOUT_SECS: u64 = 120;

// ─── Request / response shaping (native Anthropic wire format) ─────────────────

/// Build the native Anthropic `/v1/messages` request body from an SM request.
///
/// Why: Anthropic's format differs from OpenAI's enough that a serde round-trip
/// is insufficient — `system` is top-level and messages omit the system turn
/// (ported from trusty-agents `anthropic_native::build_anthropic_request`,
/// minus the tool plumbing the SM does not use).
/// What: lifts `req.system` to a top-level `system` field (when non-empty), and
/// maps each `ChatMessage` to `{role, content}`. `model` is the already-bare id.
/// Test: `build_body_places_system_top_level`.
fn build_body(req: &LlmRequest) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let role = if m.role == "assistant" {
                "assistant"
            } else {
                "user"
            };
            json!({ "role": role, "content": m.content })
        })
        .collect();

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
        "messages": messages,
    });
    if !req.system.is_empty() {
        body["system"] = Value::String(req.system.clone());
    }
    body
}

/// Extract `(text, input_tokens, output_tokens)` from a native Anthropic
/// `/v1/messages` response.
///
/// Why: the response is a flat `content[]` of typed blocks; we concatenate the
/// `text` blocks and read `usage.input_tokens` / `usage.output_tokens` (ported
/// from `anthropic_native::parse_anthropic_response`, text path only).
/// What: joins all `type == "text"` blocks with newlines; reads usage,
/// defaulting absent counts to `0`.
/// Test: covered by `complete_roundtrips_against_mock`.
fn parse_response(value: &Value) -> (String, u32, u32) {
    let mut text = String::new();
    if let Some(blocks) = value.get("content").and_then(|c| c.as_array()) {
        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) == Some("text")
                && let Some(s) = block.get("text").and_then(|t| t.as_str())
            {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(s);
            }
        }
    }
    let (input, output) = value
        .get("usage")
        .map(|u| {
            (
                u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            )
        })
        .unwrap_or((0, 0));
    (text, input, output)
}

// ─── Provider ──────────────────────────────────────────────────────────────────

/// Direct Anthropic `/v1/messages` provider for the SM.
///
/// Why: satisfies [`LlmProvider`] against `api.anthropic.com`; the configurable
/// `base` lets tests drive it against a local mock.
/// What: holds the api key, bare model id, base URL, and a pooled
/// `reqwest::Client`. `complete` POSTs the native body and maps the outcome to
/// [`LlmResponse`] / [`SmLlmError`].
/// Test: `complete_roundtrips_against_mock`, `complete_maps_http_errors`.
#[derive(Debug)]
pub struct AnthropicProvider {
    api_key: String,
    model: String,
    base: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    /// Construct a provider for the given model + API key, resolving the base
    /// URL from `ANTHROPIC_BASE_URL` or falling back to the default.
    ///
    /// Why: the resolver builds this from `ANTHROPIC_API_KEY` and the bare
    /// model id; operators may point at a proxy via `ANTHROPIC_BASE_URL`.
    /// What: reads the env override (when non-empty) and delegates to
    /// [`Self::with_base_url`].
    /// Test: `new_rejects_empty_key`.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self, SmLlmError> {
        let base = std::env::var(ENV_ANTHROPIC_BASE_URL)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_ANTHROPIC_BASE.to_string());
        Self::with_base_url(api_key, model, base)
    }

    /// Construct a provider with an explicit base URL (used by tests/mocks).
    ///
    /// Why: tests must point the provider at a local `TcpListener` mock.
    /// What: validates the key is non-empty, builds a timed `reqwest::Client`,
    /// stores the fields (trimming any trailing `/` on the base). Returns
    /// [`SmLlmError::AccessDenied`] on an empty key.
    /// Test: `new_rejects_empty_key`, `complete_roundtrips_against_mock`.
    pub fn with_base_url(
        api_key: impl Into<String>,
        model: impl Into<String>,
        base: impl Into<String>,
    ) -> Result<Self, SmLlmError> {
        let api_key = api_key.into();
        if api_key.is_empty() {
            return Err(SmLlmError::AccessDenied(
                "ANTHROPIC_API_KEY is empty".to_string(),
            ));
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
            .timeout(Duration::from_secs(READ_TIMEOUT_SECS))
            .build()
            .map_err(|e| SmLlmError::Transport(format!("build reqwest client: {e}")))?;
        Ok(Self {
            api_key,
            model: model.into(),
            base: base.into().trim_end_matches('/').to_string(),
            client,
        })
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    /// Execute a direct Anthropic `/v1/messages` completion.
    ///
    /// Why: the SM needs full text + usage from the native endpoint (§5.5).
    /// What: POSTs [`build_body`] to `{base}/v1/messages` with `x-api-key` and
    /// `anthropic-version`, maps HTTP status to [`SmLlmError`], parses the
    /// native response via [`parse_response`], and logs cost/usage to stderr.
    /// Test: `complete_roundtrips_against_mock`, `complete_maps_http_errors`.
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, SmLlmError> {
        let url = format!("{}/v1/messages", self.base);
        let body = build_body(&req);

        let start = Instant::now();
        let http_resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| SmLlmError::Transport(e.to_string()))?;

        let latency_ms = start.elapsed().as_millis() as u64;
        let status = http_resp.status();

        if !status.is_success() {
            let text = http_resp.text().await.unwrap_or_default();
            return Err(match status.as_u16() {
                401 | 403 => SmLlmError::AccessDenied(text),
                404 => SmLlmError::ModelNotFound(format!("model={}: {text}", self.model)),
                400 | 422 => SmLlmError::Validation(text),
                429 => SmLlmError::RateLimited,
                code => SmLlmError::Upstream {
                    status: code,
                    body: text,
                },
            });
        }

        let value: Value = http_resp.json().await.map_err(|e| {
            warn!("failed to parse Anthropic response: {e}");
            SmLlmError::Upstream {
                status: status.as_u16(),
                body: e.to_string(),
            }
        })?;

        let (text, input_tokens, output_tokens) = parse_response(&value);
        let model_used = value
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or(&self.model)
            .to_string();
        let cost_usd = pricing::estimate_cost_usd(&model_used, input_tokens, output_tokens);

        debug!(
            provider = "anthropic",
            model = %model_used,
            input_tokens,
            output_tokens,
            latency_ms,
            cost_usd,
            "sm anthropic complete"
        );

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

#[cfg(test)]
#[path = "anthropic_tests.rs"]
mod tests;
