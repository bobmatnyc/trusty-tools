//! Model identifier constants for trusty-review.
//!
//! Why: having model ids in one place makes it trivial to audit and update
//! the model set without grepping across the codebase; it also documents the
//! intent of the default configuration and the compare-set candidates.
//!
//! What: defines the built-in default model ids for all three roles (now
//! Bedrock-first), plus a Bedrock-only compare-set (Haiku 4.5 → Sonnet 4.6 →
//! Sonnet 5.5 → Opus 5.5).
//!
//! #5971 demoted these constants one rung: the built-in model default is now a
//! `trusty_common::inference::ModelTier` lookup against the resolved provider,
//! and a constant here applies only when that (tier, provider) pair has no
//! verified id. Which is why the reviewer default below still describes
//! Bedrock: the tier layer resolves Opus 4.8 on OpenRouter and the Anthropic
//! first-party API, but Bedrock's `Analysis` tier is unmapped, so a standalone
//! Bedrock run falls through to Sonnet 5.5 here.
//!
//! DEFAULT PROVIDER: Bedrock (effective as of this file's introduction).
//!   - Reviewer:   `us.anthropic.claude-sonnet-5-5`              (verified)
//!   - Verifier:   `us.anthropic.claude-haiku-4-5-20251001-v1:0` (verified)
//!   - Summarizer: `us.anthropic.claude-haiku-4-5-20251001-v1:0` (verified)
//!
//! Model-id verification status:
//!   - `us.anthropic.claude-sonnet-5-5`, `us.anthropic.claude-opus-5-5` — one
//!     Converse call each succeeded on 2026-10-05 (us-east-1).
//!   - `us.anthropic.claude-sonnet-4-6`                  — confirmed in CLAUDE.md.
//!   - `us.anthropic.claude-haiku-4-5-20251001-v1:0`     — verified against live
//!     Bedrock account (replaces the incorrect `us.anthropic.claude-haiku-4-5`
//!     which produced HTTP 400 ValidationException).
//!
//! OpenRouter remains fully available for all roles; select it with:
//!   - `--provider openrouter` CLI flag, or
//!   - `TRUSTY_REVIEW_PROVIDER=openrouter` env var, or
//!   - `provider = "openrouter"` in the config file, or
//!   - an `openrouter/<model-id>` prefix on the model slug.
//!
//! Fireworks.ai is likewise available for all roles (requires
//! `FIREWORKS_API_KEY`); select it with `--provider fireworks`,
//! `TRUSTY_REVIEW_PROVIDER=fireworks`, `provider = "fireworks"` in the config
//! file, or a `fireworks/<model-id>` prefix on the model slug, e.g.
//! `fireworks/accounts/fireworks/models/kimi-k2p6` (the bare id after the
//! prefix is the provider-native `accounts/fireworks/models/*` form; list
//! currently-deployed serverless models via `GET /inference/v1/models`).
//!
//! Test: `bedrock_defaults_have_inference_profile_prefix`,
//! `compare_set_models_are_bedrock_only`,
//! `haiku_default_has_correct_date_versioned_id`,
//! `compare_set_contains_the_5_5_models`.

// ─── Default Bedrock model ids ────────────────────────────────────────────────

/// Default model for the reviewer role (main review pass) — Bedrock Sonnet 5.5.
///
/// Why: the reviewer role makes the highest-quality call in the pipeline; the
/// owner chose Sonnet 5.5 for it (Bob, 2026-10-05).  Bedrock is the default
/// because it uses IAM auth (no API key), integrates with AWS secrets
/// management, and keeps data within the operator's VPC.
/// What: `us.anthropic.claude-sonnet-5-5` is the Claude Sonnet 5.5 cross-region
/// inference profile for the US geography, with no date stamp or `-v1:0`
/// suffix.  Since #5971 this is reached only when `ModelTier::Analysis`
/// declines for the resolved provider — in practice, Bedrock.  On OpenRouter
/// the reviewer resolves Opus 4.8 instead.
/// Override via `TRUSTY_REVIEW_REVIEWER_MODEL`.
/// Test: `role_models_bedrock_reviewer_keeps_sonnet_default`.
// Bob 2026-10-05: Sonnet 5.5 default
pub const DEFAULT_REVIEWER_MODEL: &str = "us.anthropic.claude-sonnet-5-5";

/// Default model for the verifier role (per-finding verification round) — Bedrock Amazon Nova Pro.
///
/// Why: the verifier checks the reviewer's findings, and the reviewer default
/// is Claude; a verifier from another model family does not share the
/// reviewer's blind spots (#11527, owner ruling D52). Verifier calls are short
/// and high-volume, so a mid-priced model fits.
/// What: `us.amazon.nova-pro-v1:0` is the US cross-region inference profile for
/// Nova Pro (`aws bedrock get-inference-profile` reported ACTIVE in us-east-1 on
/// 2026-11-19). Until #11527 this was Haiku 4.5.
/// Override via `TRUSTY_REVIEW_VERIFIER_MODEL`.
///
/// CRITICAL: the verifier model MUST be a foundation-lifecycle ACTIVE model
/// (spec REV-340).  If this slug is inactive, every finding will be silently
/// refuted and every review will APPROVE.
pub const DEFAULT_VERIFIER_MODEL: &str = "us.amazon.nova-pro-v1:0";

