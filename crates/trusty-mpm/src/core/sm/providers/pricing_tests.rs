//! Tests for the centralised SM pricing table (DOC-14 §5.5).
//!
//! Why: the pricing table is the single source of truth for per-call cost
//! across all three providers; these tests pin the family-substring matching
//! and the estimate arithmetic so a vendor price edit is a deliberate, reviewed
//! change (the numbers can no longer drift per-provider).
//! What: substring matching for the Anthropic Sonnet/Haiku/Opus families (bare
//! and `claude-*` ids), the OpenRouter-routed OpenAI ids, the unknown fallback,
//! and the `estimate_cost_usd` arithmetic. Float comparisons use an epsilon,
//! never exact `==`.
//! Test: included as `#[cfg(test)] mod tests` via `#[path]` from `pricing.rs`.

use super::{cost_per_million, estimate_cost_usd};

const EPS: f64 = 1e-9;

#[test]
fn cost_per_million_matches_anthropic_families() {
    // Bare ids (direct Anthropic + Bedrock) and `claude-*` ids (OpenRouter)
    // must price identically via substring matching.
    for sonnet in ["claude-sonnet-4-6", "us.anthropic.claude-sonnet-4-6"] {
        let (i, o) = cost_per_million(sonnet);
        assert!((i - 3.00).abs() < EPS, "{sonnet} input");
        assert!((o - 15.00).abs() < EPS, "{sonnet} output");
    }
    for haiku in ["claude-haiku-4-5", "us.anthropic.claude-haiku-4-5"] {
        let (i, o) = cost_per_million(haiku);
        assert!((i - 1.00).abs() < EPS, "{haiku} input");
        assert!((o - 5.00).abs() < EPS, "{haiku} output");
    }
    for opus in ["claude-opus-4-1", "global.anthropic.claude-opus"] {
        let (i, o) = cost_per_million(opus);
        assert!((i - 15.00).abs() < EPS, "{opus} input");
        assert!((o - 75.00).abs() < EPS, "{opus} output");
    }
}

#[test]
fn cost_per_million_prices_current_models_at_list_price() {
    // Opus 4.5 onward is $5/$25 (Opus 5.5 is $4/$20) and the 5.x Sonnet and
    // Haiku are cheaper than their families; the substring arms priced them
    // at $15/$75, $3/$15 and $0.80/$4.
    for (model, input, output) in [
        ("claude-opus-4-6", 5.00, 25.00),
        ("us.anthropic.claude-opus-4-6-v1:0", 5.00, 25.00),
        ("claude-opus-5-5", 4.00, 20.00),
        ("claude-sonnet-5-5", 2.00, 10.00),
        ("claude-haiku-5-5", 0.10, 0.50),
        ("openai/gpt-5.5", 5.00, 30.00),
    ] {
        let (i, o) = cost_per_million(model);
        assert!((i - input).abs() < EPS, "{model} input {i}");
        assert!((o - output).abs() < EPS, "{model} output {o}");
    }
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
