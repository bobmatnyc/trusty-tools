//! Cost computation and the model pricing table.
//!
//! Why: the usage ledger prices every recorded turn. #11632: moved here from
//! the cut `perf` module, whose collector and run records left with the
//! harness; the pricing table is the part `usage` reads.
//!
//! **#4098 (COST-05, formerly #4103): this is the SINGLE pricing entry point.**
//! A second, Haiku-only rate table used to live in `usage::daily`, which meant
//! the REPL statusline and the persisted daily total priced every Sonnet turn
//! at Haiku rates — every cost the operator saw was ~12x too low. That table
//! is gone; `usage::daily::cost_from_tokens` and `usage::aggregate` route
//! through [`cost_usd`] here. Do not add a
//! second table: a rate that disagrees with this one is a defect, not a
//! variant.
//!
//! What: `cost_usd` (public) and the `pricing_for`/`per_million` rate helpers.
//! Test: `cost_usd_*` in this module's tests.

/// Compute USD cost for a single LLM call given the model name and token
/// counts. Unknown models fall back to Sonnet-class pricing.
///
/// Why: (#47) We hard-code the pricing table rather than hit a live endpoint
/// so offline/CI runs still produce comparable cost figures. Pricing is
/// per-million-tokens as published by Anthropic and OpenRouter. (#4098) This
/// is the one entry point every cost surface must call — see the module doc
/// for the duplicate table it replaced.
/// What: Substring-matches the model string (e.g. "anthropic/claude-sonnet-4-5"
/// or "claude-haiku-4") and multiplies each token bucket by its rate.
///
/// Counts are `u64` rather than `u32` (#4098) because the aggregate surfaces
/// that now share this entry point — a whole day of dispatches in
/// `usage::aggregate`, a whole REPL session in `usage::daily` — sum well past
/// `u32::MAX`, and a saturating cast at each of those call sites would
/// silently under-report exactly the totals the Costs tab exists to show.
/// Test: `cost_usd_known_sonnet`, `cost_usd_known_haiku`,
/// `cost_usd_unknown_defaults_to_sonnet`, `cost_usd_accepts_counts_beyond_u32`.
pub fn cost_usd(
    model: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_read: u64,
    cache_creation: u64,
) -> f64 {
    // Rates in USD per token (not per million).
    let (rate_in, rate_out, rate_cache_r, rate_cache_w) = pricing_for(model);
    let to_usd = |tokens: u64, rate: f64| tokens as f64 * rate;
    to_usd(prompt_tokens, rate_in)
        + to_usd(completion_tokens, rate_out)
        + to_usd(cache_read, rate_cache_r)
        + to_usd(cache_creation, rate_cache_w)
}

/// Returns (input, output, cache_read, cache_creation) rates per token.
fn pricing_for(model: &str) -> (f64, f64, f64, f64) {
    let m = model.to_ascii_lowercase();
    // Claude Sonnet 4.x — $3 in, $15 out, $0.30 cache read, $3.75 cache write
    if m.contains("sonnet-4") || m.contains("claude-sonnet-4") {
        return (
            per_million(3.0),
            per_million(15.0),
            per_million(0.30),
            per_million(3.75),
        );
    }
    // Claude Haiku 3/4 — $0.80 in, $4 out, $0.08 cache read, $1 cache write
    if m.contains("haiku-3") || m.contains("haiku-4") || m.contains("claude-haiku") {
        return (
            per_million(0.80),
            per_million(4.0),
            per_million(0.08),
            per_million(1.0),
        );
    }
    // Claude Opus 4 — $15 in, $75 out (cache rates not published here, use
    // conservative 10% / 125% of input, matching Anthropic convention).
    if m.contains("opus-4") || m.contains("claude-opus") {
        return (
            per_million(15.0),
            per_million(75.0),
            per_million(1.50),
            per_million(18.75),
        );
    }
    // Default: Sonnet-class rates.
    (
        per_million(3.0),
        per_million(15.0),
        per_million(0.30),
        per_million(3.75),
    )
}

