//! SM per-call USD cost estimation over the shared table (DOC-14 §5.5).
//!
//! Why: the three SM providers (Anthropic, Bedrock, OpenRouter) all bill the
//! same Anthropic models, so copy-pasting a pricing table into every provider
//! file meant the numbers drifted the moment one vendor changed a price. SM-2
//! review centralised them here.
//!
//! **#6875: the table itself moved again, out of this crate.** Centralising per
//! crate still left three tables in the workspace — this one, a differently
//! stale one in `trusty-agents`, and the one #6872's cost ledger would have
//! added. The rows now live in `trusty_common::pricing`, whose `pricing.toml`
//! carries `effective_from` dates, alias resolution and an operator override.
//! Two rates changed as a result: Opus is $5/$25 rather than the $15/$75 this
//! file recorded, and Haiku 4.5 is $1/$5 rather than $0.80/$4.
//!
//! What: [`cost_per_million`] (model id → `(input, output)` USD per million
//! tokens) and [`estimate_cost_usd`] (token counts → USD), both thin adapters
//! over the shared table so every existing provider call site is unchanged.
//! Test: `pricing_tests.rs`; per-provider tests still assert that their
//! `complete` round-trips compute the expected cost via this module.

use trusty_common::pricing::{Usage, shared, warn_unknown_model_once};

/// `(input, output)` USD cost per million tokens for a model the SM routes
/// through any of its three providers.
///
/// Why: the SM logs a per-call cost estimate in `LlmResponse` (§5.5); all three
/// providers need the SAME numbers so their telemetry agrees.
/// What: (#6875) resolves today's row from `trusty_common::pricing`, which
/// normalises the routing spellings the providers actually pass — bare ids,
/// `us.anthropic.…` Bedrock ids, and OpenRouter's `anthropic/claude-…`. An id no
/// row claims returns `(0.0, 0.0)` after [`warn_unknown_model_once`]: a missing
/// estimate, reported, rather than an error or a fabricated rate.
/// Test: `pricing::tests::cost_per_million_*`.
pub fn cost_per_million(model: &str) -> (f64, f64) {
    // #6875: one table for the workspace — see trusty_common::pricing.
    match shared().rate_today(model) {
        Some(rates) => (rates.input, rates.output),
        None => {
            warn_unknown_model_once(model);
            (0.0, 0.0)
        }
    }
}

/// Compute a USD cost estimate from token counts and the pricing table.
///
/// Why: lets the SM total session cost and rank tiers by cost (§5.5) using one
/// shared formula across every provider.
/// What: prices the input and output buckets from the shared table. The SM's
/// `LlmResponse` carries no cache-token counts, so those buckets stay zero here;
/// `trusty-agents` prices all four through the same table.
/// Test: `pricing::tests::estimate_cost_usd_*`.
pub fn estimate_cost_usd(model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
    let usage = Usage::new(u64::from(input_tokens), u64::from(output_tokens), 0, 0);
    match shared().rate_today(model) {
        Some(rates) => rates.cost_usd(&usage),
        None => {
            warn_unknown_model_once(model);
            0.0
        }
    }
}

#[cfg(test)]
#[path = "pricing_tests.rs"]
mod tests;
