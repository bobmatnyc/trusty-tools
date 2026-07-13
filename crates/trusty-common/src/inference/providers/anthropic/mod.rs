//! Anthropic-direct adapter — the native Messages-API inference backend
//! (issue #2408, epic #2400 Wave 2).
//!
//! Why: Anthropic's first-party API speaks its OWN wire format (`/v1/messages`
//! with `x-api-key` + `anthropic-version` headers, a top-level `system` param,
//! `content` blocks, `cache_control` breakpoints, and `input_tokens`-shaped
//! usage) — NOT the OpenAI `/chat/completions` schema the shared
//! [`super::openai_compat`] core speaks. Routing it through that core would drop
//! the system prompt, mis-shape tools, and report zero usage. So Anthropic-direct
//! needs a dedicated adapter with its own request/response translation (in the
//! [`request`]/[`response`] submodules) rather than a thin config over the
//! OpenAI-compat core.
//! What: [`AnthropicAdapter`] (the [`InferenceAdapter`] impl owning the reqwest
//! POST with Anthropic auth/version headers), [`build`]/[`factory`] (the
//! configurator construction path), and the [`ANTHROPIC_BASE_URL`]/
//! [`ANTHROPIC_VERSION`] constants. The neutral↔Anthropic translation lives in
//! the two submodules; this file owns transport + trait wiring.
//! Test: inline `tests` — `factory_builds_named_adapter`, `missing_key_errors`,
//! `endpoint_appends_messages_path`, `map_tool_choice_uses_anthropic_dialect`,
//! and the `#[ignore]` `live_anthropic_call`; offline HTTP round-trip in
//! `crates/trusty-common/tests/inference_adapters.rs`.

pub mod request;
pub mod response;

pub use request::anthropic_tool_choice;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

use crate::inference::adapter::InferenceAdapter;
use crate::inference::configurator::ResolvedProvider;
use crate::inference::error::InferenceError;
use crate::inference::registry::{ProviderCapabilities, ProviderId};
use crate::inference::types::{ChatRequest, ChatResponse, SecretString, ToolChoice};

/// Anthropic API root; the adapter appends `/messages`.
pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";

/// The `anthropic-version` header value (the current stable Messages API date).
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Fallback `max_tokens` when the caller left it unset — Anthropic REQUIRES the
/// field, so an adapter that forwarded `None` would 400 on every call.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// The native Anthropic Messages-API inference adapter.
///
/// Why: Anthropic's wire format has no OpenAI-compatible equivalent, so it owns a
/// bespoke adapter rather than a thin config over the shared core — the request
/// and response translation are handled by the [`request`]/[`response`]
/// submodules, and this struct owns only the HTTP transport (auth via `x-api-key`,
/// not `Authorization: Bearer`).
/// What: wraps a cloneable `reqwest::Client`, the precomputed `/messages`
/// endpoint, the redacted key, and the capability descriptor. The API key is held
/// as a [`SecretString`] and only ever placed in the `x-api-key` header.
/// Test: inline `tests`; offline round-trip in
/// `crates/trusty-common/tests/inference_adapters.rs`.
pub struct AnthropicAdapter {
    endpoint: String,
    api_key: SecretString,
    capabilities: ProviderCapabilities,
    http: Client,
}

impl AnthropicAdapter {
    /// Build an adapter for a resolved credential against `base_url`.
    ///
    /// Why: the single construction path the factory funnels through; `base_url`
    /// is a parameter so tests can point the same adapter at
    /// [`crate::inference::test_support::MockInferenceServer`].
    /// What: precomputes the `/messages` endpoint (tolerating a trailing slash),
    /// builds a rustls `reqwest::Client`, and stores the config. A client-build
    /// failure maps to [`InferenceError::Transport`] rather than panicking.
    /// Test: `endpoint_appends_messages_path`.
    pub fn new(
        base_url: &str,
        api_key: SecretString,
        capabilities: ProviderCapabilities,
    ) -> Result<Self, InferenceError> {
        let endpoint = format!("{}/messages", base_url.trim_end_matches('/'));
        let http = Client::builder()
            .use_rustls_tls()
            .build()
            .map_err(|e| InferenceError::Transport(e.to_string()))?;
        Ok(Self {
            endpoint,
            api_key,
            capabilities,
            http,
        })
    }

