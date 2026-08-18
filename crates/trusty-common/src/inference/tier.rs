//! Capability-tier → concrete model id resolution (issue #5971).
//!
//! Why: every model default across this workspace is a version-pinned id, so
//! none of them moves when the model behind a role moves. That has already cost
//! real behaviour — `trusty-agents-common/src/runner.rs:118-121` records a phase
//! that silently ran a pinned `claude-sonnet-4-6` while ignoring its configured
//! override, because the pin was doing the work an alias should have done. A
//! consumer that asks for a TIER instead of an id moves with the tier.
//!
//! What: [`ModelTier`] (three tiers — analysis, interaction, classification) and
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

/// OpenRouter slug for Claude Sonnet 4.6.
///
/// Confirmed present in a live `GET https://openrouter.ai/api/v1/models`
/// response, 2026-08-18. Same id `trusty-audit` verified on 2026-08-13 and used
/// as its `DEFAULT_REVIEWER_MODEL` until earlier that day (#5987).
const OPENROUTER_SONNET_4_6: &str = "anthropic/claude-sonnet-4.6";

/// OpenRouter slug for Claude Haiku 4.5.
///
/// Confirmed present in a live `GET https://openrouter.ai/api/v1/models`
/// response, 2026-08-18.
const OPENROUTER_HAIKU_4_5: &str = "anthropic/claude-haiku-4.5";

/// Anthropic first-party API id for Claude Opus 4.8.
///
/// From the `claude-api` model reference.
const ANTHROPIC_OPUS_4_8: &str = "claude-opus-4-8";

/// Anthropic first-party API id for Claude Sonnet 4.6.
///
/// From the `claude-api` model reference, which states the first-party ids are
/// complete as written and must never carry a date suffix — so this is
/// `claude-sonnet-4-6`, not a dated variant, and it is NOT derived from the
/// OpenRouter spelling above (#5987).
const ANTHROPIC_SONNET_4_6: &str = "claude-sonnet-4-6";

/// Anthropic first-party API id for Claude Haiku 4.5.
///
/// From the `claude-api` model reference.
const ANTHROPIC_HAIKU_4_5: &str = "claude-haiku-4-5-20251001";

/// Bedrock cross-region inference profile for Claude Sonnet 4.6 (US geography).
///
/// The bare form is the one Bedrock accepts — no date stamp and no `-v1:0`
/// suffix, unlike the Haiku profile below. #5987: the owner ruled this arm maps
/// because this id is verified, which is the whole difference from the analysis
/// tier's unverifiable Opus profile.
const BEDROCK_SONNET_4_6: &str = "us.anthropic.claude-sonnet-4-6";

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
/// rather than a grep for every pinned id. Every variant therefore names a
/// PURPOSE, never a model — a variant named for the model it currently resolves
/// to goes stale the moment that workload moves, which is the pin-shaped mistake
/// this layer exists to remove (#5987).
/// What: three tiers, resolving to three different models under the owner ruling
/// of 2026-08-18 — [`Self::Analysis`] to Opus 4.8, [`Self::Interaction`] to
/// Sonnet 4.6, [`Self::Classification`] to Haiku 4.5.
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
    /// Sorting inputs into categories — labelling, routing, triage. Also the
    /// cheapest tier, which is why cost-driven callers select it; see
    /// `trusty-review`'s `RoleModels::resolve` for that distinction.
    Classification,
}

