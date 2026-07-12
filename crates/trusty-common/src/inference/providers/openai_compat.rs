//! [`OpenAiCompatAdapter`] — the shared OpenAI-compatible HTTP inference core.
//!
//! Why: OpenRouter, Fireworks, and OpenAI-direct all speak the identical
//! `/chat/completions` schema (OpenAI-style `messages`/`tools`/`tool_choice`,
//! and an OpenAI-shaped response with a `usage` block). Hand-rolling three
//! near-identical HTTP clients is exactly the duplication epic #2400 exists to
//! kill. This is the ONE client every OpenAI-dialect provider is a thin config
//! over: it owns the reqwest mechanics, auth-header injection, the OpenRouter
//! detailed-usage request flag, HTTP→[`InferenceError`] classification, and
//! response parsing. Behaviour-compatible with tcode's `llm::client::LlmClient`
//! so #2406 can drop this in.
//! What: [`OpenAiCompatConfig`] (per-provider knobs: name, base URL, key,
//! extra headers, capabilities) and [`OpenAiCompatAdapter`] implementing
//! [`InferenceAdapter`]. The API key is held as a [`SecretString`] and only ever
//! placed in the `Authorization` header — never logged, never in an error.
//! Test: inline `tests` — `endpoint_appends_path`, `usage_directive_injected`,
//! `usage_directive_absent_without_detailed`, `usage_directive_not_overwritten`;
//! full HTTP round-trip in `crates/trusty-common/tests/inference_adapters.rs`.

use async_trait::async_trait;
use reqwest::Client;

use crate::inference::adapter::InferenceAdapter;
use crate::inference::error::InferenceError;
use crate::inference::registry::ProviderCapabilities;
use crate::inference::types::{ChatRequest, ChatResponse, RequestUsageConfig, SecretString};

/// Per-provider configuration for an [`OpenAiCompatAdapter`].
///
/// Why: the only things that differ between OpenRouter, Fireworks, and
/// OpenAI-direct are their base URL, attribution headers, and capability
/// descriptor — everything else (the wire schema, auth scheme, error mapping)
/// is shared. Bundling the deltas in one struct keeps each provider module a
/// tiny config factory instead of a bespoke client.
/// What: `name` is the stable provider label; `base_url` is the API root the
/// `/chat/completions` path is appended to; `api_key` is the bearer credential
/// (redacting [`SecretString`]); `extra_headers` are provider attribution
/// headers (e.g. OpenRouter's `HTTP-Referer`/`X-Title`); `capabilities` drives
/// the trait introspection defaults (incl. the OpenRouter detailed-usage flag).
/// Test: `endpoint_appends_path`.
pub struct OpenAiCompatConfig {
    /// Stable provider name for logging/diagnostics (e.g. `"openrouter"`).
    pub name: String,
    /// API root URL; `/chat/completions` is appended to it.
    pub base_url: String,
    /// Bearer API key, held redacted; only ever written to the auth header.
    pub api_key: SecretString,
    /// Provider-specific attribution headers as (name, value) pairs.
    pub extra_headers: Vec<(String, String)>,
    /// The provider's capability descriptor (from the registry).
    pub capabilities: ProviderCapabilities,
}

/// The shared OpenAI-compatible HTTP inference adapter.
///
/// Why: one client behind every OpenAI-dialect provider so the request/response
/// translation, auth, and error classification live in exactly one place.
/// What: wraps a cloneable `reqwest::Client`, the precomputed `/chat/completions`
/// endpoint, the redacted key, attribution headers, and the capability
/// descriptor. [`Self::chat`] serialises the (usage-augmented) [`ChatRequest`],
/// POSTs it with a `Bearer` auth header, and maps the outcome to a
/// [`ChatResponse`] or a classified [`InferenceError`].
/// Test: inline `tests` + `crates/trusty-common/tests/inference_adapters.rs`.
pub struct OpenAiCompatAdapter {
    name: String,
    endpoint: String,
    api_key: SecretString,
    extra_headers: Vec<(String, String)>,
    capabilities: ProviderCapabilities,
    http: Client,
}

impl OpenAiCompatAdapter {
    /// Build an adapter from a provider config.
    ///
    /// Why: the single construction path every provider factory funnels through.
    /// What: precomputes the `/chat/completions` endpoint from `base_url`
    /// (tolerating a trailing slash), builds a rustls `reqwest::Client`, and
    /// stores the config. A client-build failure (extremely rare) maps to
    /// [`InferenceError::Transport`] rather than panicking.
    /// Test: `endpoint_appends_path`.
    pub fn new(config: OpenAiCompatConfig) -> Result<Self, InferenceError> {
        let endpoint = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
        let http = Client::builder()
            .use_rustls_tls()
            .build()
            .map_err(|e| InferenceError::Transport(e.to_string()))?;
        Ok(Self {
            name: config.name,
            endpoint,
            api_key: config.api_key,
            extra_headers: config.extra_headers,
            capabilities: config.capabilities,
            http,
        })
    }

