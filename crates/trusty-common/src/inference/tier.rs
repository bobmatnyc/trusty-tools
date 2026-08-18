//! Capability-tier → concrete model id resolution (issue #5971).
//!
//! Why: every model default across this workspace is a version-pinned id, so
//! none of them moves when the model behind a role moves. That has already cost
//! real behaviour — `trusty-agents-common/src/runner.rs:118-121` records a phase
//! that silently ran a pinned `claude-sonnet-4-6` while ignoring its configured
//! override, because the pin was doing the work an alias should have done. A
//! consumer that asks for a TIER instead of an id moves with the tier.
//!
//! What: [`ModelTier`] (three tiers — analysis, interaction, haiku) and
//! [`ModelTier::resolve`], which maps a tier plus a [`ProviderId`] to a concrete
//! model id. Resolution is provider-dependent because the same model wears a
//! different id per provider: Bedrock inference profiles are `us.anthropic.…`,
//! OpenRouter slugs are `anthropic/claude-…`, and the Anthropic first-party API
//! uses dashed ids. A provider with no mapping for a tier returns `None` so the
//! caller keeps whatever default it already had.
//!
//! This sits beside the provider/transport resolution epic #2400 put in
//! [`crate::inference::configurator`] — that layer answers "which provider and
//! how do I reach it", this one answers "which model on that provider".
//!
//! Test: inline `tests` below, plus
//! `crates/trusty-common/tests/inference_foundation.rs::model_tier_*`.

use super::registry::ProviderId;

// ─── Verified model ids ───────────────────────────────────────────────────────
//
// Every id below was verified against its provider before being written here.
// Do NOT add an id derived from another provider's naming pattern: the shapes do
// not correspond, and this table proves it in both directions — Bedrock's Sonnet
// 4.6 profile works bare (`us.anthropic.claude-sonnet-4-6`) while its Haiku 4.5
// profile needs both a date stamp and a `-v1:0` version suffix. An invented id
// fails at call time, not compile time.

/// OpenRouter slug for Claude Opus 4.8.
///
/// Confirmed present in a live `GET https://openrouter.ai/api/v1/models`
/// response, 2026-08-18.
const OPENROUTER_OPUS_4_8: &str = "anthropic/claude-opus-4.8";

/// OpenRouter slug for Claude Haiku 4.5.
///
/// Confirmed present in a live `GET https://openrouter.ai/api/v1/models`
/// response, 2026-08-18.
const OPENROUTER_HAIKU_4_5: &str = "anthropic/claude-haiku-4.5";

/// Anthropic first-party API id for Claude Opus 4.8.
///
/// From the `claude-api` model reference.
const ANTHROPIC_OPUS_4_8: &str = "claude-opus-4-8";

/// Anthropic first-party API id for Claude Haiku 4.5.
///
/// From the `claude-api` model reference.
const ANTHROPIC_HAIKU_4_5: &str = "claude-haiku-4-5-20251001";

/// Bedrock cross-region inference profile for Claude Haiku 4.5 (US geography).
///
/// Verified against a live Bedrock account; the short form
/// `us.anthropic.claude-haiku-4-5` returns HTTP 400 ValidationException. Same id
/// as `trusty-review`'s pre-#5971 verifier/summarizer default.
const BEDROCK_HAIKU_4_5: &str = "us.anthropic.claude-haiku-4-5-20251001-v1:0";

// ─── Tiers ────────────────────────────────────────────────────────────────────

/// A capability tier a caller asks for instead of naming a concrete model id.
///
/// Why: a role should declare the class of model its work needs, so that moving
/// the whole workspace to a newer model is one edit to [`ModelTier::resolve`]
/// rather than a grep for every pinned id.
/// What: three tiers. [`Self::Analysis`] and [`Self::Interaction`] resolve to
/// the same model today (owner ruling, 2026-08-18) and are deliberately kept as
/// two arms so they can diverge without touching a caller. [`Self::Haiku`] is
/// the cheap high-volume tier for short, low-stakes calls.
/// Test: `analysis_and_interaction_are_distinct_variants`,
/// `every_tier_resolves_on_openrouter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ModelTier {
    /// Deep reasoning: the model that does the judging (code review, digests,
    /// deep analysis passes).
    Analysis,
    /// Conversational turns with a user.
    Interaction,
    /// Short, high-volume, low-stakes calls — per-finding verification,
    /// classification, summarisation.
    Haiku,
}

