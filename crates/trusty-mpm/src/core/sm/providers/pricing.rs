//! Single source of truth for SM per-call USD cost estimation (DOC-14 §5.5).
//!
//! Why: the three SM providers (Anthropic, Bedrock, OpenRouter) all bill the
//! same Anthropic model FAMILIES (Sonnet / Haiku / Opus) at the same public
//! per-million-token rates, plus a couple of common OpenAI ids routed through
//! OpenRouter. SM-2 review flagged that copy-pasting that pricing table into
//! every provider file means the numbers silently drift out of sync the moment
//! one vendor changes a price. Centralising the table in one dated module gives
//! a single place to audit and update, and keeps every provider's cost
//! telemetry consistent.
//! What: exposes [`cost_per_million`] (family-substring → `(input, output)` USD
//! per million tokens) and [`estimate_cost_usd`] (token counts → USD). Both use
//! the same substring matching the per-provider copies used before, so behaviour
//! is identical for every previously-priced model.
//! Test: `pricing_tests.rs` (substring matching, known/unknown estimates, the
//! OpenRouter-routed OpenAI ids); per-provider tests still assert that their
//! `complete` round-trips compute the expected cost via this module.
//
// ─────────────────────────────────────────────────────────────────────────────
// Pricing as of 2026-06 — update when Anthropic/AWS/OpenRouter publish changes.
//   - Anthropic:  https://www.anthropic.com/pricing
//   - AWS Bedrock: https://aws.amazon.com/bedrock/pricing/
//   - OpenRouter: https://openrouter.ai/models
// All rates are USD per 1,000,000 tokens, expressed as (input, output).
// ─────────────────────────────────────────────────────────────────────────────

/// Approximate `(input, output)` USD cost per million tokens for the model
/// families the SM routes through any of its three providers.
///
/// Why: the SM logs a per-call cost estimate in `LlmResponse` (§5.5); all three
/// providers need the SAME table so their telemetry agrees and updates land in
/// one place. Unknown ids fall back to `(0.0, 0.0)` — a missing estimate, not
/// an error.
/// What: matches on the model-family substring (so version suffixes such as
/// `-4-6` still price) for the Anthropic Sonnet/Haiku/Opus tiers, accepting both
/// the bare `sonnet`/`haiku`/`opus` ids (direct Anthropic + Bedrock) and the
/// `claude-sonnet`/… ids OpenRouter uses; plus a couple of common OpenAI ids
/// routed through OpenRouter.
/// Test: `pricing::tests::cost_per_million_*`.
pub fn cost_per_million(model: &str) -> (f64, f64) {
    match model {
        // Anthropic families (direct, Bedrock, and OpenRouter `claude-*` ids).
        m if m.contains("sonnet") => (3.00, 15.00),
        m if m.contains("haiku") => (0.80, 4.00),
        m if m.contains("opus") => (15.00, 75.00),
        // Common OpenAI ids routed through OpenRouter.
        "openai/gpt-5.4-mini-20260317" => (0.75, 4.50),
        "openai/gpt-5.4-nano-20260317" => (0.20, 1.25),
        _ => (0.0, 0.0),
    }
}

/// Compute a USD cost estimate from token counts and the pricing table.
///
/// Why: lets the SM total session cost and rank tiers by cost (§5.5) using one
/// shared formula across every provider.
/// What: applies [`cost_per_million`]; returns `0.0` for unknown models.
/// Test: `pricing::tests::estimate_cost_usd_*`.
pub fn estimate_cost_usd(model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
    let (in_price, out_price) = cost_per_million(model);
    (input_tokens as f64 / 1_000_000.0) * in_price
        + (output_tokens as f64 / 1_000_000.0) * out_price
}

#[cfg(test)]
#[path = "pricing_tests.rs"]
mod tests;
