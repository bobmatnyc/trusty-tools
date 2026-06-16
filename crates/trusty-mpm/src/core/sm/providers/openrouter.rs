//! OpenRouter LLM provider for the SM (vendored, DOC-14 §5.1).
//!
//! Why: OpenRouter is one of the SM's three providers; it speaks the
//! OpenAI-compatible `/v1/chat/completions` endpoint with bearer auth. We
//! vendor a focused, non-streaming implementation (ported from trusty-review's
//! `llm/openrouter.rs`) so the SM's `complete()` call reliably captures token
//! usage from the non-streaming response body.
//! What: [`OpenRouterProvider`] reads an API key + bare model id, and a
//! configurable base URL (so tests can point it at a local mock). `complete`
//! POSTs `stream: false`, maps HTTP errors to [`SmLlmError`], and returns
//! [`LlmResponse`] with token usage, wall-clock latency, and an estimated cost.
//! Test: `complete_roundtrips_against_mock`, `complete_maps_http_errors`,
//! `new_rejects_empty_key` in the test module.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::{LlmProvider, LlmRequest, LlmResponse, error::SmLlmError, pricing};

/// Default OpenRouter chat-completions endpoint.
pub const DEFAULT_OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const CONNECT_TIMEOUT_SECS: u64 = 10;
const READ_TIMEOUT_SECS: u64 = 120;
const HTTP_REFERER: &str = "https://github.com/bobmatnyc/trusty-tools";
const X_TITLE: &str = "trusty-mpm-session-manager";

// ─── Wire types (non-streaming) ───────────────────────────────────────────────

/// OpenRouter non-streaming request body.
#[derive(Debug, Serialize)]
struct OrcRequest<'a> {
    model: &'a str,
    messages: &'a [OrcMessage],
    stream: bool,
    temperature: f32,
    max_tokens: u32,
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

// ─── Provider ──────────────────────────────────────────────────────────────────

/// OpenRouter provider for the SM.
///
/// Why: satisfies [`LlmProvider`] over the OpenRouter API; the configurable
/// `base_url` lets unit tests drive it against a local mock without real
/// network calls.
/// What: holds the api key, bare model id, base URL, and a pooled
/// `reqwest::Client`. `complete` POSTs a non-streaming request and maps the
/// outcome to [`LlmResponse`] / [`SmLlmError`].
/// Test: `complete_roundtrips_against_mock`, `complete_maps_http_errors`.
#[derive(Debug)]
pub struct OpenRouterProvider {
    api_key: String,
    model: String,
    base_url: String,
    client: reqwest::Client,
}

impl OpenRouterProvider {
    /// Construct a provider for the given model + API key (default endpoint).
    ///
    /// Why: the resolver builds this from `OPENROUTER_API_KEY` and the bare
    /// model id once the routing prefix is stripped.
    /// What: delegates to [`Self::with_base_url`] using
    /// [`DEFAULT_OPENROUTER_URL`].
    /// Test: `new_rejects_empty_key`.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self, SmLlmError> {
        Self::with_base_url(api_key, model, DEFAULT_OPENROUTER_URL)
    }

    /// Construct a provider with an explicit base URL (used by tests/mocks).
    ///
    /// Why: tests must point the provider at a local `TcpListener` mock; the
    /// default endpoint is not reachable (and must not be hit) in unit tests.
    /// What: validates the key is non-empty, builds a `reqwest::Client` with
    /// connect/read timeouts, and stores the fields. Returns
    /// [`SmLlmError::AccessDenied`] on an empty key.
    /// Test: `new_rejects_empty_key`, `complete_roundtrips_against_mock`.
    pub fn with_base_url(
        api_key: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Result<Self, SmLlmError> {
        let api_key = api_key.into();
        if api_key.is_empty() {
            return Err(SmLlmError::AccessDenied(
                "OPENROUTER_API_KEY is empty".to_string(),
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
            base_url: base_url.into(),
            client,
        })
    }
}

#[async_trait]
impl LlmProvider for OpenRouterProvider {
    fn name(&self) -> &str {
        "openrouter"
    }

    /// Execute a non-streaming OpenRouter completion.
    ///
    /// Why: the SM needs full text + usage in one call (§5.5).
    /// What: builds the message list (optional system + turns), POSTs with
    /// `stream: false`, maps HTTP status codes to [`SmLlmError`] variants,
    /// extracts text + token counts, measures latency, and computes cost. Logs
    /// per-call cost/usage to stderr via `tracing` (§5.5; never stdout).
    /// Test: `complete_roundtrips_against_mock`, `complete_maps_http_errors`.
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, SmLlmError> {
        let mut messages = Vec::with_capacity(req.messages.len() + 1);
        if !req.system.is_empty() {
            messages.push(OrcMessage {
                role: "system".to_string(),
                content: req.system.clone(),
            });
        }
        for m in &req.messages {
            messages.push(OrcMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            });
        }

        let body = OrcRequest {
            model: &self.model,
            messages: &messages,
            stream: false,
            temperature: req.temperature,
            max_tokens: req.max_tokens,
        };

        let start = Instant::now();
        let http_resp = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", HTTP_REFERER)
            .header("X-Title", X_TITLE)
            .json(&body)
            .send()
            .await
            .map_err(|e| SmLlmError::Transport(e.to_string()))?;

        let latency_ms = start.elapsed().as_millis() as u64;
        let status = http_resp.status();

        if !status.is_success() {
            let text = http_resp.text().await.unwrap_or_default();
            return Err(match status.as_u16() {
                // A 400 is a malformed request (bad params / body). It is a
                // non-retryable client alarm — retrying an identical request
                // just wastes calls — so map it to a non-retryable Validation
                // error, consistent with the Anthropic provider.
                400 => SmLlmError::Validation(text),
                401 | 403 => SmLlmError::AccessDenied(text),
                404 => SmLlmError::ModelNotFound(format!("model={}: {text}", self.model)),
                422 => SmLlmError::Validation(text),
                429 => SmLlmError::RateLimited,
                code => SmLlmError::Upstream {
                    status: code,
                    body: text,
                },
            });
        }

        let orc: OrcResponse = http_resp.json().await.map_err(|e| {
            warn!("failed to parse OpenRouter response: {e}");
            SmLlmError::Upstream {
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
        let cost_usd = pricing::estimate_cost_usd(&model_used, input_tokens, output_tokens);

        debug!(
            provider = "openrouter",
            model = %model_used,
            input_tokens,
            output_tokens,
            latency_ms,
            cost_usd,
            "sm openrouter complete"
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
#[path = "openrouter_tests.rs"]
mod tests;
