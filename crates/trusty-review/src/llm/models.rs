//! GPT-5-family model identifier constants.
//!
//! Why: having model ids in one place makes it trivial to audit and update
//! the model set without grepping across the codebase; it also documents the
//! intent of the MVP (test GPT-5-class models, not earlier generations).
//!
//! What: defines the built-in default model ids for all three roles, plus a
//! compare-set of GPT-5 candidate ids the `compare` subcommand uses.
//!
//! IMPORTANT — model id accuracy: these ids are OpenRouter model slugs as of
//! June 2026.  OpenRouter slugs change when providers release new variants.
//! If a model is unavailable, set the override via:
//!   - `TRUSTY_REVIEW_REVIEWER_MODEL`, `TRUSTY_REVIEW_VERIFIER_MODEL`,
//!     `TRUSTY_REVIEW_SUMMARIZER_MODEL` environment variables, OR
//!   - the `[models]` table in `~/.config/trusty-review/config.toml`.
//!
//! Test: `model_ids_are_openrouter_slugs` checks that the default ids contain
//! a `/` (the OpenRouter `provider/model-name` format) and start with `openai/`.

// ─── Default model ids ────────────────────────────────────────────────────────

/// Default model for the reviewer role (main review pass).
///
/// Why: reviewer calls are the most expensive in the pipeline; we want the
/// most capable cheap-tier GPT-5 variant.
/// What: `openai/gpt-5-mini` is the cost-effective GPT-5-class choice on
/// OpenRouter.  Override via `TRUSTY_REVIEW_REVIEWER_MODEL`.
///
/// NOTE: if OpenRouter has renamed this slug, update here and in your
/// config.toml.  Run `trusty-review compare` to validate quality vs cost.
pub const DEFAULT_REVIEWER_MODEL: &str = "openai/gpt-5-mini";

/// Default model for the verifier role (per-finding verification round).
///
/// Why: verifier calls are short (single-word output) and high-volume; the
/// cheapest GPT-5 variant keeps latency and cost low while preserving the
/// quality bar.
/// What: `openai/gpt-5-nano` on OpenRouter.
/// Override via `TRUSTY_REVIEW_VERIFIER_MODEL`.
///
/// CRITICAL: the verifier model MUST be a foundation-lifecycle ACTIVE model
/// (spec REV-340).  If this slug is inactive, every finding will be silently
/// refuted and every review will APPROVE — the same failure mode that broke
/// production (source-analysis §12.1).
pub const DEFAULT_VERIFIER_MODEL: &str = "openai/gpt-5-nano";

/// Default model for the summarizer role (diff Stage-C classification).
///
/// Why: summarizer calls are deterministic (temperature 0) and low-stakes;
/// the cheapest GPT-5 variant is appropriate.
/// What: `openai/gpt-5-nano` on OpenRouter.
/// Override via `TRUSTY_REVIEW_SUMMARIZER_MODEL`.
pub const DEFAULT_SUMMARIZER_MODEL: &str = "openai/gpt-5-nano";

// ─── Compare-set ─────────────────────────────────────────────────────────────

/// Candidate GPT-5-class model ids for the `compare` subcommand.
///
/// Why: the Stage-3 `compare` mode runs the same PR through multiple reviewer
/// models and ranks them by quality/speed/cost.  This set seeds the default
/// candidate list so operators don't have to look up OpenRouter slugs.
/// What: a static slice of OpenRouter GPT-5-family slugs.  The Stage-3 CLI
/// will use this as the default `--models` list when no explicit list is
/// provided.
///
/// IMPORTANT: update these slugs if OpenRouter renames them.
pub const COMPARE_CANDIDATE_MODELS: &[&str] =
    &["openai/gpt-5", "openai/gpt-5-mini", "openai/gpt-5-nano"];

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
    fn compare_set_is_gpt5_family() {
        for id in COMPARE_CANDIDATE_MODELS {
            assert!(
                id.contains("gpt-5"),
                "compare candidate {id:?} must be a gpt-5 model"
            );
        }
    }

    #[test]
    fn defaults_are_in_compare_set_or_documented() {
        // The reviewer default should be in the compare set so the operator
        // can see how it stacks up against the full GPT-5 model.
        assert!(
            COMPARE_CANDIDATE_MODELS.contains(&DEFAULT_REVIEWER_MODEL),
            "DEFAULT_REVIEWER_MODEL {DEFAULT_REVIEWER_MODEL:?} should be in COMPARE_CANDIDATE_MODELS"
        );
    }
}
