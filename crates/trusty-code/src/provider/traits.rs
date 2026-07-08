//! Provider trait and tool-choice abstraction.
//!
//! Why: Different LLM backends (OpenRouter, AWS Bedrock, …) normalise
//! `tool_choice` and token-usage differently on the wire. The agent loop should
//! not branch on the provider; instead it asks a `Provider` to translate a
//! provider-neutral [`ToolChoice`] into the backend's wire format and to extract
//! a canonical [`TokenUsage`] from the response. This keeps per-backend quirks
//! behind one seam (#1021) and feeds fallback-guidance selection in #1032/#1023.
//! What: Defines [`ToolChoice`] (the neutral policy enum) and the [`Provider`]
//! trait with `name`, `map_tool_choice`, `extract_usage`, and
//! `supports_native_tools`.
//! Test: `provider::tests::*` plus per-impl tests in `openrouter` and `bedrock`.

use serde_json::Value;

use crate::llm::ChatResponse;
use crate::perf::TokenUsage;

/// Provider-neutral tool-choice policy.
///
/// Why: Callers (the agent loop) want to express intent — "don't call tools",
/// "you may call tools", "you must call a tool", or "call this specific
/// function" — without knowing how each backend spells it on the wire. The
/// `Provider` translates this enum via [`Provider::map_tool_choice`].
/// What: `None` disables tools, `Auto` lets the model decide, `Required` forces
/// some tool call, and `Function(name)` forces a specific named function.
/// Test: `openrouter::tests::map_tool_choice_*` cover every variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoice {
    /// The model must not call any tool (`"none"`).
    None,
    /// The model may call a tool if it decides to (`"auto"`).
    Auto,
    /// The model must call some tool this turn (`"required"`).
    Required,
    /// The model must call the named function.
    Function(String),
}

/// A backend that normalises tool-choice and usage for one family of models.
///
/// Why: The agent loop must stay backend-agnostic. By depending on `dyn
/// Provider` it can drive OpenRouter today and Bedrock later without changing
/// the loop — the factory ([`crate::provider::provider_for`]) picks the impl
/// from the model slug.
/// What: Object-safe (`Send + Sync`) trait with the four normalisation hooks the
/// loop needs. Implementations live in `openrouter` and `bedrock`.
/// Test: `openrouter::tests::*`, `bedrock::tests::*`.
pub trait Provider: Send + Sync {
    /// Stable provider name for logging and diagnostics.
    ///
    /// Why: Telemetry and error messages need a human-readable backend label
    /// that does not change across releases.
    /// What: Returns a short identifier such as `"openrouter"` or `"bedrock"`.
    /// Test: `openrouter::tests::name_is_stable`, `bedrock::tests::name_is_bedrock`.
    fn name(&self) -> &str;

    /// Translate a neutral [`ToolChoice`] into this backend's wire JSON.
    ///
    /// Why: OpenAI-style backends use `"none"`/`"auto"`/`"required"` strings and
    /// a `{type:function, function:{name}}` object for a specific call; other
    /// backends differ. Centralising the mapping keeps the loop clean.
    /// What: Returns the `serde_json::Value` to place in
    /// `ChatRequest.tool_choice`.
    /// Test: `openrouter::tests::map_tool_choice_*`.
    fn map_tool_choice(&self, choice: ToolChoice) -> Value;

    /// Extract canonical token usage from a chat response.
    ///
    /// Why: Cost accounting needs a single `TokenUsage` shape regardless of how
    /// the backend reports prompt/completion/cache tokens.
    /// What: Reads the provider-specific usage block out of `response` and maps
    /// it to [`TokenUsage`].
    /// Test: `openrouter::tests::extract_usage_maps_fields`.
    fn extract_usage(&self, response: &ChatResponse) -> TokenUsage;

    /// Whether the backend/model supports native function-calling.
    ///
    /// Why: Models without native tool support need prompt-injected fallback
    /// guidance instead of the `tools` array; this flag drives that selection in
    /// #1032/#1023.
    /// What: Returns `true` when the model can be sent the native `tools` array,
    /// `false` when the loop must fall back to prompt-based tool emulation.
    /// Test: `openrouter::tests::supports_native_tools_*`,
    /// `bedrock::tests::bedrock_supports_native_tools`.
    fn supports_native_tools(&self) -> bool;

    /// Whether this backend/model honours Anthropic-style `cache_control`
    /// prompt-cache breakpoints (#2156).
    ///
    /// Why: Emitting a `cache_control` marker to a backend/model that ignores
    /// it is harmless (the field is simply dropped), but gating on this flag
    /// keeps the wire payload minimal for models that can never benefit and
    /// documents, in one place, exactly which backends this has been
    /// verified against — rather than scattering `model.starts_with(...)`
    /// checks at every call site.
    /// What: Returns `true` when `agent_loop::build_request` should mark the
    /// static tools+system-prompt prefix with an ephemeral cache breakpoint;
    /// `false` otherwise.
    /// Test: `openrouter::tests::supports_prompt_caching_*`,
    /// `bedrock::tests::bedrock_does_not_support_prompt_caching`.
    fn supports_prompt_caching(&self) -> bool;

    /// Whether this backend should be asked for detailed usage accounting
    /// (response-side cache-usage fix).
    ///
    /// Why: OpenRouter's detailed-usage directive
    /// (`RequestUsageConfig::detailed`, `usage: {"include": true}`) is what
    /// makes OpenRouter return its authoritative, cache-discount-aware
    /// `usage.cost` and the nested `prompt_tokens_details` cache counters —
    /// without it only the bare prompt/completion/total counts are
    /// guaranteed. This is an OpenRouter-specific wire directive with no
    /// equivalent on the direct/Bedrock path, so it must stay gated per
    /// backend rather than sent unconditionally.
    /// What: Returns `true` when `agent_loop::build_request` should attach
    /// `ChatRequest.usage = Some(RequestUsageConfig::detailed())`; `false`
    /// otherwise.
    /// Test: `openrouter::tests::wants_detailed_usage_is_true`,
    /// `bedrock::tests::bedrock_does_not_want_detailed_usage`.
    fn wants_detailed_usage(&self) -> bool;
}
