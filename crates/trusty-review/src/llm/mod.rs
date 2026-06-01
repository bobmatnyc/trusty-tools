//! LLM provider abstraction for trusty-review.
//!
//! Why: the pipeline and diff summarizer must call the LLM through a trait
//! so the provider (OpenRouter vs Bedrock) is an implementation detail
//! invisible to the caller, and so tests can inject mocks.
//! What: defines the `LlmProvider` trait (single non-streaming `complete`
//! call), `LlmRequest` / `LlmResponse` data shapes, the `build_provider`
//! factory, and re-exports the `LlmError` enum, `OpenRouterProvider`,
//! `BedrockProvider`, and `models` constants.
//! Test: each submodule carries its own unit tests; this module's
//! `provider_trait_object_compiles` smoke-tests that the trait is
//! object-safe; `provider_factory_*` tests cover the routing logic.

pub mod bedrock;
pub mod error;
pub mod models;
pub mod openrouter;

pub use bedrock::BedrockProvider;
pub use error::LlmError;
pub use openrouter::OpenRouterProvider;

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::Provider;

// ─── Chat message (simplified, no tool-use for review calls) ─────────────────

/// A single chat message for an LLM completion request.
///
/// Why: the review pipeline sends system + user messages; we do not need
/// the full `trusty_common::ChatMessage` tool-call fields for review calls.
/// What: a minimal role + content pair.
/// Test: covered transitively by `LlmRequest` tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Role: `"system"`, `"user"`, or `"assistant"`.
    pub role: String,
    /// Content text.
    pub content: String,
}

// ─── Request / response types ─────────────────────────────────────────────────

/// Input to `LlmProvider::complete`.
///
/// Why: carries all per-call parameters so each role (reviewer, verifier,
/// summarizer) can vary temperature and max_tokens independently.
/// What: `model` is the fully-resolved model id for this call (set by
/// `RoleModels`); `system` is the system prompt; `messages` are the user
/// turns.
/// Test: constructed in `LlmProvider` implementation tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    /// Fully-resolved model id for this call.
    pub model: String,
    /// System prompt (empty string = no system message).
    pub system: String,
    /// User conversation turns.
    pub messages: Vec<ChatMessage>,
    /// Sampling temperature (0.0–2.0).
    pub temperature: f32,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
}

/// Output from `LlmProvider::complete`.
///
/// Why: captures the response text plus all telemetry fields required by
/// spec REV-330 and the Stage-3 `compare` mode (speed, cost, tokens).
/// What: `text` is the full response; `model` echoes the actual model id
/// used (providers may normalise or alias the requested id); `input_tokens` /
/// `output_tokens` come from the API usage field; `latency_ms` is wall-clock
/// end-to-end; `cost_usd` is an estimate from a pricing table.
/// Test: `llm_response_serde_roundtrip` in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    /// Full response text.
    pub text: String,
    /// Model id actually used by the provider (echoed from the response).
    pub model: String,
    /// Number of input (prompt) tokens consumed.
    pub input_tokens: u32,
    /// Number of output (completion) tokens generated.
    pub output_tokens: u32,
    /// Wall-clock latency in milliseconds from request send to last byte received.
    pub latency_ms: u64,
    /// Estimated USD cost for this call based on the model pricing table.
    pub cost_usd: f64,
}

// ─── Provider trait ───────────────────────────────────────────────────────────