/// Verifier used when the built-in default is denied at startup (#11527).
///
/// Why: an account without Nova Pro access would otherwise refuse live mode
/// after upgrading; the prior default (Haiku 4.5) keeps it running.
/// What: the pre-#11527 verifier default. Only the liveness gate reads it, and
/// only for a verifier left at the built-in default.
/// Test: `pipeline::verify_liveness::tests::enforce_default_access_denied_falls_back`.
pub const FALLBACK_VERIFIER_MODEL: &str = "us.anthropic.claude-haiku-4-5-20251001-v1:0";

/// Default verifier model when the verifier resolves to OpenRouter (#11527).
///
/// Why: OpenRouter's reviewer default is Claude Opus; the verifier takes a
/// different family for the same reason as [`DEFAULT_VERIFIER_MODEL`].
/// What: Google Gemini 2.5 Flash, which supports the strict `json_schema`
/// structured output the verifier request forces.
pub const OPENROUTER_VERIFIER_MODEL: &str = "google/gemini-2.5-flash";

/// Default model for the summarizer role (diff Stage-C classification) — Bedrock Haiku 4.5.
///
/// Why: summarizer calls are deterministic (temperature 0) and low-stakes;
/// the cheapest Haiku-tier Bedrock model is appropriate.
/// What: same as `DEFAULT_VERIFIER_MODEL` — `us.anthropic.claude-haiku-4-5-20251001-v1:0`.
/// Override via `TRUSTY_REVIEW_SUMMARIZER_MODEL`.
pub const DEFAULT_SUMMARIZER_MODEL: &str = "us.anthropic.claude-haiku-4-5-20251001-v1:0";

// ─── Compare-set ─────────────────────────────────────────────────────────────