    /// The resolved `/chat/completions` endpoint this adapter POSTs to.
    ///
    /// Why: exposed for diagnostics and the inline endpoint-construction test.
    /// What: the precomputed absolute URL.
    /// Test: `endpoint_appends_path`.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Produce the outbound request body, injecting the detailed-usage directive
    /// when this provider wants it.
    ///
    /// Why: OpenRouter only returns its authoritative, cache-discounted
    /// `usage.cost` and nested cache counters when asked via `usage:{include:true}`;
    /// every other provider must never see the directive. Centralising the
    /// decision here (driven by [`InferenceAdapter::wants_detailed_usage`]) keeps
    /// the caller provider-agnostic.
    /// What: clones `request`; if [`Self::wants_detailed_usage`] is `true` and the
    /// caller did not already set `usage`, sets it to
    /// [`RequestUsageConfig::detailed`]. Otherwise returns the request unchanged.
    /// Test: `usage_directive_injected`, `usage_directive_absent_without_detailed`,
    /// `usage_directive_not_overwritten`.
    fn prepare_body(&self, request: &ChatRequest) -> ChatRequest {
        let mut req = request.clone();
        if self.wants_detailed_usage() && req.usage.is_none() {
            req.usage = Some(RequestUsageConfig::detailed());
        }
        req
    }
}

#[async_trait]
impl InferenceAdapter for OpenAiCompatAdapter {
    /// The configured provider name.
    fn name(&self) -> &str {
        &self.name
    }

    /// The provider's capability descriptor.
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    /// POST a chat request and translate the response.
    ///
    /// Why: the one method the whole agent loop depends on; all HTTP mechanics
    /// (auth, headers, status classification, parsing) live here so callers only
    /// speak [`ChatRequest`]/[`ChatResponse`].
    /// What: serialises [`Self::prepare_body`] as JSON, adds `Authorization:
    /// Bearer <key>` plus any attribution headers, and POSTs to the endpoint. A
    /// transport failure maps to [`InferenceError::Transport`]; a non-2xx status
    /// to [`InferenceError::Api`] (retryable for 429/5xx via
    /// [`InferenceError::is_retryable`]); a body that will not parse to
    /// [`InferenceError::Deserialise`]. The API key is only ever placed in the
    /// auth header and never appears in any returned error.
    /// Test: `crates/trusty-common/tests/inference_adapters.rs`.
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let body = self.prepare_body(request);

        let mut builder = self
            .http
            .post(&self.endpoint)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.api_key.expose()),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        for (name, value) in &self.extra_headers {
            builder = builder.header(name, value);
        }

        // `reqwest::Error` display carries the URL/kind but never a header value,
        // so stringifying it cannot leak the API key.
        let resp = builder
            .json(&body)
            .send()
            .await
            .map_err(|e| InferenceError::Transport(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(InferenceError::Api {
                status: status.as_u16(),
                body,
            });
        }

        // Read the full body first so the raw text can accompany a parse error.
        let text = resp
            .text()
            .await
            .map_err(|e| InferenceError::Transport(e.to_string()))?;
        serde_json::from_str::<ChatResponse>(&text).map_err(|e| InferenceError::Deserialise {
            message: e.to_string(),
            body: text,
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::registry::{ProviderId, capabilities};
    use crate::inference::types::ChatMessage;

    fn config_for(id: ProviderId, base_url: &str) -> OpenAiCompatConfig {
        OpenAiCompatConfig {
            name: id.as_str().to_string(),
            base_url: base_url.to_string(),
            api_key: SecretString::new("sk-test"), // pragma: allowlist secret
            extra_headers: Vec::new(),
            capabilities: *capabilities(id),
        }
    }

    /// Why: the endpoint must be `<base>/chat/completions` regardless of a
    /// trailing slash on the base URL.
    /// Test: itself.
    #[test]
    fn endpoint_appends_path() {
        let a =
            OpenAiCompatAdapter::new(config_for(ProviderId::OpenAI, "https://api.openai.com/v1"))
                .expect("build");
        assert_eq!(a.endpoint(), "https://api.openai.com/v1/chat/completions");
        let b =
            OpenAiCompatAdapter::new(config_for(ProviderId::OpenAI, "https://api.openai.com/v1/"))
                .expect("build");
        assert_eq!(b.endpoint(), "https://api.openai.com/v1/chat/completions");
    }

    /// Why: OpenRouter (detailed_usage_accounting = true) must inject
    /// `usage:{include:true}` so cost/cache accounting is returned.
    /// Test: itself.
    #[test]
    fn usage_directive_injected() {
        let a = OpenAiCompatAdapter::new(config_for(
            ProviderId::OpenRouter,
            "https://openrouter.ai/api/v1",
        ))
        .expect("build");
        let req = ChatRequest::new("openai/gpt-4o-mini", vec![ChatMessage::user("hi")]);
        let prepared = a.prepare_body(&req);
        assert_eq!(prepared.usage, Some(RequestUsageConfig::detailed()));
    }

    /// Why: providers without detailed-usage accounting (Fireworks/OpenAI) must
    /// never send the directive.
    /// Test: itself.
    #[test]
    fn usage_directive_absent_without_detailed() {
        for id in [ProviderId::Fireworks, ProviderId::OpenAI] {
            let a =
                OpenAiCompatAdapter::new(config_for(id, "https://example.test/v1")).expect("build");
            let req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
            assert!(
                a.prepare_body(&req).usage.is_none(),
                "{} must not inject usage",
                id.as_str()
            );
        }
    }

    /// Why: a caller that already set `usage` must have it preserved, not
    /// clobbered by the injection.
    /// Test: itself.
    #[test]
    fn usage_directive_not_overwritten() {
        let a = OpenAiCompatAdapter::new(config_for(
            ProviderId::OpenRouter,
            "https://openrouter.ai/api/v1",
        ))
        .expect("build");
        let mut req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        req.usage = Some(RequestUsageConfig { include: false });
        assert_eq!(
            a.prepare_body(&req).usage,
            Some(RequestUsageConfig { include: false })
        );
    }
}
