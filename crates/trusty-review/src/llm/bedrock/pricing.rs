//! Bedrock pricing table and cost estimation.
//!
//! Why: extracted from `bedrock/mod.rs` to keep file sizes under the 500-line
//! cap while keeping pricing logic independently testable.
//! What: `bedrock_cost_per_million`, `normalize_model_family`, and
//! `estimate_bedrock_cost_usd` — the full pricing lookup chain.
//! Test: `bedrock_cost_estimate_*` and `bedrock_normalize_model_family_*`
//! tests live in `bedrock/mod.rs` test module.

use tracing::debug;

use super::INFERENCE_PROFILE_PREFIXES;

/// Approximate Bedrock pricing per million tokens (input, output) in USD.
///
/// Why: surfaces cost estimates in [`LlmResponse`] for the `compare` mode.
/// What: keyed by model id (inference-profile id without the `us.`/`eu.` prefix
/// since the per-token cost is the same across regions for the same model).
/// After stripping the geo-prefix the lookup also normalises date/version
/// suffixes (e.g. `-20251001-v1:0`) via [`normalize_model_family`] so that
/// date-stamped ids like `anthropic.claude-haiku-4-5-20251001-v1:0` match the
/// same table entry as the family prefix `anthropic.claude-haiku-4-5`.
/// Unknown ids → cost 0.0 with a debug log.
///
/// Sources: AWS Bedrock On-Demand pricing page (us-east-1, June 2026).
pub(super) fn bedrock_cost_per_million(model: &str) -> (f64, f64) {
    // Step 1: strip the geography prefix (us., eu., ap., jp., global.) so
    // the pricing table works for all cross-region inference profiles.
    let after_geo = INFERENCE_PROFILE_PREFIXES
        .iter()
        .find_map(|pfx| model.strip_prefix(pfx))
        .unwrap_or(model);

    // Step 2: normalise by stripping any date/version suffix of the form
    // `-YYYYMMDD-vN:N` so that date-stamped ids (e.g. the verified Haiku 4.5
    // id `anthropic.claude-haiku-4-5-20251001-v1:0`) match the same table
    // entry as the short family prefix (`anthropic.claude-haiku-4-5`).
    let normalized = normalize_model_family(after_geo);

    match normalized {
        // Claude Sonnet 4.6 — default reviewer model.
        "anthropic.claude-sonnet-4-6" => (3.00, 15.00),
        // Claude Sonnet 4.5 — second tier in compare set.
        // Confirmed-available in target account; pricing ≈ Sonnet 4.6.
        "anthropic.claude-sonnet-4-5" => (3.00, 15.00),
        // Claude Haiku 4.5 — default verifier/summarizer model.
        // Matches both `anthropic.claude-haiku-4-5` and the date-versioned
        // `anthropic.claude-haiku-4-5-20251001-v1:0` after normalization.
        "anthropic.claude-haiku-4-5" => (0.80, 4.00),
        // Claude Opus 4.8 — premium option.
        "anthropic.claude-opus-4-8" => (15.00, 75.00),
        // Legacy Claude 3.5 Sonnet (cross-region profile, date-versioned).
        "anthropic.claude-3-5-sonnet-20241022-v2:0" | "anthropic.claude-3-5-sonnet" => {
            (3.00, 15.00)
        }
        // Legacy Claude 3 Haiku (cross-region profile, date-versioned).
        "anthropic.claude-3-haiku-20240307-v1:0" | "anthropic.claude-3-haiku" => (0.25, 1.25),
        // Unknown model — no cost estimate.
        _ => {
            debug!(
                model = %model,
                "BedrockProvider: no pricing entry for model id — cost_usd will be 0.0"
            );
            (0.0, 0.0)
        }
    }
}

/// Normalise a model id by stripping date/version suffixes of the form
/// `-YYYYMMDD-vN:N` (e.g. `-20251001-v1:0`) so date-stamped Bedrock ids
/// match the short family-prefix entry in the pricing table.
///
/// Why: Bedrock uses date-versioned inference-profile ids (e.g.
/// `anthropic.claude-haiku-4-5-20251001-v1:0`) that would not match a
/// short key like `anthropic.claude-haiku-4-5` without normalization,
/// causing the pricing lookup to return (0.0, 0.0).
/// What: scans from the end of the string for a `-vN:N` version tag
/// (optionally preceded by `-YYYYMMDD`) and strips everything from the
/// first date segment onwards.  Returns the input unchanged if no suffix
/// is found.
/// Test: `bedrock_normalize_model_family_strips_suffix`,
/// `bedrock_cost_estimate_haiku_date_versioned`.
pub fn normalize_model_family(model: &str) -> &str {
    // Look for a `-vN:N` or `-vN` suffix, optionally preceded by a date segment
    // `-YYYYMMDD`.  Walk backwards through '-'-delimited segments.
    // Strategy: find the first '-'-separated segment that looks like a date
    // (8 digits) and strip from there.  If no date segment, look for a version
    // segment (`v` followed by digits/colons) and strip from there.
    let mut end = model.len();

    // Walk segments from the right.
    while let Some(dash_pos) = model[..end].rfind('-') {
        let segment = &model[dash_pos + 1..end];

        let is_date_segment = segment.len() == 8 && segment.bytes().all(|b| b.is_ascii_digit());
        let is_version_segment = segment.starts_with('v')
            && segment[1..]
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b':');

        if is_date_segment || is_version_segment {
            end = dash_pos;
            // Keep stripping — a date segment may be followed by a version segment.
        } else {
            break;
        }
    }

    &model[..end]
}

/// Compute USD cost estimate from token counts and model id.
///
/// Why: surfaces per-call cost in `compare` mode so operators can rank models
/// by cost-efficiency.
/// What: applies `bedrock_cost_per_million`; returns 0.0 for unknown models.
/// Test: `bedrock_cost_estimate_sonnet`, `bedrock_cost_estimate_unknown_model`.
pub fn estimate_bedrock_cost_usd(model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
    let (in_price, out_price) = bedrock_cost_per_million(model);
    (input_tokens as f64 / 1_000_000.0) * in_price
        + (output_tokens as f64 / 1_000_000.0) * out_price
}
