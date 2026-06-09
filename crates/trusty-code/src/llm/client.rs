//! HTTP client for the OpenRouter `/v1/chat/completions` endpoint.
//!
//! Why: trusty-code needs a native LLM client that speaks the OpenAI
//! chat-completions schema and works with any OpenRouter-hosted model slug —
//! without pulling in the `async-openai` crate (which adds dependency weight
//! and couples us to Anthropic's published API directly).
//! What: `LlmClient` wraps a `reqwest::Client`, holds the API key and optional
//! HTTP-Referer header, and exposes a single `chat` method that posts a
//! `ChatRequest` and returns a `ChatResponse`.  API-key injection happens at
//! construction time so library code never reads from `std::env` itself.
//! Test: Unit tests in `llm::types::tests` cover serialisation / deserialisation.
//! The ignore-gated integration test `live_openrouter_call` in this file sends a
//! real request (requires `OPENROUTER_API_KEY` in env).

use reqwest::Client;

use super::{error::LlmError, request::ChatRequest, response::ChatResponse};

/// Base URL for OpenRouter's chat-completions endpoint.
const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Configuration for `LlmClient`.
///
/// Why: Separating config from the client struct makes it easy to construct the
/// client from different sources (env, TOML config, tests) without duplicating
/// the validation logic.
/// What: Carries the API key (mandatory), an optional HTTP-Referer, and an
/// optional X-Title header forwarded to OpenRouter for analytics.
/// Test: `LlmClientConfig::from_env` unit-tested in `client_config_from_env`.
#[derive(Debug, Clone)]
pub struct LlmClientConfig {
    /// OpenRouter API key (Bearer token).
    pub api_key: String,

    /// Optional HTTP-Referer sent to OpenRouter for request attribution.
    pub http_referer: Option<String>,

    /// Optional X-Title header sent to OpenRouter (appears in dashboards).
    pub x_title: Option<String>,
}

impl LlmClientConfig {
    /// Construct from an explicit API key string.
    ///
    /// Why: Tests and callers that already hold the key don't need an env read.
    /// What: Validates that `api_key` is non-empty; returns `MissingConfig` if
    /// it is.
    /// Test: `client_config_direct_construction`.
    pub fn new(api_key: impl Into<String>) -> Result<Self, LlmError> {
        let api_key = api_key.into();
        if api_key.is_empty() {
            return Err(LlmError::MissingConfig("api_key must not be empty".into()));
        }
        Ok(Self {
            api_key,
            http_referer: None,
            x_title: None,
        })
    }

    /// Read the API key from the `OPENROUTER_API_KEY` environment variable.
    ///
    /// Why: Binary entry points call this once at startup; library helpers
    /// never read env variables directly (convention from CLAUDE.md).
    /// What: Reads `OPENROUTER_API_KEY`; returns `MissingConfig` if unset or
    /// empty.
    /// Test: `client_config_from_env_missing` (key absent → error).
    pub fn from_env() -> Result<Self, LlmError> {
        let key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
        Self::new(key)
    }

    /// Set the optional HTTP-Referer attribution header.
    ///
    /// Why: OpenRouter uses this for per-project analytics in their dashboard.
    /// What: Builder-style setter; returns `self` for chaining.
    /// Test: `client_config_builder`.
    pub fn with_referer(mut self, referer: impl Into<String>) -> Self {
        self.http_referer = Some(referer.into());
        self
    }

    /// Set the optional X-Title attribution header.
    ///
    /// Why: Appears in OpenRouter's request logs as a human-readable label.
    /// What: Builder-style setter; returns `self` for chaining.
    /// Test: `client_config_builder`.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.x_title = Some(title.into());
        self
    }
}

