//! Bedrock pricing table and cost estimation.
//!
//! Why: extracted from `bedrock/mod.rs` to keep file sizes under the 500-line
//! cap while keeping pricing logic independently testable.
//! What: `bedrock_cost_per_million`, `normalize_model_family`, and
//! `estimate_bedrock_cost_usd` — the full pricing lookup chain.
//! Test: `bedrock_cost_estimate_*` and `bedrock_normalize_model_family_*`
//! tests live in `bedrock/tests.rs`.

use tracing::debug;

use super::INFERENCE_PROFILE_PREFIXES;

/// The inference-profile prefix billed at the global on-demand rate.
const GLOBAL_PROFILE_PREFIX: &str = "global.";

/// On-demand rates for one model family, in USD per million tokens, as
/// `(input, output)` pairs.
struct FamilyRates {
    /// Rate for a `global.` cross-region inference profile.
    global: (f64, f64),
    /// Rate for a geographic profile (`us.`, `eu.`, `ap.`, `jp.`) or a bare id.
    geo: (f64, f64),
}

impl FamilyRates {
    /// One rate for both profile kinds (legacy models with no global premium).
    const fn flat(input: f64, output: f64) -> Self {
        Self {
            global: (input, output),
            geo: (input, output),
        }
    }
}

/// Bedrock on-demand pricing per million tokens (input, output) in USD.
///
/// Why: surfaces cost estimates in [`LlmResponse`] for `run` and `compare`.
/// AWS bills a geographic inference profile (`us.` and the other geo prefixes)
/// at the "Regional" rate, 10% above the `global.` rate for Claude 4.5 and
/// later, so the prefix must pick the rate before it is stripped.
/// What: reads whether `model` carries the `global.` prefix, strips any
/// inference-profile prefix, normalises date/version suffixes via
/// [`normalize_model_family`], and returns the matching family's global or
/// geo rate. Unknown ids → `(0.0, 0.0)` with a debug log.
/// Test: `bedrock_cost_estimate_matches_price_list_per_profile`,
/// `bedrock_cost_estimate_haiku_date_versioned`.
pub(super) fn bedrock_cost_per_million(model: &str) -> (f64, f64) {
    let is_global = model.starts_with(GLOBAL_PROFILE_PREFIX);
    let after_geo = INFERENCE_PROFILE_PREFIXES
        .iter()
        .find_map(|pfx| model.strip_prefix(pfx))
        .unwrap_or(model);

    match family_rates(normalize_model_family(after_geo)) {
        Some(rates) if is_global => rates.global,
        Some(rates) => rates.geo,
        None => {
            debug!(
                model = %model,
                "BedrockProvider: no pricing entry for model id — cost_usd will be 0.0"
            );
            (0.0, 0.0)
        }
    }
}

/// Price table keyed by normalised model family (no profile prefix, no
/// date/version suffix).
///
/// Source for every entry: AWS Price List API, service code
/// `AmazonBedrockFoundationModels`, region `us-east-1`, retrieved 2026-10-05.
/// Global rates are the `*_Global` / `*_global_standard` usage types; geo rates
/// are the regional `InputTokenCount` / `input_tokens_standard` usage types.
/// SKUs are listed as input-global, output-global / input-geo, output-geo.
fn family_rates(family: &str) -> Option<FamilyRates> {
    let rates = match family {
        // Claude Sonnet 5.5 — default reviewer (Bob 2026-10-05). Effective
        // 2026-09-01. SKUs PXMCKMF8EGSRB3GB, HP32J6RXJZ3XYFUM /
        // 6TG78WT6WYJUVS72, ZNY2E4B6MTPHYXVH.
        "anthropic.claude-sonnet-5-5" => FamilyRates {
            global: (2.00, 10.00),
            geo: (2.20, 11.00),
        },
        // Claude Opus 5.5 — compare set. Effective 2026-09-01. SKUs
        // KVG5FBPDJPKF5TJY, MWH4TD2A4D5CEBAP / J9QFZT8WAQABG9ZX, FCRGDQ596BG7EQKH.
        "anthropic.claude-opus-5-5" => FamilyRates {
            global: (4.00, 20.00),
            geo: (4.40, 22.00),
        },
        // Claude Sonnet 4.6 — compare set. Effective 2026-09-01. SKUs
        // 4BCSX42248MW35W4, CMWTKXQCAJ2T8A4U / KPFGPZJ8VWMREGAV, GMB9AQ5DYNAGYZEC.
        "anthropic.claude-sonnet-4-6" => FamilyRates {
            global: (3.00, 15.00),
            geo: (3.30, 16.50),
        },
        // Claude Sonnet 4.5. Effective 2026-09-01. SKUs GJ7NHAXUYKGEHMVV,
        // W4BVSJCZM5M9FZYT / SSK4DT4JBRUQJYP5, 3YN5KGC2HJU9VC5A.
        "anthropic.claude-sonnet-4-5" => FamilyRates {
            global: (3.00, 15.00),
            geo: (3.30, 16.50),
        },
        // Claude Haiku 4.5 — default verifier/summarizer; matches the
        // date-versioned id after normalisation. Effective 2026-09-01. SKUs
        // DY4B4Q3TRTAY8V6G, XADKFYXVGBFBJSU7 / JQDUC8Q4K8C6GSGH, X629GDA2GXAP6R54.
        "anthropic.claude-haiku-4-5" => FamilyRates {
            global: (1.00, 5.00),
            geo: (1.10, 5.50),
        },
        // Claude Opus 4.8. Effective 2026-09-01. SKUs UF2KA578YZK6T9UE,
        // DWUA6RYSEUDBTPA4 / 4AVHTD2NXFKSU6HU, YKJ5FPMCZAQF5BHF.
        "anthropic.claude-opus-4-8" => FamilyRates {
            global: (5.00, 25.00),
            geo: (5.50, 27.50),
        },
        // Legacy Claude 3.5 Sonnet v2 (one rate, no global profile).
        // Effective 2026-07-01. SKUs G9H8GZD6JNDG8SYS, XGQXAJ2EEBAXERX5.
        "anthropic.claude-3-5-sonnet" => FamilyRates::flat(3.00, 15.00),
        // Legacy Claude 3 Haiku (one rate, no global profile).
        // Effective 2026-07-01. SKUs XQ9Q5XG2HVDMTF2S, NNM89KRFHHY9RWAT.
        "anthropic.claude-3-haiku" => FamilyRates::flat(0.25, 1.25),
        _ => return None,
    };
    Some(rates)
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