impl ModelTier {
    /// Resolve this tier to a concrete model id for `provider`.
    ///
    /// Why: the id for one tier differs per provider, so a tier alone cannot
    /// name a model. The caller already knows its provider (epic #2400's
    /// `provider_for` resolves it), so it passes both.
    /// What: returns the provider's id for this tier, or `None` when this
    /// provider has no verified id for it — including AWS Bedrock's analysis
    /// tier, deliberately unmapped because the Opus 4.8 inference-profile id
    /// could not be verified and its shape is not derivable from the family
    /// name. `None` is not an error: it means "no tier default here", and the
    /// caller keeps whatever default it had. Providers that host no Claude model
    /// (`OpenAI`, `Fireworks`, `Together`, `AtlasCloud`, `Local`) return `None`
    /// for every tier.
    /// Test: `bedrock_analysis_tier_is_unmapped`,
    /// `bedrock_interaction_resolves_sonnet_4_6`, `bedrock_classification_resolves`,
    /// `every_tier_resolves_on_openrouter`, `every_tier_resolves_on_anthropic`,
    /// `non_claude_providers_resolve_nothing`.
    pub fn resolve(self, provider: ProviderId) -> Option<&'static str> {
        match provider {
            ProviderId::OpenRouter => Some(match self {
                Self::Analysis => OPENROUTER_OPUS_4_8,
                Self::Interaction => OPENROUTER_SONNET_4_6,
                Self::Classification => OPENROUTER_HAIKU_4_5,
            }),
            ProviderId::Anthropic => Some(match self {
                Self::Analysis => ANTHROPIC_OPUS_4_8,
                Self::Interaction => ANTHROPIC_SONNET_4_6,
                Self::Classification => ANTHROPIC_HAIKU_4_5,
            }),
            ProviderId::Bedrock => match self {
                // #5971: Bedrock's Opus 4.8 inference-profile id is unmapped by
                // owner ruling — it could not be verified and must not be
                // guessed. Do not map this arm from the Sonnet arm below: the
                // profile shapes do not correspond, and an invented id fails at
                // call time, not compile time.
                Self::Analysis => None,
                // #5987: the owner ruled this arm maps, because unlike Opus the
                // Sonnet 4.6 profile id is verified.
                Self::Interaction => Some(BEDROCK_SONNET_4_6),
                Self::Classification => Some(BEDROCK_HAIKU_4_5),
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
        ModelTier::Classification,
    ];

    #[test]
    fn every_tier_resolves_on_openrouter() {
        assert_eq!(
            ModelTier::Analysis.resolve(ProviderId::OpenRouter),
            Some("anthropic/claude-opus-4.8")
        );
        assert_eq!(
            ModelTier::Interaction.resolve(ProviderId::OpenRouter),
            Some("anthropic/claude-sonnet-4.6")
        );
        assert_eq!(
            ModelTier::Classification.resolve(ProviderId::OpenRouter),
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
            Some("claude-sonnet-4-6"),
            "the first-party id carries no date suffix, and is not derived from \
             the OpenRouter spelling"
        );
        assert_eq!(
            ModelTier::Classification.resolve(ProviderId::Anthropic),
            Some("claude-haiku-4-5-20251001")
        );
    }

    /// Why: the three tiers resolved to two distinct models before #5987 and
    /// three after, so a refactor that re-groups the match arms could silently
    /// collapse interaction back onto analysis. Asserting the values differ
    /// pins the ruling rather than the arm layout.
    /// What: on both mapped providers, no two tiers resolve to the same id.
    #[test]
    fn the_three_tiers_resolve_three_distinct_models() {
        for provider in [ProviderId::OpenRouter, ProviderId::Anthropic] {
            let analysis = ModelTier::Analysis.resolve(provider);
            let interaction = ModelTier::Interaction.resolve(provider);
            let classification = ModelTier::Classification.resolve(provider);
            assert_ne!(analysis, interaction, "{provider:?}");
            assert_ne!(interaction, classification, "{provider:?}");
            assert_ne!(analysis, classification, "{provider:?}");
        }
    }

    /// Why: guessing an unverified Bedrock inference-profile id fails at call
    /// time, not compile time, so the absence of a mapping is the intended
    /// state and must be asserted rather than left to drift (#5971). Its
    /// neighbours now resolve, so a later change that "completes the table" from
    /// their shape has to delete this assertion to do it.
    /// What: the analysis tier resolves to `None` on Bedrock.
    #[test]
    fn bedrock_analysis_tier_is_unmapped() {
        assert_eq!(ModelTier::Analysis.resolve(ProviderId::Bedrock), None);
    }

    /// Why: Bedrock's Sonnet 4.6 profile id is verified, so the interaction tier
    /// resolves there while analysis stays unmapped (owner ruling, #5987).
    /// What: the interaction arm returns the bare profile — no date stamp and no
    /// `-v1:0` suffix, unlike the Haiku profile beside it.
    #[test]
    fn bedrock_interaction_resolves_sonnet_4_6() {
        assert_eq!(
            ModelTier::Interaction.resolve(ProviderId::Bedrock),
            Some("us.anthropic.claude-sonnet-4-6"),
            "the bare profile is the form Bedrock accepts for Sonnet 4.6"
        );
    }

    #[test]
    fn bedrock_classification_resolves() {
        assert_eq!(
            ModelTier::Classification.resolve(ProviderId::Bedrock),
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

    /// Why: the two tiers resolved to the same model when the split was
    /// introduced, so #5971 pinned that they stayed separate variants against a
    /// collapse. #5987 gave them different models, which is the divergence the
    /// split existed to allow — the assertion now checks that it happened.
    #[test]
    fn analysis_and_interaction_are_distinct_variants() {
        assert_ne!(ModelTier::Analysis, ModelTier::Interaction);
        assert_ne!(
            ModelTier::Analysis.resolve(ProviderId::OpenRouter),
            ModelTier::Interaction.resolve(ProviderId::OpenRouter),
            "analysis is Opus 4.8 and interaction is Sonnet 4.6 (owner ruling 2026-08-18)"
        );
    }
}
