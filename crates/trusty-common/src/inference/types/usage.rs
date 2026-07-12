//! Token-usage + cost accounting types (issue #2402, epic #2400 Wave 1).
//!
//! Why: every inference consumer needs one canonical shape for "how many
//! tokens did this call cost, and what did it cost in dollars" — the tcode
//! agent loop, trusty-review's cost ranking, and tga's SM accounting each grew
//! their own. This is the single [`Usage`] the [`super::ChatResponse`] carries,
//! modelled to absorb both wire shapes tcode already parses (Anthropic-native
//! flat cache fields AND OpenRouter's nested `prompt_tokens_details`) so #2406
//! can migrate its `UsageBlock`→`TokenUsage` mapping here without behaviour
//! change.
//! What: [`Usage`] is the normalized in-memory value; [`UsageBlock`] is the
//! wire representation with [`UsageBlock::into_usage`] merging whichever cache
//! shape the provider populated. [`PromptTokensDetails`] is OpenRouter's nested
//! cache-accounting object.
//! Test: inline `tests` — `usage_block_maps_flat_fields`,
//! `usage_block_maps_openrouter_details`, `usage_block_prefers_authoritative_cost`,
//! `default_usage_block_is_zero`, `usage_total_tokens`.

use serde::{Deserialize, Serialize};

/// Normalized token + cost accounting for a single inference call.
///
/// Why: cost/telemetry code must not care whether the provider reported cache
/// tokens as flat Anthropic-native fields or OpenRouter's nested object — it
/// consumes this one shape. `cost_usd` carries the provider's authoritative,
/// already-cache-discounted price when available so callers prefer it over a
/// static per-token recompute.
/// What: four token buckets plus an optional dollar cost. `cost_usd` is `None`
/// when the provider does not report an authoritative figure (the caller may
/// then estimate from [`super::super::registry::pricing`]).
/// Test: `usage_total_tokens`, and every `UsageBlock` mapping test.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt (input) tokens billed, including any cached-read tokens.
    pub prompt_tokens: u32,
    /// Completion (output) tokens produced.
    pub completion_tokens: u32,
    /// Prompt tokens served from the provider's prompt cache (billed at the
    /// discounted cache-read rate).
    pub cache_read_tokens: u32,
    /// Prompt tokens newly written into the prompt cache this turn (billed at
    /// the cache-write rate).
    pub cache_creation_tokens: u32,
    /// Provider-authoritative total USD cost for this call, already netting any
    /// cache discount. `None` when the provider does not report it.
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// Construct a [`Usage`] from the four token buckets, leaving `cost_usd`
    /// unset.
    ///
    /// Why: the common construction path (a provider that reports token counts
    /// but no authoritative dollar cost) should not have to spell out `cost_usd:
    /// None` at every call site.
    /// What: sets the four token fields; `cost_usd` defaults to `None`.
    /// Test: `usage_total_tokens`.
    pub fn new(
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
    ) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            cost_usd: None,
        }
    }

    /// Total tokens (prompt + completion) for this call.
    ///
    /// Why: rate-limit and context-budget code wants a single billed-token
    /// figure; cache buckets are already included in `prompt_tokens` so they
    /// are not re-added here.
    /// What: `prompt_tokens + completion_tokens` (saturating, to never panic on
    /// an adversarial provider response).
    /// Test: `usage_total_tokens`.
    pub fn total_tokens(&self) -> u32 {
        self.prompt_tokens.saturating_add(self.completion_tokens)
    }
}

/// OpenRouter-shaped nested cache-accounting object.
///
/// Why: OpenRouter's usage-accounting normalisation nests cache counters under
/// `prompt_tokens_details` using OpenAI-style names (`cached_tokens`,
/// `cache_write_tokens`) rather than the Anthropic-native flat fields. Without
/// parsing this shape, cache accounting silently reports zero on every
/// OpenRouter call.
/// What: carries the two cache counters OpenRouter reports; `audio_tokens` is
/// deliberately not modelled (irrelevant to text inference).
/// Test: `usage_block_maps_openrouter_details`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct PromptTokensDetails {
    /// Prompt tokens served from cache (OpenAI name for Anthropic's
    /// `cache_read_input_tokens`).
    #[serde(default)]
    pub cached_tokens: u32,
    /// Prompt tokens newly written to cache (OpenAI name for Anthropic's
    /// `cache_creation_input_tokens`).
    #[serde(default)]
    pub cache_write_tokens: u32,
}

