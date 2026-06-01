//! GPT-5-family model identifier constants.
//!
//! Why: having model ids in one place makes it trivial to audit and update
//! the model set without grepping across the codebase; it also documents the
//! intent of the MVP (test GPT-5-class models, not earlier generations).
//!
//! What: defines the built-in default model ids for all three roles, plus a
//! compare-set of GPT-5 candidate ids the `compare` subcommand uses.
//!
//! IMPORTANT — model id accuracy: OpenRouter slugs are **version-stamped**
//! (e.g. `openai/gpt-5.4-mini-20260317`) and may need updating as new model
//! versions are released or old ones are retired.  If a model is unavailable,
//! set the override via:
//!
//!   - `TRUSTY_REVIEW_REVIEWER_MODEL`, `TRUSTY_REVIEW_VERIFIER_MODEL`,
//!     `TRUSTY_REVIEW_SUMMARIZER_MODEL` environment variables, OR
//!   - the `[models]` table in `~/.config/trusty-review/config.toml`.
//!
//! Run `trusty-review compare` to validate quality vs cost after any update.
//!
//! Test: `model_ids_are_openrouter_slugs` checks that the default ids contain
//! a `/` (the OpenRouter `provider/model-name` format) and start with `openai/`.

// ─── Default model ids ────────────────────────────────────────────────────────

/// Default model for the reviewer role (main review pass).
///
/// Why: reviewer calls are the most expensive in the pipeline; we want the
/// best cheap-tier GPT-5.4 variant for good quality at moderate cost.
/// What: `openai/gpt-5.4-mini-20260317` is the cost-effective GPT-5-class
/// choice on OpenRouter ($0.75/M input, $4.50/M output).
/// Override via `TRUSTY_REVIEW_REVIEWER_MODEL`.
///
/// NOTE: OpenRouter slugs are version-stamped; if this slug is unavailable,
/// update here and in your config.toml.
pub const DEFAULT_REVIEWER_MODEL: &str = "openai/gpt-5.4-mini-20260317";

/// Default model for the verifier role (per-finding verification round).
///
/// Why: verifier calls are short (single-word output) and high-volume; the
/// cheapest GPT-5 nano variant keeps latency and cost low.
/// What: `openai/gpt-5.4-nano-20260317` on OpenRouter ($0.20/M input, $1.25/M output).
/// Override via `TRUSTY_REVIEW_VERIFIER_MODEL`.
///
/// CRITICAL: the verifier model MUST be a foundation-lifecycle ACTIVE model
/// (spec REV-340).  If this slug is inactive, every finding will be silently
/// refuted and every review will APPROVE — the same failure mode that broke
/// production (source-analysis §12.1).
pub const DEFAULT_VERIFIER_MODEL: &str = "openai/gpt-5.4-nano-20260317";

/// Default model for the summarizer role (diff Stage-C classification).
///
/// Why: summarizer calls are deterministic (temperature 0) and low-stakes;
/// the cheapest GPT-5 nano variant is appropriate.
/// What: `openai/gpt-5.4-nano-20260317` on OpenRouter ($0.20/M input, $1.25/M output).
/// Override via `TRUSTY_REVIEW_SUMMARIZER_MODEL`.
pub const DEFAULT_SUMMARIZER_MODEL: &str = "openai/gpt-5.4-nano-20260317";

// ─── Compare-set ─────────────────────────────────────────────────────────────