/// Async HTTP client for the OpenRouter chat-completions API.
///
/// Why: Centralises all HTTP mechanics (header injection, error handling,
/// response parsing) so call sites only deal with `ChatRequest` / `ChatResponse`.
/// What: Wraps a `reqwest::Client` and `LlmClientConfig`; exposes `chat` as the
/// sole public method.  The client is cheaply cloneable (`reqwest::Client` uses
/// an `Arc` internally).
/// Test: Serialisation/deserialisation unit tests live in `llm::types::tests`.
/// The `#[ignore]`-gated `live_openrouter_call` test below validates the full
/// HTTP round-trip.
#[derive(Debug, Clone)]
pub struct LlmClient {
    http: Client,
    config: LlmClientConfig,
}

impl LlmClient {
    /// Construct an `LlmClient` from an explicit config.
    ///
    /// Why: Allows dependency injection in tests and in callers that manage
    /// their own key source.
    /// What: Builds a `reqwest::Client` with rustls TLS; returns an error if
    /// the HTTP client cannot be constructed (extremely rare).
    /// Test: `lm_client_from_config`.
    pub fn from_config(config: LlmClientConfig) -> Result<Self, LlmError> {
        let http = Client::builder()
            .use_rustls_tls()
            .build()
            .map_err(LlmError::Transport)?;
        Ok(Self { http, config })
    }

    /// Construct from the `OPENROUTER_API_KEY` environment variable.
    ///
    /// Why: Convenience entry point for binary code that reads config from env.
    /// What: Delegates to `LlmClientConfig::from_env`, then `from_config`.
    /// Test: Integration test `live_openrouter_call` uses this path.
    pub fn from_env() -> Result<Self, LlmError> {
        Self::from_config(LlmClientConfig::from_env()?)
    }

    /// POST a `ChatRequest` to OpenRouter and return the parsed `ChatResponse`.
    ///
    /// Why: Single-method surface keeps call sites simple and makes mocking
    /// (for future test doubles) easy.
    /// What: Serialises `req` to JSON, adds required OpenRouter headers
    /// (`Authorization`, optional `HTTP-Referer`, optional `X-Title`), sends
    /// the POST, and deserialises the response body.  Non-2xx responses are
    /// mapped to `LlmError::ApiError` with the raw body included.
    /// Test: `live_openrouter_call` (#[ignore]) validates the happy path.
    /// Unit tests in `types.rs` cover the serialisation/deserialisation paths.
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let mut builder = self
            .http
            .post(OPENROUTER_BASE_URL)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json");

        if let Some(referer) = &self.config.http_referer {
            builder = builder.header("HTTP-Referer", referer.as_str());
        }
        if let Some(title) = &self.config.x_title {
            builder = builder.header("X-Title", title.as_str());
        }

