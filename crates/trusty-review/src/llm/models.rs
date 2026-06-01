//! Model identifier constants for trusty-review.
//!
//! Why: having model ids in one place makes it trivial to audit and update
//! the model set without grepping across the codebase; it also documents the
//! intent of the default configuration and the compare-set candidates.
//!
//! What: defines the built-in default model ids for all three roles (now
//! Bedrock-first), plus a compare-set that mixes Bedrock tiers and includes
//! an OpenRouter example to show the mixed-provider capability.
//!
//! DEFAULT PROVIDER: Bedrock (effective as of this file's introduction).
//!   - Reviewer: `us.anthropic.claude-sonnet-4-6` (Bedrock cross-region profile)
//!   - Verifier: `us.anthropic.claude-haiku-4-5` (Bedrock, cheaper tier)
//!   - Summarizer: `us.anthropic.claude-haiku-4-5` (Bedrock, cheaper tier)
//!
//! FLAG — Bedrock model-id accuracy:
//!   The `us.anthropic.claude-sonnet-4-6` id is verified in CLAUDE.md.
//!   `us.anthropic.claude-haiku-4-5` is a plausible current Haiku-tier id
//!   but SHOULD be confirmed against the caller's Bedrock model catalog:
//!     aws bedrock list-foundation-models --query 'modelSummaries[*].modelId'
//!   Override via `TRUSTY_REVIEW_VERIFIER_MODEL` / `TRUSTY_REVIEW_SUMMARIZER_MODEL`
//!   env vars or the `[models]` table in `~/.config/trusty-review/config.toml`
//!   if the Haiku id is wrong for your account.
//!
//! OpenRouter remains fully available for all roles; select it with:
//!   - `--provider openrouter` CLI flag, or
//!   - `TRUSTY_REVIEW_PROVIDER=openrouter` env var, or
//!   - `provider = "openrouter"` in the config file, or
//!   - an `openrouter/<model-id>` prefix on the model slug.
//!
//! Test: `bedrock_defaults_have_inference_profile_prefix`,
//! `compare_set_includes_bedrock_and_openrouter_examples`.

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

/// Default model for the verifier role (per-finding verification round) — Bedrock Haiku.
///
/// Why: verifier calls are short (single-word output) and high-volume; the
/// cheapest Haiku-tier Bedrock model keeps latency and cost low.
/// What: `us.anthropic.claude-haiku-4-5` is the expected Haiku 4.5 cross-region
/// inference profile id.
/// Override via `TRUSTY_REVIEW_VERIFIER_MODEL`.
///
/// FLAG: Confirm `us.anthropic.claude-haiku-4-5` against your Bedrock model
/// catalog — the exact id may differ in your account or region.  Check with:
///   aws bedrock list-foundation-models --query 'modelSummaries[*].modelId'
///
/// CRITICAL: the verifier model MUST be a foundation-lifecycle ACTIVE model
/// (spec REV-340).  If this slug is inactive, every finding will be silently
/// refuted and every review will APPROVE.
pub const DEFAULT_VERIFIER_MODEL: &str = "us.anthropic.claude-haiku-4-5";

/// Default model for the summarizer role (diff Stage-C classification) — Bedrock Haiku.
///
/// Why: summarizer calls are deterministic (temperature 0) and low-stakes;
/// the cheapest Haiku-tier Bedrock model is appropriate.
/// What: same as `DEFAULT_VERIFIER_MODEL` — `us.anthropic.claude-haiku-4-5`.
/// Override via `TRUSTY_REVIEW_SUMMARIZER_MODEL`.
///
/// FLAG: Same caveat as DEFAULT_VERIFIER_MODEL — confirm the Haiku id.
pub const DEFAULT_SUMMARIZER_MODEL: &str = "us.anthropic.claude-haiku-4-5";

// ─── Compare-set ─────────────────────────────────────────────────────────────

/// Candidate model ids for the `compare` subcommand.
///
/// Why: the `compare` mode runs the same PR through multiple reviewer models
/// and ranks them by quality/speed/cost.  This set seeds the default candidate
/// list and demonstrates the mixed-provider capability (Bedrock tiers + an
/// OpenRouter example).
/// What: a static slice of model ids ordered cheapest → most capable.
/// Each entry is either a `bedrock/`-prefixed or `openrouter/`-prefixed id.
/// The `compare` subcommand resolves the provider per-entry via
/// `resolve_provider_and_model`.
///
/// FLAG: Confirm the Haiku and Opus ids against your Bedrock catalog.  The
/// Sonnet 4.6 id is verified; Haiku 4.5 and Opus 4.8 are plausible ids.
pub const COMPARE_CANDIDATE_MODELS: &[&str] = &[
    // Bedrock Haiku — cheapest tier (verifier/summarizer default).
    "bedrock/us.anthropic.claude-haiku-4-5",
    // Bedrock Sonnet 4.6 — balanced (reviewer default).
    "bedrock/us.anthropic.claude-sonnet-4-6",
    // Bedrock Opus — premium (if enabled in your Bedrock account).
    // FLAG: confirm us.anthropic.claude-opus-4-8 is available in your account.
    "bedrock/us.anthropic.claude-opus-4-8",
    // OpenRouter GPT-5.4-mini — shows mixed-provider capability.
    "openrouter/openai/gpt-5.4-mini-20260317",
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

    #[test]
    fn compare_set_includes_bedrock_and_openrouter_examples() {
        let has_bedrock = COMPARE_CANDIDATE_MODELS
            .iter()
            .any(|m| m.starts_with("bedrock/"));
        let has_openrouter = COMPARE_CANDIDATE_MODELS
            .iter()
            .any(|m| m.starts_with("openrouter/"));
        assert!(
            has_bedrock,
            "compare set must include at least one bedrock/ entry"
        );
        assert!(
            has_openrouter,
            "compare set must include at least one openrouter/ entry as example"
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
