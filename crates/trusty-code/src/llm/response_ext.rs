//! trusty-code-local views over the SHARED [`ChatResponse`] (#4425).
//!
//! Why: #4425 deleted trusty-code's duplicate `ChatRequest`/`ChatResponse`/
//! `ChatMessage`/`InferenceError` types in favour of `trusty_common::inference`'s.
//! Three helpers that used to be inherent methods on the local response type
//! are trusty-code-specific and therefore have no business on the shared type:
//! they translate into `crate::perf`'s cost vocabulary or encode a
//! trusty-code recording rule. Rust forbids inherent impls on a foreign type,
//! so they live here as free functions rather than being pushed upstream into
//! a crate that has no `perf` module.
//! What: [`token_usage`] (shared wire usage → [`TokenUsage`]), [`finish_reason`]
//! (the raw wire stop string), and [`resolved_model`] (which slug a turn is
//! attributed to, #1475 bug 2).
//! Test: inline `tests` — usage mapping incl. both cache shapes and the
//! authoritative cost, and both `resolved_model` branches.

use trusty_common::inference::ChatResponse;

use crate::perf::TokenUsage;

/// Convert a response's wire usage block into trusty-code's [`TokenUsage`].
///
/// Why: `PerfCollector::record_phase` accepts `&TokenUsage`; keeping the
/// conversion in ONE place stops each call site from re-deciding how the two
/// cache-token shapes (flat Anthropic-native fields vs. OpenRouter's nested
/// `prompt_tokens_details`) merge.
/// What: delegates the merge to the shared `UsageBlock::into_usage` — which is
/// the same field-for-field logic the deleted `UsageBlock::into_token_usage`
/// performed — then copies the five fields across. `cost_usd` carries the
/// provider's authoritative, already-cache-discounted price so callers prefer
/// it over a static per-token recompute.
/// Test: `token_usage_maps_flat_cache_fields`,
/// `token_usage_maps_openrouter_details`, `token_usage_carries_cost`.
pub fn token_usage(response: &ChatResponse) -> TokenUsage {
    let usage = response.usage();
    let mut out = TokenUsage::new(
        usage.prompt_tokens,
        usage.completion_tokens,
        usage.cache_read_tokens,
        usage.cache_creation_tokens,
    );
    out.cost_usd = usage.cost_usd;
    out
}

/// The first choice's raw wire stop condition.
///
/// Why: trusty-code's loop and its tests compare the stop reason as the wire
/// string (`"stop"`, `"tool_calls"`); the shared type exposes it only as the
/// typed `StopReason`, and re-deriving the string at each site would risk two
/// spellings drifting apart.
/// What: `choices[0].finish_reason.as_deref()`, or `None`.
/// Test: `finish_reason_reads_first_choice`.
pub fn finish_reason(response: &ChatResponse) -> Option<&str> {
    response.choices.first()?.finish_reason.as_deref()
}

/// The model slug this turn should be attributed to (#1475 bug 2).
///
/// Why: `run_task::recorder::RecordingLlmClient` must record what the provider
/// actually ran, not merely what was asked for — a routing/fallback slug
/// (`:auto`, a multi-model alias) can resolve to a different concrete model,
/// and pricing the turn against the requested slug would then be wrong.
/// What: the response's `model` when non-empty, else `requested`.
/// Test: `resolved_model_prefers_response_model`,
/// `resolved_model_falls_back_to_requested`.
pub fn resolved_model<'a>(response: &'a ChatResponse, requested: &'a str) -> &'a str {
    if response.model.is_empty() {
        requested
    } else {
        &response.model
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a response carrying only the given usage JSON.
    ///
    /// Why: every usage test differs only in the usage block; one builder keeps
    /// the assertions readable.
    /// What: deserialises a minimal fixture with the supplied usage object.
    fn with_usage(usage_json: &str) -> ChatResponse {
        let fixture = format!(
            r#"{{"id":"gen-1","model":"anthropic/claude-sonnet-4-5",
                 "choices":[{{"message":{{"role":"assistant","content":"ok"}},
                              "finish_reason":"stop"}}],
                 "usage":{usage_json}}}"#
        );
        serde_json::from_str(&fixture).expect("deserialise fixture")
    }

    /// The flat Anthropic-native cache fields map onto `TokenUsage`.
    ///
    /// Why: this is the Bedrock / direct-Anthropic wire shape; dropping it
    /// would silently zero cache effectiveness on those routes.
    /// What: assert all four token buckets.
    /// Test: this test.
    #[test]
    fn token_usage_maps_flat_cache_fields() {
        let resp = with_usage(
            r#"{"prompt_tokens":12,"completion_tokens":5,"total_tokens":17,
                "cache_read_input_tokens":3,"cache_creation_input_tokens":1}"#,
        );
        let usage = token_usage(&resp);
        assert_eq!(usage.prompt_tokens, 12);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.cache_read_tokens, 3);
        assert_eq!(usage.cache_creation_tokens, 1);
    }

    /// OpenRouter's nested `prompt_tokens_details` maps onto the same buckets.
    ///
    /// Why: OpenRouter reports cache counters nested rather than flat; the
    /// caller must not have to know which shape arrived.
    /// What: assert the nested counters surface as cache read/creation.
    /// Test: this test.
    #[test]
    fn token_usage_maps_openrouter_details() {
        let resp = with_usage(
            r#"{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120,
                "prompt_tokens_details":{"cached_tokens":80,"cache_write_tokens":5}}"#,
        );
        let usage = token_usage(&resp);
        assert_eq!(usage.cache_read_tokens, 80);
        assert_eq!(usage.cache_creation_tokens, 5);
    }

    /// The provider's authoritative cost survives the conversion.
    ///
    /// Why: without it, cost reporting falls back to a static per-token
    /// estimate that ignores the cache discount and overstates spend.
    /// What: assert `cost_usd`.
    /// Test: this test.
    #[test]
    fn token_usage_carries_cost() {
        let resp = with_usage(
            r#"{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2,"cost":0.00123}"#,
        );
        assert_eq!(token_usage(&resp).cost_usd, Some(0.00123));
    }

    /// `finish_reason` reads the first choice's raw wire string.
    ///
    /// Why: the loop's stop/continue decision keys off this exact spelling.
    /// What: assert `"stop"` from the fixture.
    /// Test: this test.
    #[test]
    fn finish_reason_reads_first_choice() {
        let resp = with_usage(r#"{}"#);
        assert_eq!(finish_reason(&resp), Some("stop"));
    }

    /// A response that names a model wins over the requested slug.
    ///
    /// Why: the recorder must attribute the turn to what actually ran.
    /// What: assert the response's own model is returned.
    /// Test: this test.
    #[test]
    fn resolved_model_prefers_response_model() {
        let resp = with_usage(r#"{}"#);
        assert_eq!(
            resolved_model(&resp, "asked/for"),
            "anthropic/claude-sonnet-4-5"
        );
    }

    /// An empty response model falls back to the requested slug.
    ///
    /// Why: mocks and older fixtures omit `model`; attributing those turns to
    /// an empty string would break pricing lookups.
    /// What: blank the model, assert the fallback.
    /// Test: this test.
    #[test]
    fn resolved_model_falls_back_to_requested() {
        let mut resp = with_usage(r#"{}"#);
        resp.model = String::new();
        assert_eq!(resolved_model(&resp, "asked/for"), "asked/for");
    }
}
