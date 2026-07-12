//! Per-model pricing (issue #2402, epic #2400 Wave 1).
//!
//! Why: cost estimation (trusty-review's `compare` ranking, tga's SM accounting)
//! needs one pricing source rather than three hard-coded tables. This is the
//! seed the epic references ("pricing from trusty-review"), consolidating
//! tcode's Claude rates and trusty-review's GPT-5/Gemini rates into one lookup.
//! Rates are a documented BEST-EFFORT snapshot — the provider's authoritative
//! `usage.cost` (carried on [`crate::inference::Usage::cost_usd`]) is always
//! preferred when present; this table is the offline/CI fallback.
//! What: [`Pricing`] (USD per MILLION tokens, four buckets) and [`pricing`], a
//! substring lookup returning `Some(Pricing)` for known families, `None`
//! otherwise.
//! Test: inline `tests` — `claude_family_rates`, `gpt5_and_gemini_rates`,
//! `unknown_model_is_none`, `estimate_matches_hand_calc`.

/// USD-per-million-tokens pricing for one model family.
///
/// Why: a typed rate quartet lets callers compute a cost estimate from a
/// [`crate::inference::Usage`] without re-deriving the per-token math or the
/// wire field names.
/// What: `input`/`output`/`cache_read`/`cache_write` are USD per 1,000,000
/// tokens. Providers that publish no separate cache rate use `0.0` for those
/// buckets (a cache-read then bills at the input rate via the caller's own
/// accounting, or simply contributes nothing here).
/// Test: `estimate_matches_hand_calc`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pricing {
    /// USD per million input (prompt) tokens.
    pub input: f64,
    /// USD per million output (completion) tokens.
    pub output: f64,
    /// USD per million cache-read tokens.
    pub cache_read: f64,
    /// USD per million cache-write tokens.
    pub cache_write: f64,
}

impl Pricing {
    /// Estimate the USD cost of a call from its token buckets.
    ///
    /// Why: a convenience for the offline/CI estimate path; live cost should
    /// prefer the provider's authoritative figure when available.
    /// What: sums each bucket × its per-million rate.
    /// Test: `estimate_matches_hand_calc`.
    pub fn estimate_usd(
        &self,
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_write_tokens: u32,
    ) -> f64 {
        let per_m = |tokens: u32, rate: f64| (tokens as f64 / 1_000_000.0) * rate;
        per_m(input_tokens, self.input)
            + per_m(output_tokens, self.output)
            + per_m(cache_read_tokens, self.cache_read)
            + per_m(cache_write_tokens, self.cache_write)
    }
}

