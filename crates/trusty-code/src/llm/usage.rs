//! Token-usage wire types for the OpenRouter / OpenAI chat-completions API.
//!
//! Why: Isolating the `UsageBlock` → `TokenUsage` mapping in its own module
//! keeps request and response types independent and makes the accounting logic
//! easy to audit and test in isolation.
//! What: Defines `UsageBlock` (the wire representation) and its conversion to
//! the crate's `TokenUsage`.
//! Test: `usage_block_maps_to_token_usage`, `default_usage_block_is_zero`.

use serde::Deserialize;

use crate::perf::TokenUsage;

/// Token usage block from the API response.
///
/// Why: Mapping the wire `usage` object to our internal `TokenUsage` is easiest
/// when we first deserialise into this intermediate struct.
/// What: Carries `prompt_tokens`, `completion_tokens`, and `total_tokens` from
/// the wire; optional Anthropic-specific cache fields (present on Anthropic
/// models routed via OpenRouter).
/// Test: `usage_block_maps_to_token_usage`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UsageBlock {
    /// Tokens consumed by the prompt (including cached tokens).
    #[serde(default)]
    pub prompt_tokens: u32,

    /// Tokens produced by the completion.
    #[serde(default)]
    pub completion_tokens: u32,

    /// Sum of prompt + completion; informational only.
    #[serde(default)]
    pub total_tokens: u32,

    /// Anthropic-only: prompt tokens served from the cache (not re-computed).
    #[serde(default)]
    pub cache_read_input_tokens: u32,

    /// Anthropic-only: prompt tokens written into the cache this turn.
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
}

impl UsageBlock {
    /// Convert this wire usage block into the crate's `TokenUsage`.
    ///
    /// Why: `PerfCollector` speaks `TokenUsage`; this conversion centralises the
    /// mapping so callers don't need to know the wire field names.
    /// What: Maps `prompt_tokens` → `prompt_tokens`, `completion_tokens` →
    /// `completion_tokens`, `cache_read_input_tokens` → `cache_read_tokens`,
    /// `cache_creation_input_tokens` → `cache_creation_tokens`.
    /// Test: `usage_block_maps_to_token_usage`.
    pub fn into_token_usage(self) -> TokenUsage {
        TokenUsage::new(
            self.prompt_tokens,
            self.completion_tokens,
            self.cache_read_input_tokens,
            self.cache_creation_input_tokens,
        )
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `UsageBlock::into_token_usage` maps all four fields correctly.
    ///
    /// Why: Correct usage mapping is critical for cost tracking; a mis-map
    /// would silently produce wrong cost calculations.
    /// What: Build a `UsageBlock` with known values, convert, assert each field.
    /// Test: this test.
    #[test]
    fn usage_block_maps_to_token_usage() {
        let block = UsageBlock {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            cache_read_input_tokens: 20,
            cache_creation_input_tokens: 10,
        };
        let usage = block.into_token_usage();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 20);
        assert_eq!(usage.cache_creation_tokens, 10);
    }

    /// Default `UsageBlock` produces a zero `TokenUsage`.
    ///
    /// Why: When the API omits the `usage` field (rare but possible), the
    /// default should not crash and should produce zero counts.
    /// What: Build `UsageBlock::default()`, convert, assert all zeros.
    /// Test: this test.
    #[test]
    fn default_usage_block_is_zero() {
        let usage = UsageBlock::default().into_token_usage();
        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_creation_tokens, 0);
    }
}
