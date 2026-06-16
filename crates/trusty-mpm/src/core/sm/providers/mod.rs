//! Vendored multi-provider LLM abstraction for the Session Manager (DOC-14 §5).
//!
//! Why: the SM is multi-provider (OpenRouter / AWS Bedrock / direct Anthropic,
//! §5.1) and selects a model per task (orchestration vs. summarization vs.
//! compaction, §5.4). It needs a single, prefix-routed, non-streaming,
//! cost/latency-aware `complete()`-style provider trait — the request/response
//! shape (not token-streamed to a UI) that trusty-review's `llm` module already
//! offers. Per the SM-2 PM decision we VENDOR a focused copy of that abstraction
//! here rather than extracting trusty-review's module into a shared crate now
//! (the cross-crate extraction is too high blast-radius for SM-2).
//!
//! TODO(follow-up #1302): extract this shared `LlmProvider` abstraction into a
//! common crate (`trusty-common::llm` or a new `trusty-llm`) so trusty-review
//! and the SM consume one copy instead of two. SM-2 deliberately vendored this
//! copy (DOC-14 §5.2 D5.1, §13 open-question 1); #1302 tracks the dedup.
//!
//! What: defines the [`LlmProvider`] trait (one non-streaming `complete` call
//! returning text + token usage + estimated cost), the [`LlmRequest`] /
//! [`LlmResponse`] / [`ChatMessage`] data shapes, the [`ProviderKind`] enum
//! (parsed from the SM config `provider` string with validation), and the
//! prefix-routing [`resolve`] submodule that maps a (possibly-prefixed) model
//! id + SM config into a concrete provider + bare model id + per-task tiers.
//! The three concrete providers live in `openrouter`, `anthropic`, and (behind
//! the `bedrock` cargo feature) `bedrock`.
//! Test: each submodule carries unit tests; `provider_kind_*` and the
//! `resolve::tests` module cover routing, tiers, alias-fallback, precedence,
//! degraded mode, and provider validation.

pub mod anthropic;
pub mod error;
pub mod openrouter;
pub mod pricing;
pub mod resolve;

#[cfg(feature = "bedrock")]
pub mod bedrock;

#[cfg(test)]
pub(crate) mod test_support;

pub use anthropic::AnthropicProvider;
pub use error::SmLlmError;
pub use openrouter::OpenRouterProvider;
pub use resolve::{
    ProviderRegistry, ResolvedCall, SmModelTier, TierResolver, resolve_provider_and_model,
    resolve_tier_model,
};

#[cfg(feature = "bedrock")]
pub use bedrock::BedrockProvider;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ─── Provider-id prefixes (DOC-14 §5.2 D5.3) ───────────────────────────────────

/// `anthropic/` model-id prefix: pin the direct Anthropic provider for a call.
pub const ANTHROPIC_MODEL_PREFIX: &str = "anthropic/";
/// `bedrock/` model-id prefix: pin the AWS Bedrock provider for a call.
pub const BEDROCK_MODEL_PREFIX: &str = "bedrock/";
/// `openrouter/` model-id prefix: pin the OpenRouter provider for a call.
pub const OPENROUTER_MODEL_PREFIX: &str = "openrouter/";

// ─── Provider kind (validated `provider` config value) ─────────────────────────

/// The four legal values of `[session_manager.inference].provider` (§5.3).
///
/// Why: SM-1 stored `provider` as a free-form `String`; SM-1 review flagged
/// that a bare/unknown value must be *rejected as a config error* rather than
/// silently failing at the first inference call. Parsing into this enum at
/// resolution time gives that ergonomic, early validation.
/// What: `Auto` triggers the precedence chain (§5.3); the other three pin a
/// single default provider. [`ProviderKind::parse`] normalises case and
/// trims, and returns [`SmLlmError::Validation`] for anything else.
/// Test: `provider_kind_parse_ok`, `provider_kind_parse_rejects_unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// `provider = "auto"` — pick the first provider with valid credentials
    /// (Anthropic → Bedrock → OpenRouter → degraded).
    Auto,
    /// `provider = "anthropic"` — direct `api.anthropic.com` `/v1/messages`.
    Anthropic,
    /// `provider = "bedrock"` — AWS Bedrock Converse.
    Bedrock,
    /// `provider = "openrouter"` — OpenRouter OpenAI-compatible endpoint.
    OpenRouter,
}

