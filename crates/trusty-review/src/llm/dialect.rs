//! Reading a model id written in another provider's dialect (#6135).
//!
//! Why: the same Claude model is spelled three ways — `anthropic/claude-opus-4.8`
//! on OpenRouter, `us.anthropic.claude-haiku-4-5-20251001-v1:0` on Bedrock,
//! `claude-opus-4-8` on the first-party API. Until now a pair whose id and
//! provider disagreed was a hard stop (#6114). The owner ruling of 2026-08-21 is
//! that a naming difference must resolve to the operator's evident intent and
//! the run must proceed: "Models selection should be robust. We shouldn't fail
//! on naming/id issue."
//!
//! What: [`translate_model_dialect`] answers "the same model, in this provider's
//! spelling" — and answers it from the VERIFIED catalogue in
//! [`trusty_common::inference::ModelTier`], never by string surgery on the id.
//! That distinction is the whole safety property: Bedrock's Haiku 4.5 profile
//! needs a date stamp and a `-v1:0` suffix while its Sonnet 4.6 profile does
//! not, and its Opus 4.8 profile is deliberately unmapped because nobody could
//! verify it. A mechanically-derived id would fail at call time, not here.
//! `None` — no verified counterpart — is an ordinary answer, and the caller
//! routes to the provider the id's own shape names instead.
//! Test: the inline `tests` module below.

use trusty_common::inference::{ModelTier, ProviderId};

/// Every tier a model id can belong to.
const TIERS: [ModelTier; 3] = [
    ModelTier::Analysis,
    ModelTier::Interaction,
    ModelTier::Classification,
];

/// The dialects a known id may be written in.
///
/// Anthropic is included even though `trusty-review` cannot call it directly:
/// an operator who pasted a first-party id into a config still named a model
/// this workspace knows, and reading it is what lets the run proceed.
const DIALECTS: [ProviderId; 3] = [
    ProviderId::OpenRouter,
    ProviderId::Bedrock,
    ProviderId::Anthropic,
];

/// The capability tier `model` names, whichever dialect it is written in.
///
/// Why: a tier is the only provider-independent identity this workspace has for
/// a model, so it is the hinge every translation turns on.
/// What: an exact match against every `(tier, dialect)` id in the verified
/// catalogue. `None` for an id the catalogue does not carry — a newer model, a
/// non-Claude one, a typo.
/// Test: `every_catalogue_id_reports_its_tier`, `an_unknown_id_has_no_tier`.
pub fn tier_of_model(model: &str) -> Option<ModelTier> {
    let id = model.trim();
    TIERS.into_iter().find(|tier| {
        DIALECTS
            .into_iter()
            .any(|dialect| tier.resolve(dialect) == Some(id))
    })
}

/// The same model as `model`, spelled the way `to` spells it.
///
/// Why: an engagement that pinned OpenRouter slugs and a host configured for
/// Bedrock used to be irreconcilable. Translating through the catalogue keeps
/// the operator's model — the actual intent — while honouring the provider they
/// pinned for this call.
/// What: `Some(id)` when `model` is a catalogue id AND `to` has a verified id
/// for the same tier AND that id differs from the input. `None` otherwise, which
/// includes every unverifiable direction (Bedrock has no Opus 4.8 profile) and
/// every provider hosting no Claude model (Fireworks).
/// Test: `an_openrouter_slug_translates_to_bedrock`,
/// `a_bedrock_profile_translates_to_openrouter`,
/// `an_unverifiable_direction_does_not_translate`,
/// `a_provider_with_no_claude_models_does_not_translate`.
pub fn translate_model_dialect(model: &str, to: ProviderId) -> Option<String> {
    let tier = tier_of_model(model)?;
    let translated = tier.resolve(to)?;
    (translated != model.trim()).then(|| translated.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalogue_id_reports_its_tier() {
        for tier in TIERS {
            for dialect in DIALECTS {
                let Some(id) = tier.resolve(dialect) else {
                    continue;
                };
                assert_eq!(
                    tier_of_model(id),
                    Some(tier),
                    "{id} ({dialect:?}) must read back as its own tier"
                );
            }
        }
    }

    #[test]
    fn an_unknown_id_has_no_tier() {
        for id in [
            "",
            "gpt-5.4-mini",
            "anthropic/claude-opus-9.9",
            "accounts/fireworks/models/llama-v3",
        ] {
            assert_eq!(tier_of_model(id), None, "{id} is not a catalogue id");
        }
    }

    #[test]
    fn an_openrouter_slug_translates_to_bedrock() {
        // Sonnet and Haiku have verified profiles on both sides.
        assert_eq!(
            translate_model_dialect("anthropic/claude-sonnet-4.6", ProviderId::Bedrock).as_deref(),
            Some("us.anthropic.claude-sonnet-4-6")
        );
        assert_eq!(
            translate_model_dialect("anthropic/claude-haiku-4.5", ProviderId::Bedrock).as_deref(),
            Some("us.anthropic.claude-haiku-4-5-20251001-v1:0"),
            "the translation must be the VERIFIED profile, date stamp and all"
        );
    }

    #[test]
    fn a_bedrock_profile_translates_to_openrouter() {
        assert_eq!(
            translate_model_dialect("us.anthropic.claude-sonnet-4-6", ProviderId::OpenRouter)
                .as_deref(),
            Some("anthropic/claude-sonnet-4.6")
        );
    }

    #[test]
    fn an_unverifiable_direction_does_not_translate() {
        // Bedrock's Opus 4.8 profile id is unmapped by owner ruling (#5971), so
        // there is nothing to translate INTO — and inventing one is the failure
        // this module exists to avoid.
        assert_eq!(
            translate_model_dialect("anthropic/claude-opus-4.8", ProviderId::Bedrock),
            None
        );
    }

    #[test]
    fn a_provider_with_no_claude_models_does_not_translate() {
        assert_eq!(
            translate_model_dialect("anthropic/claude-sonnet-4.6", ProviderId::Fireworks),
            None
        );
    }

    #[test]
    fn an_id_already_in_the_target_dialect_is_not_a_translation() {
        assert_eq!(
            translate_model_dialect("anthropic/claude-sonnet-4.6", ProviderId::OpenRouter),
            None,
            "nothing changed, so there is nothing to report as an adjustment"
        );
    }
}
