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
///
/// Why: `RunReport.cost_usd` is compared against Claude Code's own
/// `total_cost_usd` in the #8127 bake-off, so the Claude 5 tiers need their
/// real published rates rather than the 4.x substring fallback they used to
/// land on (#8128). Cache rates are derived from the published input rate by
/// Anthropic's documented multipliers — a read is 0.1x input and a 5-minute
/// write is 1.25x input.
/// What: An ordered table. The explicitly-labelled Claude 5 / Haiku 4.5 rows
/// come FIRST because their slugs also contain the older rows' substrings
/// (`claude-haiku-4.5` contains `haiku-4`), so a later row would shadow them.
/// Everything below them is the pre-#8128 4.x table, unchanged.
/// Test: `perf::tests::cost_usd_opus_5_matches_published_rates`,
/// `perf::tests::cost_usd_sonnet_5_matches_published_rates`,
/// `perf::tests::cost_usd_haiku_4_5_matches_published_rates`,
/// `perf::tests::cost_usd_legacy_4x_rows_are_unchanged`.
// #8128: list prices from the `claude-api` skill's "Current Models" table
// (cached 2026-06-24); cache multipliers from its `shared/prompt-caching.md`
// § Economics (read 0.1x input, 5-minute write 1.25x input).
fn pricing_for(model: &str) -> (f64, f64, f64, f64) {
    let m = model.to_ascii_lowercase();
    // Claude Opus 5 — $5 in, $25 out, $0.50 cache read, $6.25 cache write
    if m.contains("opus-5") {
        return (
            per_million(5.0),
            per_million(25.0),
            per_million(0.50),
            per_million(6.25),
        );
    }
    // Claude Sonnet 5 — $2 in, $10 out, $0.20 cache read, $2.50 cache write
    if m.contains("sonnet-5") {
        return (
            per_million(2.0),
            per_million(10.0),
            per_million(0.20),
            per_million(2.50),
        );
    }
    // Claude Haiku 4.5 — $1 in, $5 out, $0.10 cache read, $1.25 cache write.
    // Both spellings: the OpenRouter slug (`claude-haiku-4.5`) and the
    // Anthropic/Bedrock one (`claude-haiku-4-5`).
    if m.contains("haiku-4.5") || m.contains("haiku-4-5") {
        return (
            per_million(1.0),
            per_million(5.0),
            per_million(0.10),
            per_million(1.25),
        );
    }
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