impl ProviderKind {
    /// Parse a `provider` config string into a validated [`ProviderKind`].
    ///
    /// Why: reject unknown provider strings at the config boundary (SM-1 review
    /// carry-forward) so operators get a clear error instead of a late,
    /// confusing inference failure.
    /// What: trims + lowercases `s`, then matches `auto`/`anthropic`/`bedrock`/
    /// `openrouter`. An empty string is treated as `auto` (the §10 default).
    /// Anything else returns [`SmLlmError::Validation`].
    /// Test: `provider_kind_parse_ok`, `provider_kind_parse_rejects_unknown`.
    pub fn parse(s: &str) -> Result<Self, SmLlmError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(ProviderKind::Auto),
            "anthropic" => Ok(ProviderKind::Anthropic),
            "bedrock" => Ok(ProviderKind::Bedrock),
            "openrouter" => Ok(ProviderKind::OpenRouter),
            other => Err(SmLlmError::Validation(format!(
                "unknown [session_manager.inference] provider {other:?}; \
                 expected one of: auto, anthropic, bedrock, openrouter"
            ))),
        }
    }

    /// The stable lowercase name of this provider kind.
    ///
    /// Why: `sm.health` (SM-STDIO) reports which concrete provider resolved for
    /// the orchestration tier; a stable string keeps the wire payload and any
    /// operator-facing status consistent with the `provider` config values.
    /// What: returns `"auto"`/`"anthropic"`/`"bedrock"`/`"openrouter"`.
    /// Test: `provider_kind_name_round_trips` (this module's tests).
    pub fn name(self) -> &'static str {
        match self {
            ProviderKind::Auto => "auto",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Bedrock => "bedrock",
            ProviderKind::OpenRouter => "openrouter",
        }
    }
}

// ─── Chat / request / response shapes ──────────────────────────────────────────

/// A single chat message for an SM completion request.
///
/// Why: the SM sends system + user/assistant turns; it does not need the full
/// tool-call shape for its reasoning calls (§5.5).
/// What: a minimal `role` (`"system"` | `"user"` | `"assistant"`) + `content`.
/// Test: covered transitively by provider round-trip tests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    /// Role: `"system"`, `"user"`, or `"assistant"`.
    pub role: String,
    /// Message text content.
    pub content: String,
}

/// Input to [`LlmProvider::complete`].
///
/// Why: carries every per-call parameter so each task tier (orchestration,
/// summarization, compaction) can vary `model`, `temperature`, and
/// `max_tokens` independently (§5.4).
/// What: `model` is the fully-resolved bare model id for THIS call (the routing
/// prefix has already been stripped by [`resolve`]); `system` is the system
/// prompt; `messages` are the conversation turns.
/// Test: provider round-trip tests construct these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    /// Fully-resolved bare model id for this call (no routing prefix).
    pub model: String,
    /// System prompt (empty string = no system message).
    pub system: String,
    /// Conversation turns.
    pub messages: Vec<ChatMessage>,
    /// Sampling temperature (typically the SM default `0.3`, §5.5).
    pub temperature: f32,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
}

/// Output from [`LlmProvider::complete`].
///
/// Why: captures the response text plus the telemetry the SM logs per call and
/// totals per session (§5.5): tokens, latency, and estimated USD cost.
/// What: `text` is the full response; `model` echoes the model id used;
/// `input_tokens`/`output_tokens` come from the API usage; `latency_ms` is
/// wall-clock; `cost_usd` is an estimate from a per-provider pricing table.
/// Test: `llm_response_serde_roundtrip` below.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    /// Full response text.
    pub text: String,
    /// Model id actually used by the provider.
    pub model: String,
    /// Input (prompt) tokens consumed.
    pub input_tokens: u32,
    /// Output (completion) tokens generated.
    pub output_tokens: u32,
    /// Wall-clock latency in milliseconds.
    pub latency_ms: u64,
    /// Estimated USD cost for this call.
    pub cost_usd: f64,
}

// ─── Provider trait ─────────────────────────────────────────────────────────────