    /// The resolved `/messages` endpoint this adapter POSTs to.
    ///
    /// Why: exposed for diagnostics and the inline endpoint-construction test.
    /// What: the precomputed absolute URL.
    /// Test: `endpoint_appends_messages_path`.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

/// Build an Anthropic-direct adapter for a resolved credential against `base_url`.
///
/// Why: the base URL is a parameter (not a constant) so tests point the exact
/// same adapter at the mock server while production uses [`ANTHROPIC_BASE_URL`].
/// What: requires the resolved key (Anthropic is a keyed provider — a missing key
/// is [`InferenceError::MissingCredential`]), then constructs an
/// [`AnthropicAdapter`] with Anthropic's registry capabilities.
/// Test: `factory_builds_named_adapter`, `missing_key_errors`;
/// `crates/trusty-common/tests/inference_adapters.rs`.
pub fn build(
    resolved: &ResolvedProvider,
    base_url: &str,
) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    let key = resolved
        .key()
        .ok_or(InferenceError::MissingCredential {
            provider: ProviderId::Anthropic,
        })?
        .clone();
    Ok(Box::new(AnthropicAdapter::new(
        base_url,
        key,
        *resolved.capabilities(),
    )?))
}

/// Production factory: build an Anthropic adapter against the real base URL.
///
/// Why: registered by [`super::register_default_factories`] so an `anthropic/*`
/// slug (whose key resolves) yields a live first-party Anthropic adapter.
/// What: delegates to [`build`] with [`ANTHROPIC_BASE_URL`].
/// Test: `crates/trusty-common/tests/inference_adapters.rs` (via a mock-URL
/// factory) and the `#[ignore]` `live_anthropic_call` below.
pub fn factory(resolved: &ResolvedProvider) -> Result<Box<dyn InferenceAdapter>, InferenceError> {
    build(resolved, ANTHROPIC_BASE_URL)
}

#[async_trait]
impl InferenceAdapter for AnthropicAdapter {
    /// The stable provider name (`"anthropic"`).
    fn name(&self) -> &str {
        ProviderId::Anthropic.as_str()
    }

    /// The provider's capability descriptor.
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    /// Translate a neutral [`ToolChoice`] into the Anthropic dialect.
    ///
    /// Why: Anthropic spells tool-choice differently from OpenAI (`any` forces a
    /// call; a specific call is `{type:"tool",name}`); overriding the default
    /// keeps callers dialect-agnostic.
    /// What: delegates to [`anthropic_tool_choice`].
    /// Test: `map_tool_choice_uses_anthropic_dialect`.
    fn map_tool_choice(&self, choice: ToolChoice) -> Value {
        anthropic_tool_choice(choice)
    }

