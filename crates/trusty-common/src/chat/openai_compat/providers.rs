//! Concrete OpenAI-compatible chat provider implementations.
//!
//! Why: OpenRouter (cloud) and Ollama (local) both use the same SSE pump but
//! differ in auth, URL, and timeout configuration. Keeping both concrete
//! providers in one file makes the symmetry obvious and lets us share the
//! `build_consolidation_prompt`-style helpers without re-importing.
//! What: `OpenRouterProvider`, `OllamaProvider`, and
//! `auto_detect_local_provider` — the public surface that callers import from
//! the `openai_compat` module.
//! Test: `openrouter_provider_reports_metadata`,
//! `ollama_provider_reports_metadata`, `ollama_provider_streams_sse_deltas`,
//! `ollama_provider_emits_tool_call`,
//! `auto_detect_returns_none_on_unreachable`,
//! `auto_detect_returns_some_on_200`.

use super::sse_pump::pump_openai_sse;
use super::wire::tools_wire;
use crate::ChatMessage;
use crate::chat::{ChatEvent, ChatProvider, ToolDef};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use tokio::sync::mpsc::Sender;

const LOCAL_PROBE_TIMEOUT_SECS: u64 = 1;
const LOCAL_REQUEST_TIMEOUT_SECS: u64 = 120;
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const OPENROUTER_CONNECT_TIMEOUT_SECS: u64 = 10;
const OPENROUTER_REQUEST_TIMEOUT_SECS: u64 = 120;
const HTTP_REFERER: &str = "https://github.com/bobmatnyc/trusty-common";
const X_TITLE: &str = "trusty-common";

/// Cloud chat provider backed by OpenRouter.
///
/// Why: lets callers pick OpenRouter or a local model uniformly through
/// the [`ChatProvider`] trait.
/// What: stores an API key and model id; POSTs OpenAI-compatible streaming
/// chat completions with bearer auth and trusty-common branding headers.
/// Test: shape covered by `openrouter_provider_reports_metadata`; the
/// streaming and tool-call paths are covered by integration tests in
/// downstream crates plus the SSE-pump unit tests in this module.
pub struct OpenRouterProvider {
    pub api_key: String,
    pub model: String,
}

impl OpenRouterProvider {
    /// Construct a provider from an API key and model id.
    ///
    /// Why: keeps callers from poking the public fields directly so the
    /// struct can grow optional knobs without breaking call sites.
    /// What: stores both fields verbatim.
    /// Test: trivially exercised by `openrouter_provider_reports_metadata`.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
        }
    }
}

#[async_trait]
impl ChatProvider for OpenRouterProvider {
    fn name(&self) -> &str {
        "openrouter"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDef>,
        tx: Sender<ChatEvent>,
    ) -> Result<()> {
        if self.api_key.is_empty() {
            return Err(anyhow!("openrouter api key is empty"));
        }
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(
                OPENROUTER_CONNECT_TIMEOUT_SECS,
            ))
            .timeout(std::time::Duration::from_secs(
                OPENROUTER_REQUEST_TIMEOUT_SECS,
            ))
            .build()
            .context("build reqwest client for OpenRouterProvider::chat_stream")?;

        let tw = tools_wire(&tools);
        let body = super::wire::ChatRequestWire {
            model: &self.model,
            messages: &messages,
            stream: true,
            tools: tw,
        };
        let resp = client
            .post(OPENROUTER_URL)
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", HTTP_REFERER)
            .header("X-Title", X_TITLE)
            .json(&body)
            .send()
            .await
            .context("POST openrouter chat completions (stream)")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("openrouter HTTP {status}: {text}"));
        }

        pump_openai_sse(resp, tx).await
    }
}

/// Local chat provider for OpenAI-compatible servers (Ollama, LM Studio,
/// llama.cpp's `server`, vLLM, etc.).
///
/// Why: developers increasingly run a local model server during dev to avoid
/// API costs and latency. The OpenAI-compatible `/v1/chat/completions`
/// endpoint with SSE streaming is the de-facto common denominator.
/// What: stores the server's base URL and the model id to request.
/// `chat_stream` POSTs `{model, messages, tools?, stream: true}` and parses
/// SSE `data:` frames identically to the OpenRouter path.
/// Test: shape covered by `ollama_provider_reports_metadata`; streaming and
/// tool-call accumulation by `ollama_provider_streams_sse_deltas` and
/// `accumulates_streamed_tool_call_fragments`.
pub struct OllamaProvider {
    pub base_url: String,
    pub model: String,
}

impl OllamaProvider {
    /// Construct a provider from a base URL and model id.
    ///
    /// Why: parallel to [`OpenRouterProvider::new`] so callers see a
    /// consistent shape across providers.
    /// What: stores both fields verbatim; the base URL should NOT have a
    /// trailing slash — the implementation appends `/v1/chat/completions`.
    /// Test: covered by `ollama_provider_reports_metadata`.
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
        }
    }
}

#[async_trait]
impl ChatProvider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<ToolDef>,
        tx: Sender<ChatEvent>,
    ) -> Result<()> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(LOCAL_PROBE_TIMEOUT_SECS))
            .timeout(std::time::Duration::from_secs(LOCAL_REQUEST_TIMEOUT_SECS))
            .build()
            .context("build reqwest client for OllamaProvider::chat_stream")?;

        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        let tw = tools_wire(&tools);
        let body = super::wire::ChatRequestWire {
            model: &self.model,
            messages: &messages,
            stream: true,
            tools: tw,
        };
        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("local chat HTTP {status}: {text}"));
        }

        pump_openai_sse(resp, tx).await
    }
}

/// Probe a local model server and return an [`OllamaProvider`] if reachable.
///
/// Why: at startup, downstream daemons want to know whether a local model
/// server is running before falling back to a cloud provider. The OpenAI
/// `/v1/models` endpoint is a cheap, side-effect-free liveness check that
/// Ollama, LM Studio, and llama.cpp's server all implement.
/// What: GETs `{base_url}/v1/models` with a 1-second total timeout. Returns
/// `Some(OllamaProvider { base_url, model: "" })` on any 2xx response.
/// Returns `None` on network errors, timeouts, or non-2xx status. Never
/// returns an error — the caller treats absence as "no local provider
/// available" and is responsible for setting the model id afterwards (e.g.
/// from [`super::LocalModelConfig::model`]).
/// Test: `auto_detect_returns_none_on_unreachable` points at a closed port
/// and asserts `None` within the 1-second budget;
/// `auto_detect_returns_some_on_200` spins up an in-process server and
/// asserts a provider is returned.
pub async fn auto_detect_local_provider(base_url: &str) -> Option<OllamaProvider> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(LOCAL_PROBE_TIMEOUT_SECS))
        .timeout(std::time::Duration::from_secs(LOCAL_PROBE_TIMEOUT_SECS))
        .build()
        .ok()?;

    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => {
            Some(OllamaProvider::new(base_url.to_string(), String::new()))
        }
        _ => None,
    }
}
