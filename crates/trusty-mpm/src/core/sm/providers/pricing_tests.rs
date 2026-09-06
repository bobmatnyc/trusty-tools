//! Tests for the SM's pricing adapter (DOC-14 §5.5).
//!
//! Why: these providers are what the SM bills against, so the adapter must keep
//! answering with the routing spellings the providers actually pass — bare ids,
//! `us.anthropic.…` Bedrock ids, OpenRouter's `anthropic/claude-…`.
//! What: (#6875) the rates themselves are asserted in `trusty-common`'s
//! `pricing_tests.rs`; here we pin that the adapter RESOLVES each spelling to
//! the same row, that the OpenRouter-routed OpenAI ids survived the move
//! verbatim, that an unknown id is zero, and the `estimate_cost_usd` arithmetic.
//! Float comparisons use an epsilon, never exact `==`.
//! Test: included as `#[cfg(test)] mod tests` via `#[path]` from `pricing.rs`.

use super::{cost_per_million, estimate_cost_usd};

const EPS: f64 = 1e-9;

/// #6875: every routing spelling of one model must price identically. The
/// numbers come from `trusty-common/pricing.toml`; Opus and Haiku moved to
/// their currently published rates when the table was consolidated.
#[test]
fn cost_per_million_matches_anthropic_families() {
    for sonnet in [
        "claude-sonnet-4-6",
        "us.anthropic.claude-sonnet-4-6",
        "bedrock/us.anthropic.claude-sonnet-4-6",
    ] {
        let (i, o) = cost_per_million(sonnet);
        assert!((i - 3.00).abs() < EPS, "{sonnet} input, got {i}");
        assert!((o - 15.00).abs() < EPS, "{sonnet} output, got {o}");
    }
    // Haiku 4.5 is $1/$5 — this file previously recorded $0.80/$4.
    for haiku in ["claude-haiku-4-5", "us.anthropic.claude-haiku-4-5"] {
        let (i, o) = cost_per_million(haiku);
        assert!((i - 1.00).abs() < EPS, "{haiku} input, got {i}");
        assert!((o - 5.00).abs() < EPS, "{haiku} output, got {o}");
    }
    // Opus 5 is $5/$25 — this file previously recorded $15/$75 for every Opus.
    for opus in ["claude-opus-5", "global.anthropic.claude-opus-5"] {
        let (i, o) = cost_per_million(opus);
        assert!((i - 5.00).abs() < EPS, "{opus} input, got {i}");
        assert!((o - 25.00).abs() < EPS, "{opus} output, got {o}");
    }
    // The 4.x Opus generation keeps the rate carried over from this table.
    let (i, o) = cost_per_million("claude-opus-4-1");
    assert!((i - 15.00).abs() < EPS, "opus-4-1 input, got {i}");
    assert!((o - 75.00).abs() < EPS, "opus-4-1 output, got {o}");
}

#[test]
fn cost_per_million_matches_openrouter_openai_ids() {
    let (i, o) = cost_per_million("openai/gpt-5.4-mini-20260317");
    assert!((i - 0.75).abs() < EPS);
    assert!((o - 4.50).abs() < EPS);
    let (i, o) = cost_per_million("openai/gpt-5.4-nano-20260317");
    assert!((i - 0.20).abs() < EPS);
    assert!((o - 1.25).abs() < EPS);
}

#[test]
fn cost_per_million_unknown_is_zero() {
    let (i, o) = cost_per_million("mystery/model");
    assert!(i.abs() < EPS);
    assert!(o.abs() < EPS);
}

#[test]
fn estimate_cost_usd_known_sonnet() {
    // 1e6 input @ 3.00 + 1e6 output @ 15.00 = 18.00
    let cost = estimate_cost_usd("claude-sonnet-4-6", 1_000_000, 1_000_000);
    assert!((cost - 18.0).abs() < EPS, "expected $18.00, got {cost}");
}

#[test]
fn estimate_cost_usd_known_haiku() {
    // 1e6 input @ 1.00 + 1e6 output @ 5.00 = 6.00
    let cost = estimate_cost_usd("claude-haiku-4-5", 1_000_000, 1_000_000);
    assert!((cost - 6.00).abs() < EPS, "expected $6.00, got {cost}");
}

#[test]
fn estimate_cost_usd_partial_tokens() {
    // 2000/1e6*3.00 + 100/1e6*15.00 = 0.006 + 0.0015 = 0.0075
    let cost = estimate_cost_usd("claude-sonnet-4-6", 2000, 100);
    assert!((cost - 0.0075).abs() < EPS, "expected $0.0075, got {cost}");
}

#[test]
fn estimate_cost_usd_unknown_is_zero() {
    let cost = estimate_cost_usd("gpt-self-host", 100, 100);
    assert!(cost.abs() < EPS, "unknown model should cost 0, got {cost}");
}

/// Why (#6875): the two tables this consolidation replaced disagreed with each
/// other about Sonnet 5 — the SM's `contains("sonnet")` arm priced it at $3/$15
/// and trusty-agents' default arm did the same. Both are now $2/$10.
/// What: Sonnet 5 prices at its published rate through this adapter.
/// Test: this test.
#[test]
fn cost_per_million_sonnet_5_is_no_longer_priced_at_4x_rates() {
    let (i, o) = cost_per_million("claude-sonnet-5");
    assert!((i - 2.00).abs() < EPS, "expected $2.00 input, got {i}");
    assert!((o - 10.00).abs() < EPS, "expected $10.00 output, got {o}");
}