/// A non-streaming LLM completion provider (DOC-14 §5.2).
///
/// Why: the SM and the resolver depend on this trait rather than concrete
/// types so the active provider is an implementation detail, and so tests can
/// inject mocks. Implementors are `Send + Sync` to live behind
/// `Arc<dyn LlmProvider>`.
/// What: one `complete` method (request → response or [`SmLlmError`]) plus a
/// `name` tag for logs/metrics.
/// Test: `provider_trait_object_compiles`; each provider has mock-server tests.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Human-readable provider name, e.g. `"anthropic"`, `"bedrock"`,
    /// `"openrouter"`.
    ///
    /// Why: per-call cost/usage logs (§5.5) tag the provider.
    /// What: a static string slice; no allocation.
    /// Test: each implementation asserts its own name.
    fn name(&self) -> &str;

    /// Execute a non-streaming completion and return the full response.
    ///
    /// Why: the SM needs the full text plus token/latency/cost telemetry in one
    /// call; streaming complicates usage capture (§5.5).
    /// What: sends `req` upstream, waits for the full response, and returns
    /// [`LlmResponse`]. Transient errors may be retried / fell-back by the
    /// caller based on [`SmLlmError::is_retryable`].
    /// Test: each provider implements mock-server tests.
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, SmLlmError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `provider` config values must parse to the right kind; `auto`/empty
    /// both mean "use the precedence chain".
    /// What: asserts each legal value (case-insensitive, trimmed).
    /// Test: this is the test.
    #[test]
    fn provider_kind_parse_ok() {
        assert_eq!(ProviderKind::parse("auto").unwrap(), ProviderKind::Auto);
        assert_eq!(ProviderKind::parse("").unwrap(), ProviderKind::Auto);
        assert_eq!(
            ProviderKind::parse("  Anthropic ").unwrap(),
            ProviderKind::Anthropic
        );
        assert_eq!(
            ProviderKind::parse("BEDROCK").unwrap(),
            ProviderKind::Bedrock
        );
        assert_eq!(
            ProviderKind::parse("openrouter").unwrap(),
            ProviderKind::OpenRouter
        );
    }

    /// Why: `sm.health` reports the resolved provider by name; the name must be
    /// the stable lowercase string that also round-trips back through `parse`.
    /// What: asserts `name()` for each kind and that `parse(name())` recovers it
    /// (modulo `Auto`, whose name `"auto"` parses back to `Auto`).
    /// Test: this is the test.
    #[test]
    fn provider_kind_name_round_trips() {
        for kind in [
            ProviderKind::Auto,
            ProviderKind::Anthropic,
            ProviderKind::Bedrock,
            ProviderKind::OpenRouter,
        ] {
            assert_eq!(ProviderKind::parse(kind.name()).unwrap(), kind);
        }
        assert_eq!(ProviderKind::Anthropic.name(), "anthropic");
    }

    /// Why: SM-1 review carry-forward — an unknown `provider` string must be a
    /// config error, not a silent late failure.
    /// What: asserts `parse` returns a `Validation` error for a bogus value.
    /// Test: this is the test.
    #[test]
    fn provider_kind_parse_rejects_unknown() {
        let err = ProviderKind::parse("gpt-self-host").unwrap_err();
        assert!(matches!(err, SmLlmError::Validation(_)));
        assert!(err.is_alarm(), "unknown provider is a config alarm");
    }

    /// Why: telemetry fields must survive serde so the SM can persist/forward
    /// per-call cost.
    /// What: round-trips an [`LlmResponse`] through JSON.
    /// Test: this is the test.
    #[test]
    fn llm_response_serde_roundtrip() {
        let resp = LlmResponse {
            text: "ok".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            input_tokens: 100,
            output_tokens: 20,
            latency_ms: 42,
            cost_usd: 0.000_5,
        };
        let json = serde_json::to_string(&resp).expect("serialise");
        let back: LlmResponse = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.text, "ok");
        assert_eq!(back.input_tokens, 100);
        assert!((back.cost_usd - 0.000_5_f64).abs() < 1e-12);
    }

    /// Object-safety smoke-test: [`LlmProvider`] must be usable as `dyn`.
    ///
    /// Why: the resolver hands back `Arc<dyn LlmProvider>`.
    /// What: a coercion that only needs to compile.
    /// Test: compilation is the assertion.
    #[test]
    fn provider_trait_object_compiles() {
        fn _accepts(_p: &dyn LlmProvider) {}
    }
}