    /// POST a chat request to the Anthropic Messages API and translate the reply.
    ///
    /// Why: the one method the agent loop depends on; all Anthropic-specific HTTP
    /// mechanics (auth via `x-api-key`, the `anthropic-version` header, status
    /// classification, native response parsing) live here so callers only speak
    /// [`ChatRequest`]/[`ChatResponse`].
    /// What: translates the request via [`request::build_body`], POSTs it with the
    /// `x-api-key` + `anthropic-version` headers, and maps the outcome — a
    /// transport failure to [`InferenceError::Transport`], a non-2xx status to
    /// [`InferenceError::Api`], and the success body through [`response::parse`].
    /// The API key is only ever placed in the `x-api-key` header and never appears
    /// in a returned error.
    /// Test: `crates/trusty-common/tests/inference_adapters.rs`.
    async fn chat(&self, request: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let body = request::build_body(request, DEFAULT_MAX_TOKENS);

        // `reqwest::Error` display carries the URL/kind but never a header value,
        // so stringifying it cannot leak the API key.
        let resp = self
            .http
            .post(&self.endpoint)
            .header("x-api-key", self.api_key.expose())
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
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

        let text = resp
            .text()
            .await
            .map_err(|e| InferenceError::Transport(e.to_string()))?;
        response::parse(&text)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::types::{ChatMessage, ChatRequest, SecretString};

    fn resolved(key: &str) -> ResolvedProvider {
        ResolvedProvider::new(
            ProviderId::Anthropic,
            "anthropic/claude-sonnet-4-5".to_string(),
            Some(SecretString::new(key)),
        )
    }

    /// Why: the factory must build a named Anthropic adapter carrying Anthropic's
    /// capabilities (prompt caching supported, no detailed-usage directive).
    /// Test: itself.
    #[test]
    fn factory_builds_named_adapter() {
        let adapter = build(&resolved("sk-ant-test"), ANTHROPIC_BASE_URL) // pragma: allowlist secret
            .expect("built");
        assert_eq!(adapter.name(), "anthropic");
        assert!(adapter.supports_prompt_caching());
        assert!(!adapter.wants_detailed_usage());
        assert_eq!(adapter.capabilities().id, ProviderId::Anthropic);
    }

    /// Why: a resolved provider with no key must be an explicit alarm, never a
    /// silent anonymous call.
    /// Test: itself.
    #[test]
    fn missing_key_errors() {
        let resolved = ResolvedProvider::new(
            ProviderId::Anthropic,
            "anthropic/claude-sonnet-4-5".to_string(),
            None,
        );
        let Err(err) = build(&resolved, ANTHROPIC_BASE_URL) else {
            panic!("expected MissingCredential");
        };
        assert!(matches!(
            err,
            InferenceError::MissingCredential {
                provider: ProviderId::Anthropic
            }
        ));
    }

    /// Why: the endpoint must be `<base>/messages` regardless of a trailing slash.
    /// Test: itself.
    #[test]
    fn endpoint_appends_messages_path() {
        let caps = *crate::inference::registry::capabilities(ProviderId::Anthropic);
        let a = AnthropicAdapter::new(
            "https://api.anthropic.com/v1",
            SecretString::new("sk-ant-test"), // pragma: allowlist secret
            caps,
        )
        .expect("build");
        assert_eq!(a.endpoint(), "https://api.anthropic.com/v1/messages");
        let b = AnthropicAdapter::new(
            "https://api.anthropic.com/v1/",
            SecretString::new("sk-ant-test"), // pragma: allowlist secret
            caps,
        )
        .expect("build");
        assert_eq!(b.endpoint(), "https://api.anthropic.com/v1/messages");
    }

    /// Why: the adapter must map tool-choice through the Anthropic dialect
    /// (`Required`→`{type:any}`), not the OpenAI default (`"required"`).
    /// Test: itself.
    #[test]
    fn map_tool_choice_uses_anthropic_dialect() {
        let adapter = build(&resolved("sk-ant-test"), ANTHROPIC_BASE_URL).expect("built"); // pragma: allowlist secret
        assert_eq!(
            adapter.map_tool_choice(ToolChoice::Required),
            serde_json::json!({"type": "any"})
        );
        assert_eq!(
            adapter.map_tool_choice(ToolChoice::Function("f".into())),
            serde_json::json!({"type": "tool", "name": "f"})
        );
    }

    /// Live smoke test: send a trivial prompt to a cheap Anthropic model.
    ///
    /// Why: end-to-end validation against the real API that the Anthropic request
    /// build + response parse produce a non-empty reply and non-zero usage.
    /// Ignored so CI stays offline; run locally with a real key.
    /// What: reads `ANTHROPIC_API_KEY` from env and SKIPS (does not fail) when
    /// absent/empty; otherwise builds the adapter via [`build`] and asserts a
    /// non-empty reply with `prompt_tokens > 0`.
    /// Test: `cargo test -p trusty-common --features inference-client,axum-server \
    ///        anthropic -- --ignored --nocapture` (with `ANTHROPIC_API_KEY` set).
    #[tokio::test]
    #[ignore = "requires ANTHROPIC_API_KEY; skipped in CI"]
    async fn live_anthropic_call() {
        let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
            eprintln!("ANTHROPIC_API_KEY not set — skipping live test");
            return;
        };
        if key.trim().is_empty() {
            eprintln!("ANTHROPIC_API_KEY is empty — skipping live test");
            return;
        }

        let resolved = ResolvedProvider::new(
            ProviderId::Anthropic,
            "anthropic/claude-3-5-haiku-latest".to_string(),
            Some(SecretString::new(key)),
        );
        let adapter = build(&resolved, ANTHROPIC_BASE_URL).expect("build adapter");

        let mut req = ChatRequest::new(
            "claude-3-5-haiku-latest",
            vec![
                ChatMessage::system("You are a concise assistant."),
                ChatMessage::user("Reply with exactly the word: pong"),
            ],
        );
        req.temperature = Some(0.0);
        req.max_tokens = Some(16);

        let resp = adapter.chat(&req).await.expect("live chat");
        let text = resp.first_text().expect("assistant text");
        assert!(!text.is_empty(), "assistant text was empty");
        assert!(
            resp.usage().prompt_tokens > 0,
            "prompt_tokens should be > 0"
        );
        eprintln!(
            "live anthropic ok — text: {text:?}, usage: {:?}",
            resp.usage()
        );
    }
}
