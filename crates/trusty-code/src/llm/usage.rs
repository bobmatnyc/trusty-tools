//! Token-usage wire types for the OpenRouter / OpenAI chat-completions API.
//!
//! Why: Isolating the `UsageBlock` → `TokenUsage` mapping in its own module
//! keeps request and response types independent and makes the accounting logic
//! easy to audit and test in isolation.
//! What: Defines `UsageBlock` (the wire representation) and its conversion to
//! the crate's `TokenUsage`.
//! Test: `usage_block_maps_to_token_usage`, `default_usage_block_is_zero`,
//! `usage_block_maps_openrouter_prompt_tokens_details`,
//! `usage_block_prefers_authoritative_cost`.

use serde::Deserialize;

use crate::perf::TokenUsage;

/// OpenRouter/OpenAI-shaped nested cache-accounting object (closes the
/// response-side gap blocking #2156's cost validation).
///
/// Why: Anthropic-native responses report cache tokens as flat
/// `cache_read_input_tokens` / `cache_creation_input_tokens` fields, but
/// OpenRouter's own usage-accounting normalisation (verified against
/// OpenRouter's `usage-accounting` docs) nests them instead under
/// `prompt_tokens_details`, using the OpenAI-style names `cached_tokens` (read)
/// and `cache_write_tokens` (write). Without parsing this shape, `UsageBlock`'s
/// `#[serde(default)]` cache fields silently stayed zero on every OpenRouter
/// call — cache_read/cache_creation always reported 0 and cost never reflected
/// the cache discount, even though the request-side `cache_control` marker
/// (#2156) was being honoured upstream.
/// What: Carries the two cache counters OpenRouter reports; `audio_tokens` is
/// deliberately not modelled (irrelevant to this crate's text-only usage).
/// Test: `usage_block_maps_openrouter_prompt_tokens_details`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PromptTokensDetails {
    /// Prompt tokens served from the cache (OpenRouter's OpenAI-style name for
    /// what Anthropic calls `cache_read_input_tokens`).
    #[serde(default)]
    pub cached_tokens: u32,

    /// Prompt tokens newly written into the cache this turn (OpenRouter's
    /// OpenAI-style name for what Anthropic calls
    /// `cache_creation_input_tokens`).
    #[serde(default)]
    pub cache_write_tokens: u32,
}

/// Token usage block from the API response.
///
/// Why: Mapping the wire `usage` object to our internal `TokenUsage` is easiest
/// when we first deserialise into this intermediate struct.
/// What: Carries `prompt_tokens`, `completion_tokens`, and `total_tokens` from
/// the wire; optional Anthropic-native cache fields (flat
/// `cache_read_input_tokens` / `cache_creation_input_tokens`, present on the
/// direct/Bedrock path) AND the OpenRouter-shaped nested
/// `prompt_tokens_details` object — both are accepted so either provider's
/// wire shape populates the canonical `TokenUsage` cache fields. `cost` is
/// OpenRouter's authoritative, already-discounted USD cost for the call (only
/// present when OpenRouter includes detailed usage accounting).
/// Test: `usage_block_maps_to_token_usage`,
/// `usage_block_maps_openrouter_prompt_tokens_details`,
/// `usage_block_prefers_authoritative_cost`.
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

    /// Anthropic-native (direct/Bedrock path): prompt tokens served from the
    /// cache (not re-computed).
    #[serde(default)]
    pub cache_read_input_tokens: u32,

    /// Anthropic-native (direct/Bedrock path): prompt tokens written into the
    /// cache this turn.
    #[serde(default)]
    pub cache_creation_input_tokens: u32,

    /// OpenRouter-shaped nested cache counters (OpenAI wire style). `None`
    /// when the provider reports the flat Anthropic-native fields instead (or
    /// omits cache accounting entirely).
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,

    /// OpenRouter's authoritative total USD cost for this call, already
    /// netting any cache discount. `None` for providers that don't report it
    /// (direct/Bedrock) or when OpenRouter's detailed-usage accounting wasn't
    /// returned.
    #[serde(default)]
    pub cost: Option<f64>,
}