/// LLM completion provider.
///
/// Why: the pipeline and tests depend on this trait rather than concrete
/// types so the provider can be swapped without touching pipeline code
/// (OpenRouter for local dev, Bedrock for production).
/// What: a single `complete` method that takes `LlmRequest` and returns
/// `LlmResponse` or `LlmError`.  Implementors are `Send + Sync` so they
/// can be held behind `Arc<dyn LlmProvider>`.
/// Test: `provider_trait_object_compiles` verifies the trait is object-safe.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Human-readable provider name, e.g. `"openrouter"` or `"bedrock"`.
    ///
    /// Why: logs and metrics tag calls with the provider name.
    /// What: a static string slice; no heap allocation.
    /// Test: each implementation asserts its own name.
    fn name(&self) -> &str;

    /// Execute a non-streaming completion and return the full response.
    ///
    /// Why: the pipeline needs the full text, token counts, latency, and cost
    /// in one call — streaming complicates token-count capture.
    /// What: sends `req` to the upstream API, waits for the full response,
    /// and returns `LlmResponse`.  Transient errors may be retried by the
    /// caller based on `LlmError::is_retryable`.
    /// Test: each provider implements mock-server tests.
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError>;
}

// ─── Provider factory ─────────────────────────────────────────────────────────

/// Routing prefixes for model-id based provider selection.
///
/// Why: mirrors the `bedrock/` prefix convention from trusty-analyze so
/// operators can paste model ids from the CLAUDE.md examples without needing
/// to know which provider a model belongs to.
/// What: the model id is stripped of this prefix before being passed to the
/// provider constructor.
pub const BEDROCK_MODEL_PREFIX: &str = "bedrock/";
/// OpenRouter model-id prefix for explicit routing.
pub const OPENROUTER_MODEL_PREFIX: &str = "openrouter/";

/// Strip the provider routing prefix from a model id, returning the bare id.
///
/// Why: `LlmRequest.model` must be the bare id sent to the upstream API — the
/// `bedrock/` and `openrouter/` prefixes are routing hints only and are never
/// valid API model ids.  Using the prefixed string causes Bedrock to receive
/// `bedrock/us.anthropic.claude-sonnet-4-6` as the Converse model id, which
/// produces `POST /model/bedrock%2F.../converse` → HTTP 400 ValidationException.
/// What: strips `bedrock/` or `openrouter/` prefix if present; returns the
/// remaining string.  Bare ids (no prefix) are returned unchanged.
/// Test: `prefix_stripped_model_id_bedrock`, `prefix_stripped_model_id_openrouter`,
/// `prefix_stripped_model_id_bare`.
pub fn strip_provider_prefix(model: &str) -> &str {
    if let Some(bare) = model.strip_prefix(BEDROCK_MODEL_PREFIX) {
        return bare;
    }
    if let Some(bare) = model.strip_prefix(OPENROUTER_MODEL_PREFIX) {
        return bare;
    }
    model
}

/// Resolve the effective provider and bare model id from a potentially-prefixed
/// model id string and an explicit provider hint.
///
/// Why: `--reviewer-model bedrock/us.anthropic.claude-sonnet-4-6` must route to
/// Bedrock regardless of the default provider; a bare `us.anthropic.*` id should
/// use the default provider; `openrouter/openai/gpt-5.4-mini` must route to
/// OpenRouter.  This function is the single source of truth for that logic.
/// What: strips known prefixes and returns `(Provider, bare_model_id)`.
/// Precedence: explicit prefix in model id > `default_provider` argument.
/// Test: `provider_factory_prefix_routing`.
pub fn resolve_provider_and_model(model: &str, default_provider: &Provider) -> (Provider, String) {
    if let Some(bare) = model.strip_prefix(BEDROCK_MODEL_PREFIX) {
        return (Provider::Bedrock, bare.to_string());
    }
    if let Some(bare) = model.strip_prefix(OPENROUTER_MODEL_PREFIX) {
        return (Provider::OpenRouter, bare.to_string());
    }
    (default_provider.clone(), model.to_string())
}