/// Best-effort per-million pricing for a model slug.
///
/// Why: the registry's pricing query. Substring matching (rather than exact
/// slugs) keeps version-stamped point releases priced without a table churn.
/// What: returns `Some(Pricing)` for the Claude, GPT-5, and Gemini families this
/// project uses; `None` for anything unrecognised (the caller then relies on the
/// provider's authoritative cost or reports "unknown"). Claude rates are from
/// tcode `perf::pricing`; GPT-5/Gemini rates from trusty-review
/// `llm::openrouter` (OpenRouter pricing page, best-effort, 2026 snapshot).
/// Test: `claude_family_rates`, `gpt5_and_gemini_rates`, `unknown_model_is_none`.
pub fn pricing(model: &str) -> Option<Pricing> {
    let m = model.to_ascii_lowercase();

    // ── Claude family (source: tcode perf::pricing) ──
    if m.contains("claude-haiku") || m.contains("haiku-3") || m.contains("haiku-4") {
        return Some(Pricing {
            input: 0.80,
            output: 4.0,
            cache_read: 0.08,
            cache_write: 1.0,
        });
    }
    if m.contains("claude-opus") || m.contains("opus-4") {
        return Some(Pricing {
            input: 15.0,
            output: 75.0,
            cache_read: 1.50,
            cache_write: 18.75,
        });
    }
    if m.contains("claude-sonnet") || m.contains("sonnet-4") {
        return Some(Pricing {
            input: 3.0,
            output: 15.0,
            cache_read: 0.30,
            cache_write: 3.75,
        });
    }

    // ── GPT-5 family (source: trusty-review llm::openrouter) ──
    if m.contains("gpt-5.5-pro") {
        return Some(Pricing {
            input: 30.0,
            output: 180.0,
            cache_read: 0.0,
            cache_write: 0.0,
        });
    }
    if m.contains("gpt-5.5") {
        return Some(Pricing {
            input: 5.0,
            output: 30.0,
            cache_read: 0.0,
            cache_write: 0.0,
        });
    }
    if m.contains("gpt-5.4-nano") {
        return Some(Pricing {
            input: 0.20,
            output: 1.25,
            cache_read: 0.0,
            cache_write: 0.0,
        });
    }
    if m.contains("gpt-5.4-mini") {
        return Some(Pricing {
            input: 0.75,
            output: 4.50,
            cache_read: 0.0,
            cache_write: 0.0,
        });
    }
    if m.contains("gpt-5.4") {
        return Some(Pricing {
            input: 2.50,
            output: 15.0,
            cache_read: 0.0,
            cache_write: 0.0,
        });
    }

    // ── Gemini family (source: trusty-review, best-effort tiers) ──
    if m.contains("gemini") {
        return Some(if m.contains("flash-lite") {
            Pricing {
                input: 0.10,
                output: 0.40,
                cache_read: 0.0,
                cache_write: 0.0,
            }
        } else if m.contains("flash") {
            Pricing {
                input: 0.30,
                output: 2.50,
                cache_read: 0.0,
                cache_write: 0.0,
            }
        } else {
            Pricing {
                input: 1.25,
                output: 10.0,
                cache_read: 0.0,
                cache_write: 0.0,
            }
        });
    }

    None
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the Claude rates must match the authoritative tcode table.
    /// Test: itself.
    #[test]
    fn claude_family_rates() {
        let haiku = pricing("anthropic/claude-haiku-4-5").expect("haiku priced");
        assert_eq!(haiku.input, 0.80);
        assert_eq!(haiku.output, 4.0);
        let sonnet = pricing("bedrock/us.anthropic.claude-sonnet-4-6").expect("sonnet priced");
        assert_eq!(sonnet.input, 3.0);
        let opus = pricing("claude-opus-4-1").expect("opus priced");
        assert_eq!(opus.output, 75.0);
    }

    /// Why: the GPT-5 and Gemini families must price via substring, most-specific
    /// first (nano/mini before the bare 5.4; flash-lite before flash).
    /// Test: itself.
    #[test]
    fn gpt5_and_gemini_rates() {
        assert_eq!(pricing("openai/gpt-5.5-pro-20260423").unwrap().input, 30.0);
        assert_eq!(pricing("openai/gpt-5.4-nano-20260317").unwrap().input, 0.20);
        assert_eq!(pricing("openai/gpt-5.4-mini-20260317").unwrap().input, 0.75);
        assert_eq!(pricing("openai/gpt-5.4-20260305").unwrap().input, 2.50);
        assert_eq!(pricing("google/gemini-2.5-flash-lite").unwrap().input, 0.10);
        assert_eq!(pricing("google/gemini-2.5-flash").unwrap().input, 0.30);
        assert_eq!(pricing("google/gemini-2.5-pro").unwrap().input, 1.25);
    }

    /// Why: an unknown model must be `None`, not a fabricated rate.
    /// Test: itself.
    #[test]
    fn unknown_model_is_none() {
        assert!(pricing("cohere/command-r-plus").is_none());
    }

    /// Why: the estimate math must be a plain per-million weighted sum.
    /// Test: itself.
    #[test]
    fn estimate_matches_hand_calc() {
        let p = Pricing {
            input: 3.0,
            output: 15.0,
            cache_read: 0.30,
            cache_write: 3.75,
        };
        // 1M input @3 + 1M output @15 + 1M cr @0.30 + 1M cw @3.75 = 22.05
        let cost = p.estimate_usd(1_000_000, 1_000_000, 1_000_000, 1_000_000);
        assert!((cost - 22.05).abs() < 1e-9, "cost was {cost}");
    }
}