impl UsageBlock {
    /// Convert this wire usage block into the crate's `TokenUsage`.
    ///
    /// Why: `PerfCollector` speaks `TokenUsage`; this conversion centralises the
    /// mapping so callers don't need to know the wire field names, and merges
    /// whichever of the two wire cache shapes (Anthropic-native flat fields vs.
    /// OpenRouter's nested `prompt_tokens_details`) the provider populated.
    /// What: `cache_read_tokens` is `cache_read_input_tokens` when non-zero,
    /// else `prompt_tokens_details.cached_tokens`; `cache_creation_tokens`
    /// follows the same fallback using `cache_creation_input_tokens` /
    /// `cache_write_tokens`. `cost_usd` on the resulting `TokenUsage` carries
    /// `self.cost` verbatim so callers can prefer OpenRouter's authoritative,
    /// cache-aware price over a static per-token recompute.
    /// Test: `usage_block_maps_to_token_usage`,
    /// `usage_block_maps_openrouter_prompt_tokens_details`,
    /// `usage_block_prefers_authoritative_cost`.
    pub fn into_token_usage(self) -> TokenUsage {
        let (details_read, details_write) = self
            .prompt_tokens_details
            .map(|d| (d.cached_tokens, d.cache_write_tokens))
            .unwrap_or((0, 0));

        let cache_read = if self.cache_read_input_tokens != 0 {
            self.cache_read_input_tokens
        } else {
            details_read
        };
        let cache_creation = if self.cache_creation_input_tokens != 0 {
            self.cache_creation_input_tokens
        } else {
            details_write
        };

        let mut usage = TokenUsage::new(
            self.prompt_tokens,
            self.completion_tokens,
            cache_read,
            cache_creation,
        );
        usage.cost_usd = self.cost;
        usage
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
            prompt_tokens_details: None,
            cost: None,
        };
        let usage = block.into_token_usage();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 20);
        assert_eq!(usage.cache_creation_tokens, 10);
        assert_eq!(usage.cost_usd, None);
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
        assert_eq!(usage.cost_usd, None);
    }

    /// An OpenRouter-shaped `usage` block (nested `prompt_tokens_details`,
    /// NEITHER flat Anthropic-native cache field present) deserialises into a
    /// non-zero `cache_read_tokens` / `cache_creation_tokens` — the exact gap
    /// that made #2156's OpenRouter-path cache accounting always report 0.
    ///
    /// Why: This is the empirically observed shape from the bake-off pilot:
    /// OpenRouter reports cache accounting under
    /// `prompt_tokens_details: { cached_tokens, cache_write_tokens }`, not the
    /// Anthropic-native flat fields.
    /// What: Deserialise the OpenRouter-shaped fixture, assert cache_read /
    /// cache_creation map from the nested object.
    /// Test: this test.
    #[test]
    fn usage_block_maps_openrouter_prompt_tokens_details() {
        let fixture = r#"{
          "prompt_tokens": 1200,
          "completion_tokens": 80,
          "total_tokens": 1280,
          "prompt_tokens_details": {"cached_tokens": 900, "cache_write_tokens": 300}
        }"#;
        let block: UsageBlock = serde_json::from_str(fixture).expect("deserialise");
        let usage = block.into_token_usage();
        assert_eq!(usage.prompt_tokens, 1200);
        assert_eq!(usage.completion_tokens, 80);
        assert_eq!(usage.cache_read_tokens, 900);
        assert_eq!(usage.cache_creation_tokens, 300);
    }

    /// The Anthropic-native flat shape still deserialises and maps correctly
    /// when both wire shapes are theoretically present (flat fields win, since
    /// they are the shape this crate has verified against the direct/Bedrock
    /// path).
    ///
    /// Why: The fix must not regress the pre-existing direct/Bedrock parsing
    /// path while adding OpenRouter support.
    /// What: Deserialise a fixture with only the Anthropic-native flat fields
    /// set, assert they map through unchanged.
    /// Test: this test.
    #[test]
    fn usage_block_still_maps_anthropic_native_shape() {
        let fixture = r#"{
          "prompt_tokens": 500,
          "completion_tokens": 20,
          "total_tokens": 520,
          "cache_read_input_tokens": 400,
          "cache_creation_input_tokens": 50
        }"#;
        let block: UsageBlock = serde_json::from_str(fixture).expect("deserialise");
        let usage = block.into_token_usage();
        assert_eq!(usage.cache_read_tokens, 400);
        assert_eq!(usage.cache_creation_tokens, 50);
    }

    /// When OpenRouter's authoritative `usage.cost` is present, it is carried
    /// through onto `TokenUsage::cost_usd` verbatim (issue: cost never
    /// reflected the cache discount because tcode recomputed from static
    /// pricing instead of reading this field).
    ///
    /// Why: The static per-token pricing table cannot see the cache discount;
    /// only OpenRouter's own `cost` field nets it correctly.
    /// What: Deserialise a fixture with `cost` set, assert `cost_usd` matches.
    /// Test: this test.
    #[test]
    fn usage_block_prefers_authoritative_cost() {
        let fixture = r#"{
          "prompt_tokens": 1200,
          "completion_tokens": 80,
          "total_tokens": 1280,
          "prompt_tokens_details": {"cached_tokens": 900, "cache_write_tokens": 0},
          "cost": 0.001234
        }"#;
        let block: UsageBlock = serde_json::from_str(fixture).expect("deserialise");
        let usage = block.into_token_usage();
        assert_eq!(usage.cost_usd, Some(0.001234));
    }
}