impl ModelTier {
    /// Resolve this tier to a concrete model id for `provider`.
    ///
    /// Why: the id for one tier differs per provider, so a tier alone cannot
    /// name a model. The caller already knows its provider (epic #2400's
    /// `provider_for` resolves it), so it passes both.
    /// What: returns the provider's id for this tier, or `None` when this
    /// provider has no verified id for it — including AWS Bedrock's opus tiers,
    /// deliberately unmapped because the inference-profile id could not be
    /// verified and its shape is not derivable from the family name. `None` is
    /// not an error: it means "no tier default here", and the caller keeps
    /// whatever default it had. Providers that host no Claude model
    /// (`OpenAI`, `Fireworks`, `Together`, `AtlasCloud`, `Local`) return `None`
    /// for every tier.
    /// Test: `bedrock_opus_tiers_are_unmapped`, `bedrock_haiku_resolves`,
    /// `every_tier_resolves_on_openrouter`, `every_tier_resolves_on_anthropic`,
    /// `non_claude_providers_resolve_nothing`.
    pub fn resolve(self, provider: ProviderId) -> Option<&'static str> {
        match provider {
            ProviderId::OpenRouter => Some(match self {
                Self::Analysis | Self::Interaction => OPENROUTER_OPUS_4_8,
                Self::Haiku => OPENROUTER_HAIKU_4_5,
            }),
            ProviderId::Anthropic => Some(match self {
                Self::Analysis | Self::Interaction => ANTHROPIC_OPUS_4_8,
                Self::Haiku => ANTHROPIC_HAIKU_4_5,
            }),
            // #5971: Bedrock's Opus 4.8 inference-profile id is unmapped by
            // owner ruling — it could not be verified and must not be guessed.
            ProviderId::Bedrock => match self {
                Self::Analysis | Self::Interaction => None,
                Self::Haiku => Some(BEDROCK_HAIKU_4_5),
            },
            ProviderId::OpenAI
            | ProviderId::Fireworks
            | ProviderId::Together
            | ProviderId::AtlasCloud
            | ProviderId::Local => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_TIERS: &[ModelTier] = &[
        ModelTier::Analysis,
        ModelTier::Interaction,
        ModelTier::Haiku,
    ];

    #[test]
    fn every_tier_resolves_on_openrouter() {
        assert_eq!(
            ModelTier::Analysis.resolve(ProviderId::OpenRouter),
            Some("anthropic/claude-opus-4.8")
        );
        assert_eq!(
            ModelTier::Interaction.resolve(ProviderId::OpenRouter),
            Some("anthropic/claude-opus-4.8")
        );
        assert_eq!(
            ModelTier::Haiku.resolve(ProviderId::OpenRouter),
            Some("anthropic/claude-haiku-4.5")
        );
    }

    #[test]
    fn every_tier_resolves_on_anthropic() {
        assert_eq!(
            ModelTier::Analysis.resolve(ProviderId::Anthropic),
            Some("claude-opus-4-8")
        );
        assert_eq!(
            ModelTier::Interaction.resolve(ProviderId::Anthropic),
            Some("claude-opus-4-8")
        );
        assert_eq!(
            ModelTier::Haiku.resolve(ProviderId::Anthropic),
            Some("claude-haiku-4-5-20251001")
        );
    }

    /// Why: guessing an unverified Bedrock inference-profile id fails at call
    /// time, not compile time, so the absence of a mapping is the intended
    /// state and must be asserted rather than left to drift (#5971).
    /// What: both opus-tier arms resolve to `None` on Bedrock.
    #[test]
    fn bedrock_opus_tiers_are_unmapped() {
        assert_eq!(ModelTier::Analysis.resolve(ProviderId::Bedrock), None);
        assert_eq!(ModelTier::Interaction.resolve(ProviderId::Bedrock), None);
    }

    #[test]
    fn bedrock_haiku_resolves() {
        assert_eq!(
            ModelTier::Haiku.resolve(ProviderId::Bedrock),
            Some("us.anthropic.claude-haiku-4-5-20251001-v1:0"),
            "the date-stamped, version-suffixed profile is the only form Bedrock accepts"
        );
    }

    #[test]
    fn non_claude_providers_resolve_nothing() {
        for provider in [
            ProviderId::OpenAI,
            ProviderId::Fireworks,
            ProviderId::Together,
            ProviderId::AtlasCloud,
            ProviderId::Local,
        ] {
            for tier in ALL_TIERS {
                assert_eq!(
                    tier.resolve(provider),
                    None,
                    "{provider:?} hosts no Claude model, so {tier:?} must not resolve"
                );
            }
        }
    }

    /// Why: the two tiers name the same model today and the obvious
    /// simplification is to collapse them into one variant. The split is the
    /// point — it lets analysis and interaction diverge later without touching
    /// a caller — so a test pins that they stay separate values (#5971).
    #[test]
    fn analysis_and_interaction_are_distinct_variants() {
        assert_ne!(ModelTier::Analysis, ModelTier::Interaction);
        assert_eq!(
            ModelTier::Analysis.resolve(ProviderId::OpenRouter),
            ModelTier::Interaction.resolve(ProviderId::OpenRouter),
            "both resolve to Opus 4.8 today (owner ruling 2026-08-18)"
        );
    }
}
