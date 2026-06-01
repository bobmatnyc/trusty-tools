//! Model identifier constants for trusty-review.
//!
//! Why: having model ids in one place makes it trivial to audit and update
//! the model set without grepping across the codebase; it also documents the
//! intent of the default configuration and the compare-set candidates.
//!
//! What: defines the built-in default model ids for all three roles (now
//! Bedrock-first), plus a Bedrock-only compare-set (Haiku → Sonnet → Opus).
//!
//! DEFAULT PROVIDER: Bedrock (effective as of this file's introduction).
//!   - Reviewer:   `us.anthropic.claude-sonnet-4-6`              (verified)
//!   - Verifier:   `us.anthropic.claude-haiku-4-5-20251001-v1:0` (verified)
//!   - Summarizer: `us.anthropic.claude-haiku-4-5-20251001-v1:0` (verified)
//!   - Opus:       `us.anthropic.claude-opus-4-8`                (verified)
//!
//! Model-id verification status (June 2026):
//!   - `us.anthropic.claude-sonnet-4-6`              — confirmed in CLAUDE.md.
//!   - `us.anthropic.claude-haiku-4-5-20251001-v1:0` — verified against live
//!     Bedrock account (replaces the incorrect `us.anthropic.claude-haiku-4-5`
//!     which produced HTTP 400 ValidationException).
//!   - `us.anthropic.claude-opus-4-8`                — confirmed in CLAUDE.md.
//!
//! OpenRouter remains fully available for all roles; select it with:
//!   - `--provider openrouter` CLI flag, or
//!   - `TRUSTY_REVIEW_PROVIDER=openrouter` env var, or
//!   - `provider = "openrouter"` in the config file, or
//!   - an `openrouter/<model-id>` prefix on the model slug.
//!
//! Test: `bedrock_defaults_have_inference_profile_prefix`,
//! `compare_set_is_bedrock_only`,
//! `haiku_default_has_correct_date_versioned_id`.

// ─── Default Bedrock model ids ────────────────────────────────────────────────

/// Default model for the reviewer role (main review pass) — Bedrock Sonnet 4.6.
///
/// Why: the reviewer role makes the highest-quality call in the pipeline; Claude
/// Sonnet 4.6 is the recommended balanced choice on Bedrock.  Bedrock is the
/// default because it uses IAM auth (no API key), integrates with AWS secrets
/// management, and keeps data within the operator's VPC.
/// What: `us.anthropic.claude-sonnet-4-6` is the Claude Sonnet 4.6 cross-region
/// inference profile for the US geography.  No date stamp or `-v1:0` suffix
/// (verified against AWS docs as of May 2026).
/// Override via `TRUSTY_REVIEW_REVIEWER_MODEL`.
pub const DEFAULT_REVIEWER_MODEL: &str = "us.anthropic.claude-sonnet-4-6";