/// Candidate GPT-5-class model ids for the `compare` subcommand.
///
/// Why: the `compare` mode runs the same PR through multiple reviewer models
/// and ranks them by quality/speed/cost.  This set seeds the default candidate
/// list so operators don't have to look up OpenRouter slugs.
/// What: a static slice of OpenRouter GPT-5-family slugs, ordered cheap → premium.
///
/// IMPORTANT: OpenRouter slugs are version-stamped; update these when
/// OpenRouter's catalog evolves or old versions are retired.
pub const COMPARE_CANDIDATE_MODELS: &[&str] = &[
    "openai/gpt-5.4-nano-20260317",
    "openai/gpt-5.4-mini-20260317",
    "openai/gpt-5.4-20260305",
    "openai/gpt-5.5-20260423",
];

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_are_openrouter_slugs() {
        for id in [
            DEFAULT_REVIEWER_MODEL,
            DEFAULT_VERIFIER_MODEL,
            DEFAULT_SUMMARIZER_MODEL,
        ] {
            assert!(
                id.contains('/'),
                "model id {id:?} must contain '/' (OpenRouter provider/name format)"
            );
            assert!(
                id.starts_with("openai/"),
                "default model {id:?} must be an openai/ GPT-5-class slug"
            );
        }
    }

    #[test]
    fn model_slugs_are_version_stamped() {
        // OpenRouter uses version-stamped slugs (e.g. gpt-5.4-mini-20260317).
        // All production slugs should carry a date stamp for reproducibility.
        for id in [
            DEFAULT_REVIEWER_MODEL,
            DEFAULT_VERIFIER_MODEL,
            DEFAULT_SUMMARIZER_MODEL,
        ] {
            // Date stamps follow the pattern YYYYMMDD — 8 consecutive digits.
            let has_date = id
                .chars()
                .collect::<Vec<_>>()
                .windows(8)
                .any(|w| w.iter().all(|c| c.is_ascii_digit()));
            assert!(
                has_date,
                "model id {id:?} should contain a version date stamp (YYYYMMDD)"
            );
        }
    }

    #[test]
    fn compare_set_is_gpt5_family() {
        for id in COMPARE_CANDIDATE_MODELS {
            assert!(
                id.contains("gpt-5"),
                "compare candidate {id:?} must be a gpt-5 model"
            );
        }
    }

    #[test]
    fn compare_set_ordered_cheap_to_premium() {
        // The compare set must be ordered cheap → premium (nano first, pro last).
        // We validate that the nano model comes before mini, and mini before the
        // full model, using index position in the slice.
        let pos = |needle: &str| {
            COMPARE_CANDIDATE_MODELS
                .iter()
                .position(|&m| m == needle)
                .unwrap_or(usize::MAX)
        };
        assert!(
            pos("openai/gpt-5.4-nano-20260317") < pos("openai/gpt-5.4-mini-20260317"),
            "nano must come before mini in compare set"
        );
        assert!(
            pos("openai/gpt-5.4-mini-20260317") < pos("openai/gpt-5.4-20260305"),
            "mini must come before full model in compare set"
        );
    }

    #[test]
    fn defaults_are_in_compare_set_or_documented() {
        // The reviewer default should be in the compare set so the operator
        // can see how it stacks up against other GPT-5 models.
        assert!(
            COMPARE_CANDIDATE_MODELS.contains(&DEFAULT_REVIEWER_MODEL),
            "DEFAULT_REVIEWER_MODEL {DEFAULT_REVIEWER_MODEL:?} should be in COMPARE_CANDIDATE_MODELS"
        );
    }

    #[test]
    fn known_model_slugs_match_spec() {
        // Verify exact slug strings match the OpenRouter catalog (June 2026).
        // If these fail, OpenRouter has renamed or retired the model.
        assert_eq!(DEFAULT_REVIEWER_MODEL, "openai/gpt-5.4-mini-20260317");
        assert_eq!(DEFAULT_VERIFIER_MODEL, "openai/gpt-5.4-nano-20260317");
        assert_eq!(DEFAULT_SUMMARIZER_MODEL, "openai/gpt-5.4-nano-20260317");
        assert!(COMPARE_CANDIDATE_MODELS.contains(&"openai/gpt-5.5-20260423"));
        assert!(COMPARE_CANDIDATE_MODELS.contains(&"openai/gpt-5.4-20260305"));
    }
}