        let resp = builder.json(req).send().await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::ApiError {
                status: status.as_u16(),
                body,
            });
        }

        // Read the full body first so we can include it in deserialisation errors.
        let body = resp.text().await?;
        serde_json::from_str::<ChatResponse>(&body)
            .map_err(|source| LlmError::Deserialise { source, body })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, ChatRequest};

    /// `LlmClientConfig::new` rejects an empty API key.
    ///
    /// Why: An empty key would produce a confusing 401 from the API; we want
    /// to fail at construction time with a clear error.
    /// What: Pass an empty string, assert `MissingConfig` error.
    /// Test: this test.
    #[test]
    fn client_config_rejects_empty_key() {
        let err = LlmClientConfig::new("").unwrap_err();
        assert!(
            matches!(err, LlmError::MissingConfig(_)),
            "expected MissingConfig, got: {err:?}"
        );
    }

    /// `LlmClientConfig::new` accepts a non-empty key.
    ///
    /// Why: Baseline happy-path for construction.
    /// What: Pass a fake key, assert `Ok`.
    /// Test: this test.
    #[test]
    fn client_config_direct_construction() {
        let cfg = LlmClientConfig::new("sk-or-test-key").expect("should succeed");
        assert_eq!(cfg.api_key, "sk-or-test-key");
        assert!(cfg.http_referer.is_none());
        assert!(cfg.x_title.is_none());
    }

    /// Builder methods chain correctly.
    ///
    /// Why: Verify the builder pattern sets optional fields.
    /// What: Chain `with_referer` and `with_title`, assert both are set.
    /// Test: this test.
    #[test]
    fn client_config_builder() {
        let cfg = LlmClientConfig::new("sk-or-x")
            .unwrap()
            .with_referer("https://example.com")
            .with_title("trusty-code");
        assert_eq!(cfg.http_referer.as_deref(), Some("https://example.com"));
        assert_eq!(cfg.x_title.as_deref(), Some("trusty-code"));
    }

    /// `LlmClientConfig::from_env` returns `MissingConfig` when the env var is absent.
    ///
    /// Why: Prevent silent failures when the key is not configured.
    /// What: Remove the env var, call `from_env`, assert error.
    /// Test: this test.
    #[test]
    fn client_config_from_env_missing() {
        // Temporarily unset the key (may already be absent in CI).
        let prev = std::env::var("OPENROUTER_API_KEY").ok();
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
        }
        let result = LlmClientConfig::from_env();
        // Restore before asserting (so other tests are not affected even if this
        // assertion panics).
        if let Some(k) = prev {
            unsafe { std::env::set_var("OPENROUTER_API_KEY", k) };
        }
        assert!(
            result.is_err(),
            "expected Err when OPENROUTER_API_KEY is unset"
        );
    }

    /// `LlmClient::from_config` succeeds with a valid config.
    ///
    /// Why: Verifies the reqwest client builds without error.
    /// What: Construct with a fake key, assert `Ok`.
    /// Test: this test.
    #[test]
    fn lm_client_from_config() {
        let cfg = LlmClientConfig::new("sk-or-test").unwrap();
        let client = LlmClient::from_config(cfg);
        assert!(client.is_ok(), "expected Ok, got: {client:?}");
    }

    /// Live integration test: send a trivial prompt to a cheap OpenRouter model.
    ///
    /// Why: End-to-end validation that `LlmClient::chat` produces a non-empty
    /// assistant response and non-zero `TokenUsage`.
    /// What: Read `OPENROUTER_API_KEY` from env; skip (not fail) if absent.
    /// POST a single-turn prompt to `openai/gpt-4o-mini`; assert non-empty
    /// `first_text()` and `usage.prompt_tokens > 0`.
    /// Test: Run with `cargo test -p trusty-code -- --include-ignored`.
    #[tokio::test]
    #[ignore = "requires OPENROUTER_API_KEY; skipped in CI"]
    async fn live_openrouter_call() {
        // Skip gracefully when the key is not present (e.g. in a dev env that
        // hasn't set it up yet) so the test doesn't block others.
        let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
            eprintln!("OPENROUTER_API_KEY not set — skipping live test");
            return;
        };
        if key.is_empty() {
            eprintln!("OPENROUTER_API_KEY is empty — skipping live test");
            return;
        }

        let config = LlmClientConfig::new(key)
            .unwrap()
            .with_referer("https://github.com/bobmatnyc/trusty-tools")
            .with_title("trusty-code-integration-test");

        let client = LlmClient::from_config(config).expect("build client");

        let req = ChatRequest {
            model: "openai/gpt-4o-mini".into(),
            messages: vec![
                ChatMessage::system("You are a concise assistant."),
                ChatMessage::user("Reply with exactly the word: pong"),
            ],
            temperature: Some(0.0),
            max_tokens: Some(16),
            tools: None,
            tool_choice: None,
        };

        let resp = client.chat(&req).await.expect("chat call succeeded");

        // Assert assistant text is non-empty.
        let text = resp.first_text().expect("assistant produced text");
        assert!(!text.is_empty(), "assistant text was empty");

        // Assert usage is non-zero.
        let usage = resp.token_usage();
        assert!(usage.prompt_tokens > 0, "prompt_tokens should be > 0");
        assert!(
            usage.completion_tokens > 0,
            "completion_tokens should be > 0"
        );

        eprintln!("live test passed — text: {text:?}, usage: {usage:?}");
    }
}