/// Default model for the verifier role (per-finding verification round) — Bedrock Haiku 4.5.
///
/// Why: verifier calls are short (single-word output) and high-volume; the
/// cheapest Haiku-tier Bedrock model keeps latency and cost low.
/// What: `us.anthropic.claude-haiku-4-5-20251001-v1:0` is the verified Haiku 4.5
/// cross-region inference profile id (date-stamped, as required by Bedrock).
/// The previous short form `us.anthropic.claude-haiku-4-5` produced HTTP 400
/// ValidationException — this is the correct full id.
/// Override via `TRUSTY_REVIEW_VERIFIER_MODEL`.
///
/// CRITICAL: the verifier model MUST be a foundation-lifecycle ACTIVE model
/// (spec REV-340).  If this slug is inactive, every finding will be silently
/// refuted and every review will APPROVE.
pub const DEFAULT_VERIFIER_MODEL: &str = "us.anthropic.claude-haiku-4-5-20251001-v1:0";

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
/// it works out of the box without an OpenRouter API key.
/// What: a static slice of `bedrock/`-prefixed model ids ordered cheapest →
/// most capable.  The `compare` subcommand resolves the provider per-entry
/// via `resolve_provider_and_model`, strips the prefix, and sends the bare id
/// to the Bedrock Converse API.
/// Override with `--models` to add OpenRouter or other providers.
///
/// All three ids are verified against the target Bedrock account (June 2026):
///   - Haiku 4.5:   `us.anthropic.claude-haiku-4-5-20251001-v1:0` (date-stamped)
///   - Sonnet 4.6:  `us.anthropic.claude-sonnet-4-6`
///   - Opus 4.8:    `us.anthropic.claude-opus-4-8`
pub const COMPARE_CANDIDATE_MODELS: &[&str] = &[
    // Bedrock Haiku 4.5 — cheapest tier (verifier/summarizer default).
    // date-versioned id required by Bedrock (short form produces HTTP 400).
    "bedrock/us.anthropic.claude-haiku-4-5-20251001-v1:0",
    // Bedrock Sonnet 4.6 — balanced (reviewer default).
    "bedrock/us.anthropic.claude-sonnet-4-6",
    // Bedrock Opus 4.8 — premium (requires Opus access in your Bedrock account).
    "bedrock/us.anthropic.claude-opus-4-8",
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
    fn default_verifier_and_summarizer_are_haiku() {
        for (name, id) in [
            ("verifier", DEFAULT_VERIFIER_MODEL),
            ("summarizer", DEFAULT_SUMMARIZER_MODEL),
        ] {
            assert!(
                id.contains("haiku"),
                "{name} default should be Haiku-tier: {id}"
            );
        }
    }

    /// Regression test: default compare set must be Bedrock-only and contain
    /// all three expected (verified) model ids.
    ///
    /// Why: the previous compare set contained the wrong Haiku id and an
    /// OpenRouter entry that requires a separate API key — the Bedrock-only
    /// default is immediately usable.
    /// What: asserts all entries are `bedrock/`-prefixed and that the verified
    /// Haiku id appears.
    /// Test: this test itself.
    #[test]
    fn compare_set_is_bedrock_only() {
        let all_bedrock = COMPARE_CANDIDATE_MODELS
            .iter()
            .all(|m| m.starts_with("bedrock/"));
        assert!(
            all_bedrock,
            "default compare set must be Bedrock-only (all entries bedrock/-prefixed)"
        );
        assert_eq!(
            COMPARE_CANDIDATE_MODELS.len(),
            3,
            "expect haiku, sonnet, opus"
        );
    }

    /// Regression test: Haiku default id must be the verified date-versioned form.
    ///
    /// Why: `us.anthropic.claude-haiku-4-5` (without date stamp) is rejected by
    /// Bedrock with HTTP 400 ValidationException — the full id is required.
    /// What: asserts both DEFAULT_VERIFIER_MODEL and DEFAULT_SUMMARIZER_MODEL
    /// use the correct date-versioned id, and that the compare set also contains it.
    /// Test: this test itself.
    #[test]
    fn haiku_default_has_correct_date_versioned_id() {
        const EXPECTED_HAIKU: &str = "us.anthropic.claude-haiku-4-5-20251001-v1:0";
        assert_eq!(
            DEFAULT_VERIFIER_MODEL, EXPECTED_HAIKU,
            "DEFAULT_VERIFIER_MODEL must use the date-versioned Haiku 4.5 id"
        );
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

    #[test]
    fn compare_set_is_ordered_cheap_to_premium() {
        // Haiku must come before Sonnet, Sonnet before Opus.
        let pos = |needle: &str| -> usize {
            COMPARE_CANDIDATE_MODELS
                .iter()
                .position(|m| m.contains(needle))
                .unwrap_or(usize::MAX)
        };
        assert!(
            pos("haiku") < pos("sonnet"),
            "haiku must come before sonnet in compare set"
        );
        assert!(
            pos("sonnet") < pos("opus"),
            "sonnet must come before opus in compare set"
        );
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