fn per_million(usd: f64) -> f64 {
    usd / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_usd_known_sonnet() {
        // 1M prompt tokens of sonnet = $3.00
        let c = cost_usd("anthropic/claude-sonnet-4-5", 1_000_000, 0, 0, 0);
        assert!((c - 3.0).abs() < 1e-9, "got {c}");
    }

    #[test]
    fn cost_usd_known_haiku() {
        // 1M output tokens of haiku = $4.00
        let c = cost_usd("anthropic/claude-haiku-4", 0, 1_000_000, 0, 0);
        assert!((c - 4.0).abs() < 1e-9, "got {c}");
    }

    #[test]
    fn cost_usd_cache_read_is_cheaper() {
        // Cache-read is 10x cheaper than fresh input on Sonnet.
        let fresh = cost_usd("claude-sonnet-4-6", 1_000_000, 0, 0, 0);
        let cached = cost_usd("claude-sonnet-4-6", 0, 0, 1_000_000, 0);
        assert!(cached < fresh);
        assert!((cached - 0.30).abs() < 1e-9);
    }

    #[test]
    fn cost_usd_sonnet_cache_creation_matches_spec() {
        // #29: Cache write rate for claude-sonnet-4-6 is $3.75/MTok.
        // 1M cache_creation tokens should cost exactly $3.75.
        let c = cost_usd("anthropic/claude-sonnet-4-6", 0, 0, 0, 1_000_000);
        assert!(
            (c - 3.75).abs() < 1e-9,
            "expected $3.75 for 1M cache write tokens on sonnet-4-6, got {c}"
        );
    }

    #[test]
    fn cost_usd_sonnet_cache_read_is_one_tenth_of_input() {
        // #29: Cache read is $0.30/MTok vs input $3.00/MTok — exactly 10%.
        // This is the headline savings figure for prompt caching.
        let fresh_input = cost_usd("claude-sonnet-4-6", 1_000_000, 0, 0, 0);
        let cache_read = cost_usd("claude-sonnet-4-6", 0, 0, 1_000_000, 0);
        let ratio = cache_read / fresh_input;
        assert!(
            (ratio - 0.10).abs() < 1e-9,
            "cache_read should be 10% of input cost, got {ratio} (fresh={fresh_input}, cached={cache_read})"
        );
    }

    #[test]
    fn cost_usd_sonnet_mixed_cache_hit_scenario() {
        // #29: Realistic scenario — a turn where most input is a cache hit:
        // 100 fresh prompt tokens + 50 completion + 9000 cache_read + 1000 cache_creation.
        // Sonnet rates: $3/MTok in, $15/MTok out, $0.30/MTok cache_r, $3.75/MTok cache_w.
        let c = cost_usd("anthropic/claude-sonnet-4-6", 100, 50, 9_000, 1_000);
        // 100 * 3e-6 = 0.0003, 50 * 15e-6 = 0.00075, 9000 * 0.30e-6 = 0.0027,
        // 1000 * 3.75e-6 = 0.00375. Total = 0.00750.
        let expected = 0.0003 + 0.00075 + 0.0027 + 0.00375;
        assert!((c - expected).abs() < 1e-9, "expected {expected}, got {c}");
    }

    #[test]
    fn cost_usd_unknown_defaults_to_sonnet() {
        let u = cost_usd("some/unknown-model", 1_000_000, 0, 0, 0);
        assert!((u - 3.0).abs() < 1e-9);
    }

    #[test]
    fn cost_usd_accepts_counts_beyond_u32() {
        let beyond = u64::from(u32::MAX) + 1;
        let c = cost_usd("anthropic/claude-sonnet-4-6", beyond, 0, 0, 0);
        let expected = beyond as f64 * 3.0 / 1_000_000.0;
        assert!((c - expected).abs() < 1e-6, "expected {expected}, got {c}");
        assert!(c.is_finite());
    }
}