/// Candidate model ids for the `compare` subcommand.
///
/// Why: the `compare` mode runs the same PR through multiple reviewer models
/// and ranks them by quality/speed/cost.  This default set is Bedrock-only so
/// it works out of the box without an OpenRouter API key, and uses only
/// confirmed-available ids from the target account.
/// What: a static slice of `bedrock/`-prefixed model ids ordered by tier
/// (Haiku, Sonnet, Opus), then by generation within a tier.  The `compare`
/// subcommand resolves the provider per-entry via `resolve_provider_and_model`,
/// strips the prefix, and sends the bare id to the Bedrock Converse API.
/// Override with `--models` to add OpenRouter or other providers.
///
/// Ids and their `us.` on-demand $/MTok (input/output, see `bedrock/pricing.rs`):
///   - Haiku 4.5:  `us.anthropic.claude-haiku-4-5-20251001-v1:0` (1.10/5.50)
///   - Sonnet 4.6: `us.anthropic.claude-sonnet-4-6`              (3.30/16.50)
///   - Sonnet 5.5: `us.anthropic.claude-sonnet-5-5`              (2.20/11.00, reviewer default)
///   - Opus 5.5:   `us.anthropic.claude-opus-5-5`                (4.40/22.00)
pub const COMPARE_CANDIDATE_MODELS: &[&str] = &[
    // Bedrock Haiku 4.5 — verifier/summarizer default.
    // date-versioned id required by Bedrock (short form produces HTTP 400).
    "bedrock/us.anthropic.claude-haiku-4-5-20251001-v1:0",
    // Bedrock Sonnet 4.6 — the previous reviewer default.
    "bedrock/us.anthropic.claude-sonnet-4-6",
    // Bedrock Sonnet 5.5 — reviewer default (no date stamp needed).
    "bedrock/us.anthropic.claude-sonnet-5-5",
    // Bedrock Opus 5.5 — premium tier (no date stamp needed).
    "bedrock/us.anthropic.claude-opus-5-5",
];

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bedrock_defaults_have_inference_profile_prefix() {
        // All default Bedrock ids must carry an inference-profile prefix.
        let prefixes = ["us.", "eu.", "ap.", "jp.", "global."];
        for id in [
            DEFAULT_REVIEWER_MODEL,
            DEFAULT_VERIFIER_MODEL,
            DEFAULT_SUMMARIZER_MODEL,
        ] {
            let has_prefix = prefixes.iter().any(|p| id.starts_with(p));
            assert!(
                has_prefix,
                "default model {id:?} must start with a cross-region inference-profile prefix"
            );
        }
    }

    #[test]
    fn default_reviewer_is_sonnet() {
        assert!(
            DEFAULT_REVIEWER_MODEL.contains("sonnet"),
            "reviewer default should be Sonnet-tier: {DEFAULT_REVIEWER_MODEL}"
        );
    }

    #[test]
    fn default_summarizer_is_haiku_and_verifier_is_not_claude() {
        assert!(
            DEFAULT_SUMMARIZER_MODEL.contains("haiku"),
            "summarizer default should be Haiku-tier: {DEFAULT_SUMMARIZER_MODEL}"
        );
        // #11527: the verifier default is a different family from the reviewer.
        for id in [DEFAULT_VERIFIER_MODEL, OPENROUTER_VERIFIER_MODEL] {
            assert!(
                !id.contains("anthropic") && !id.contains("claude"),
                "verifier default must not be Claude: {id}"
            );
        }
    }

    /// Regression test: default compare set must be Bedrock-only and contain
    /// exactly the four ids the model comparison runs on.
    ///
    /// Why: compare mode must work out of the box without an OpenRouter key.
    /// What: asserts all entries are `bedrock/`-prefixed and count is 4.
    /// Test: this test itself.
    #[test]
    fn compare_set_models_are_bedrock_only() {
        let all_bedrock = COMPARE_CANDIDATE_MODELS
            .iter()
            .all(|m| m.starts_with("bedrock/"));
        assert!(
            all_bedrock,
            "default compare set must be Bedrock-only (all entries bedrock/-prefixed)"
        );
        assert_eq!(
            COMPARE_CANDIDATE_MODELS.len(),
            4,
            "expect haiku-4.5, sonnet-4.6, sonnet-5.5, opus-5.5"
        );
    }

    /// Verify the compare set carries both 5.5 models and no Sonnet 4.5.
    ///
    /// Why: the model comparison weighs Sonnet 5.5 and Opus 5.5 against the
    /// current Haiku and Sonnet 4.6; Sonnet 4.5 was dropped from the set.
    /// What: asserts both 5.5 ids are present and no `sonnet-4-5` entry is.
    /// Test: this test itself.
    #[test]
    fn compare_set_contains_the_5_5_models() {
        for expected in [
            "bedrock/us.anthropic.claude-sonnet-5-5",
            "bedrock/us.anthropic.claude-opus-5-5",
        ] {
            assert!(
                COMPARE_CANDIDATE_MODELS.contains(&expected),
                "compare set must include {expected}"
            );
        }
        assert!(
            !COMPARE_CANDIDATE_MODELS
                .iter()
                .any(|m| m.contains("sonnet-4-5")),
            "Sonnet 4.5 was dropped from the compare set"
        );
    }

    /// Regression test: Haiku default id must be the verified date-versioned form.
    ///
    /// Why: `us.anthropic.claude-haiku-4-5` (without date stamp) is rejected by
    /// Bedrock with HTTP 400 ValidationException — the full id is required.
    /// What: asserts DEFAULT_SUMMARIZER_MODEL uses the correct date-versioned
    /// id, and that the compare set also contains it.
    /// Test: this test itself.
    #[test]
    fn haiku_default_has_correct_date_versioned_id() {
        const EXPECTED_HAIKU: &str = "us.anthropic.claude-haiku-4-5-20251001-v1:0";
        assert_eq!(
            DEFAULT_SUMMARIZER_MODEL, EXPECTED_HAIKU,
            "DEFAULT_SUMMARIZER_MODEL must use the date-versioned Haiku 4.5 id"
        );
        let compare_has_haiku = COMPARE_CANDIDATE_MODELS
            .iter()
            .any(|m| m.contains(EXPECTED_HAIKU));
        assert!(
            compare_has_haiku,
            "compare set must include the verified Haiku id {EXPECTED_HAIKU}"
        );
    }

    /// Verify compare set is ordered by tier, then generation.
    ///
    /// Why: the `compare` report presents results in table order; Haiku →
    /// Sonnet → Opus keeps the tiers readable side by side.
    /// What: asserts haiku-4-5 < sonnet-4-6 < sonnet-5-5 < opus-5-5 by position.
    /// Test: this test itself.
    #[test]
    fn compare_set_is_ordered_by_tier_then_generation() {
        let pos = |needle: &str| -> usize {
            COMPARE_CANDIDATE_MODELS
                .iter()
                .position(|m| m.contains(needle))
                .unwrap_or(usize::MAX)
        };
        let order = ["haiku-4-5", "sonnet-4-6", "sonnet-5-5", "opus-5-5"];
        for pair in order.windows(2) {
            assert!(
                pos(pair[0]) < pos(pair[1]),
                "{} must come before {} in compare set",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn compare_set_reviewer_default_is_present() {
        // The reviewer default (bedrock/ prefixed) must appear in the compare set.
        let expected = format!("bedrock/{DEFAULT_REVIEWER_MODEL}");
        assert!(
            COMPARE_CANDIDATE_MODELS.contains(&expected.as_str()),
            "compare set must include {expected:?} (bedrock/-prefixed reviewer default)"
        );
    }
}