/// Build an `Arc<dyn LlmProvider>` from the resolved role config.
///
/// Why: the CLI, HTTP server, and pipeline all need to construct a provider
/// from the same config fields (provider, model, API key); centralising this
/// avoids duplicating the `bedrock/` prefix check in every call site.
/// What: resolves the effective provider via `resolve_provider_and_model`,
/// constructs the appropriate concrete type, and returns it as a trait object.
/// Returns `LlmError::AccessDenied` if OpenRouter is selected but `api_key`
/// is empty.  Returns `LlmError::Validation` if Bedrock is selected but the
/// model id is missing an inference-profile prefix.
/// Test: `provider_factory_builds_bedrock`, `provider_factory_builds_openrouter`,
/// `provider_factory_prefix_routing`.
pub async fn build_provider(
    model: &str,
    default_provider: &Provider,
    openrouter_api_key: &str,
) -> Result<Arc<dyn LlmProvider>, LlmError> {
    let (provider, bare_model) = resolve_provider_and_model(model, default_provider);
    match provider {
        Provider::Bedrock => {
            let p = BedrockProvider::new(bare_model, None).await?;
            Ok(Arc::new(p))
        }
        Provider::OpenRouter => {
            let p = OpenRouterProvider::new(openrouter_api_key, bare_model)?;
            Ok(Arc::new(p))
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_response_serde_roundtrip() {
        let resp = LlmResponse {
            text: "LGTM".to_string(),
            model: "openai/gpt-5.4-mini-20260317".to_string(),
            input_tokens: 512,
            output_tokens: 64,
            latency_ms: 1234,
            cost_usd: 0.000123,
        };
        let json = serde_json::to_string(&resp).expect("serialise");
        let back: LlmResponse = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.text, "LGTM");
        assert_eq!(back.model, "openai/gpt-5.4-mini-20260317");
        assert_eq!(back.input_tokens, 512);
        assert_eq!(back.output_tokens, 64);
        assert_eq!(back.latency_ms, 1234);
        assert!((back.cost_usd - 0.000123_f64).abs() < 1e-15);
    }

    #[test]
    fn llm_request_serde_roundtrip() {
        let req = LlmRequest {
            model: "openai/gpt-5.4-20260305".to_string(),
            system: "You are a reviewer.".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "Review this.".to_string(),
            }],
            temperature: 0.3,
            max_tokens: 2048,
        };
        let json = serde_json::to_string(&req).expect("serialise");
        let back: LlmRequest = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.model, "openai/gpt-5.4-20260305");
        assert_eq!(back.messages.len(), 1);
        assert!((back.temperature - 0.3_f32).abs() < f32::EPSILON);
    }

    /// Object-safety smoke-test: ensures `LlmProvider` can be used as a
    /// `dyn` trait object.
    #[test]
    fn provider_trait_object_compiles() {
        // This test just needs to compile; the type coercion proves
        // LlmProvider is object-safe.
        fn _accepts_dyn(_p: &dyn LlmProvider) {}
    }

    // ── Provider factory routing tests ────────────────────────────────────

    #[test]
    fn provider_factory_prefix_routing() {
        // bedrock/ prefix forces Bedrock.
        let (prov, model) = resolve_provider_and_model(
            "bedrock/us.anthropic.claude-sonnet-4-6",
            &Provider::OpenRouter,
        );
        assert_eq!(prov, Provider::Bedrock);
        assert_eq!(model, "us.anthropic.claude-sonnet-4-6");

        // openrouter/ prefix forces OpenRouter.
        let (prov, model) = resolve_provider_and_model(
            "openrouter/openai/gpt-5.4-mini-20260317",
            &Provider::Bedrock,
        );
        assert_eq!(prov, Provider::OpenRouter);
        assert_eq!(model, "openai/gpt-5.4-mini-20260317");

        // Bare id uses the default provider.
        let (prov, model) =
            resolve_provider_and_model("us.anthropic.claude-sonnet-4-6", &Provider::Bedrock);
        assert_eq!(prov, Provider::Bedrock);
        assert_eq!(model, "us.anthropic.claude-sonnet-4-6");

        // Bare OpenRouter id with Bedrock default stays on Bedrock.
        let (prov, model) =
            resolve_provider_and_model("openai/gpt-5.4-mini-20260317", &Provider::Bedrock);
        assert_eq!(prov, Provider::Bedrock);
        assert_eq!(model, "openai/gpt-5.4-mini-20260317");
    }

    #[test]
    fn provider_factory_empty_bedrock_prefix_uses_bare_empty_string() {
        // Edge case: "bedrock/" with nothing after the slash.
        let (prov, model) = resolve_provider_and_model("bedrock/", &Provider::OpenRouter);
        assert_eq!(prov, Provider::Bedrock);
        assert_eq!(model, "");
    }

    #[test]
    fn provider_factory_mixed_providers_in_compare_set() {
        // Simulate the mixed-provider compare set.
        let candidates = [
            "bedrock/us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "bedrock/us.anthropic.claude-sonnet-4-6",
            "openrouter/openai/gpt-5.4-mini-20260317",
        ];
        let expected = [
            (
                Provider::Bedrock,
                "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            ),
            (Provider::Bedrock, "us.anthropic.claude-sonnet-4-6"),
            (Provider::OpenRouter, "openai/gpt-5.4-mini-20260317"),
        ];
        for (candidate, (exp_prov, exp_model)) in candidates.iter().zip(expected.iter()) {
            let (prov, model) = resolve_provider_and_model(candidate, &Provider::Bedrock);
            assert_eq!(prov, *exp_prov, "provider mismatch for {candidate}");
            assert_eq!(model, *exp_model, "model mismatch for {candidate}");
        }
    }

    // ── strip_provider_prefix regression tests ────────────────────────────

    /// Regression test: `bedrock/<id>` must strip to bare `<id>`.
    ///
    /// Why: guards against the Bug 1 regression where a prefixed model id
    /// reaches the Bedrock Converse API as the model parameter, causing HTTP 400.
    /// What: asserts `strip_provider_prefix("bedrock/X") == "X"` for real ids.
    /// Test: this test itself.
    #[test]
    fn prefix_stripped_model_id_bedrock() {
        assert_eq!(
            strip_provider_prefix("bedrock/us.anthropic.claude-sonnet-4-6"),
            "us.anthropic.claude-sonnet-4-6",
            "bedrock/ prefix must be stripped"
        );
        assert_eq!(
            strip_provider_prefix("bedrock/us.anthropic.claude-haiku-4-5-20251001-v1:0"),
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "bedrock/ prefix must be stripped from date-versioned haiku id"
        );
        assert_eq!(
            strip_provider_prefix("bedrock/us.anthropic.claude-opus-4-8"),
            "us.anthropic.claude-opus-4-8",
            "bedrock/ prefix must be stripped from opus id"
        );
    }

    /// Regression test: `openrouter/<id>` must strip to bare `<id>`.
    ///
    /// Why: same Bug 1 pattern applies to OpenRouter provider routing prefix.
    /// What: asserts `strip_provider_prefix("openrouter/X") == "X"`.
    /// Test: this test itself.
    #[test]
    fn prefix_stripped_model_id_openrouter() {
        assert_eq!(
            strip_provider_prefix("openrouter/openai/gpt-5.4-mini-20260317"),
            "openai/gpt-5.4-mini-20260317",
            "openrouter/ prefix must be stripped"
        );
    }

    /// Regression test: bare ids (no prefix) must be returned unchanged.
    ///
    /// Why: operators may pass bare ids when the provider is set separately;
    /// stripping must not mangle them.
    /// What: asserts `strip_provider_prefix("us.X") == "us.X"`.
    /// Test: this test itself.
    #[test]
    fn prefix_stripped_model_id_bare() {
        assert_eq!(
            strip_provider_prefix("us.anthropic.claude-sonnet-4-6"),
            "us.anthropic.claude-sonnet-4-6",
            "bare id must be returned unchanged"
        );
        assert_eq!(
            strip_provider_prefix("openai/gpt-5.4-mini-20260317"),
            "openai/gpt-5.4-mini-20260317",
            "bare OpenRouter id must not be stripped"
        );
    }
}