/// Wire-shape token-usage block from a chat-completions response.
///
/// Why: the wire `usage` object comes in two shapes (Anthropic-native flat
/// fields on the direct/Bedrock path, OpenRouter's nested `prompt_tokens_details`
/// on the OpenRouter path). Deserialising into this intermediate struct — which
/// accepts BOTH — then mapping to [`Usage`] keeps the provider-shape knowledge
/// in one place.
/// What: carries prompt/completion/total counts, the optional Anthropic-native
/// flat cache fields, the optional OpenRouter nested object, and the optional
/// authoritative `cost`.
/// Test: `usage_block_maps_flat_fields`, `usage_block_maps_openrouter_details`,
/// `usage_block_prefers_authoritative_cost`, `default_usage_block_is_zero`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageBlock {
    /// Prompt tokens (including cached tokens).
    #[serde(default)]
    pub prompt_tokens: u32,
    /// Completion tokens produced.
    #[serde(default)]
    pub completion_tokens: u32,
    /// Prompt + completion; informational only.
    #[serde(default)]
    pub total_tokens: u32,
    /// Anthropic-native (direct/Bedrock path): prompt tokens served from cache.
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    /// Anthropic-native (direct/Bedrock path): prompt tokens written to cache.
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    /// OpenRouter-shaped nested cache counters. `None` when the provider
    /// reports the flat fields instead (or omits cache accounting entirely).
    #[serde(default)]
    pub prompt_tokens_details: Option<PromptTokensDetails>,
    /// Provider-authoritative total USD cost, already netting any cache
    /// discount. `None` for providers that do not report it.
    #[serde(default)]
    pub cost: Option<f64>,
}

impl UsageBlock {
    /// Convert this wire block into the normalized [`Usage`].
    ///
    /// Why: callers speak [`Usage`]; this centralises the wire-field mapping and
    /// merges whichever of the two cache shapes the provider populated so no
    /// call site needs to know the wire field names.
    /// What: `cache_read_tokens` is the flat `cache_read_input_tokens` when
    /// non-zero, else `prompt_tokens_details.cached_tokens`; `cache_creation_tokens`
    /// follows the same fallback. `cost_usd` carries `self.cost` verbatim.
    /// Test: `usage_block_maps_flat_fields`, `usage_block_maps_openrouter_details`,
    /// `usage_block_prefers_authoritative_cost`.
    pub fn into_usage(self) -> Usage {
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

        let mut usage = Usage::new(
            self.prompt_tokens,
            self.completion_tokens,
            cache_read,
            cache_creation,
        );
        usage.cost_usd = self.cost;
        usage
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `total_tokens` is what budget code sums; verify cache buckets are
    /// not double-counted.
    /// Test: itself.
    #[test]
    fn usage_total_tokens() {
        let u = Usage::new(100, 50, 20, 10);
        assert_eq!(u.total_tokens(), 150);
        assert_eq!(u.cost_usd, None);
    }

    /// Why: the Anthropic-native flat shape must map through unchanged.
    /// Test: itself.
    #[test]
    fn usage_block_maps_flat_fields() {
        let block = UsageBlock {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            cache_read_input_tokens: 20,
            cache_creation_input_tokens: 10,
            prompt_tokens_details: None,
            cost: None,
        };
        let u = block.into_usage();
        assert_eq!(u.prompt_tokens, 100);
        assert_eq!(u.completion_tokens, 50);
        assert_eq!(u.cache_read_tokens, 20);
        assert_eq!(u.cache_creation_tokens, 10);
    }

    /// Why: the OpenRouter nested shape (no flat fields) must still populate the
    /// cache buckets — the exact gap that made cache accounting report zero.
    /// Test: itself.
    #[test]
    fn usage_block_maps_openrouter_details() {
        let fixture = r#"{
          "prompt_tokens": 1200,
          "completion_tokens": 80,
          "total_tokens": 1280,
          "prompt_tokens_details": {"cached_tokens": 900, "cache_write_tokens": 300}
        }"#;
        let block: UsageBlock = serde_json::from_str(fixture).expect("deserialise");
        let u = block.into_usage();
        assert_eq!(u.cache_read_tokens, 900);
        assert_eq!(u.cache_creation_tokens, 300);
    }

    /// Why: the provider's authoritative `cost` must be carried through verbatim.
    /// Test: itself.
    #[test]
    fn usage_block_prefers_authoritative_cost() {
        let fixture = r#"{"prompt_tokens": 10, "completion_tokens": 2, "cost": 0.001234}"#;
        let block: UsageBlock = serde_json::from_str(fixture).expect("deserialise");
        assert_eq!(block.into_usage().cost_usd, Some(0.001234));
    }

    /// Why: a missing `usage` object must not panic and must produce zeros.
    /// Test: itself.
    #[test]
    fn default_usage_block_is_zero() {
        let u = UsageBlock::default().into_usage();
        assert_eq!(u.total_tokens(), 0);
        assert_eq!(u.cost_usd, None);
    }
}
