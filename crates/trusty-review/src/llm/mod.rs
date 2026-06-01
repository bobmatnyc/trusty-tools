//! LLM provider abstraction for trusty-review.
//!
//! Why: the pipeline and diff summarizer must call the LLM through a trait
//! so the provider (OpenRouter vs Bedrock) is an implementation detail
//! invisible to the caller, and so tests can inject mocks.
//! What: defines the `LlmProvider` trait (single non-streaming `complete`
//! call), `LlmRequest` / `LlmResponse` data shapes, and re-exports the
//! `LlmError` enum, the `OpenRouterProvider` implementation, and the
//! `models` constants.
//! Test: each submodule carries its own unit tests; this module's
//! `provider_trait_object_compiles` smoke-tests that the trait is
//! object-safe.

pub mod error;
pub mod models;
pub mod openrouter;

pub use error::LlmError;
pub use openrouter::OpenRouterProvider;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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
}
