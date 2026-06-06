//! Cost computation, model pricing table, and small formatting helpers.
//!
//! Why: The pricing table + cost math is mechanically independent of the
//! collector's lifecycle; isolating it keeps `mod.rs` focused on recording
//! and persisting performance records.
//! What: `cost_usd` (public), the `pricing_for`/`per_million` rate helpers, and
//! the `filename_stamp` / `truncate_preview` formatters used by the collector.
//! Test: `cost_usd_*` / `filename_stamp_format` / `truncate_preview_*` in
//! `perf::tests`.

/// Compute USD cost for a single LLM call given the model name and token
/// counts. Unknown models fall back to Sonnet-class pricing.
///
/// Why: (#47) We hard-code the pricing table rather than hit a live endpoint
/// so offline/CI runs still produce comparable cost figures. Pricing is
/// per-million-tokens as published by Anthropic and OpenRouter.
/// What: Substring-matches the model string (e.g. "anthropic/claude-sonnet-4-5"
/// or "claude-haiku-4") and multiplies each token bucket by its rate.
/// Test: `cost_usd_known_model`, `cost_usd_unknown_defaults_to_sonnet`.
pub fn cost_usd(
    model: &str,
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read: u32,
    cache_creation: u32,
) -> f64 {
    // Rates in USD per token (not per million).
    let (rate_in, rate_out, rate_cache_r, rate_cache_w) = pricing_for(model);
    let to_usd = |tokens: u32, rate: f64| tokens as f64 * rate;
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

/// Build the canonical filename stamp from an ISO8601 timestamp + build #.
///
/// Why: Deterministic filenames let tests assert exact paths and let humans
/// sort runs chronologically with `ls`.
/// What: Converts `2026-04-22T17:31:30Z` + build=42 to `20260422-173130-build42`.
/// Test: `filename_stamp_format`.
pub(super) fn filename_stamp(iso: &str, build: u64) -> String {
    // Strip non-digit chars to keep only YYYYMMDDHHMMSS, then reinsert the dash.
    let digits: String = iso.chars().filter(|c| c.is_ascii_digit()).collect();
    // Expected layout: YYYYMMDDHHMMSS (14 digits). If shorter, just pad.
    let date = digits.get(0..8).unwrap_or("00000000");
    let time = digits.get(8..14).unwrap_or("000000");
    format!("{date}-{time}-build{build}")
}

pub(super) fn truncate_preview(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out = String::with_capacity(max_chars + 3);
    for (i, ch) in trimmed.chars().enumerate() {
        if i >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}
