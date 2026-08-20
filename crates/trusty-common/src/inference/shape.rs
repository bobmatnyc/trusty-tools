//! Which provider a bare model id names, judged from its SHAPE (#6114).
//!
//! Why: a model id only means something inside one provider's namespace, but
//! every resolver in this workspace routed on an explicit `bedrock/` /
//! `openrouter/` prefix alone and passed anything else through to whatever the
//! default provider happened to be. On the #6093 mitigation run that sent an
//! OpenRouter slug (`anthropic/claude-opus-4.8`) into a Bedrock-default path:
//! Bedrock rejected the id, and the render that eventually completed ran
//! Bedrock's `us.anthropic.claude-sonnet-4-6` default instead of the model the
//! operator asked for. Two resolvers carried that fall-through
//! (`trusty-review`'s `llm::resolve_provider_and_model` and `trusty-mpm`'s
//! `core::sm::providers::resolve`), so the rule lives here once.
//!
//! What: [`infer_provider_from_model_shape`] answers "whose namespace is this
//! id in", returning `None` when the id is genuinely ambiguous;
//! [`shape_mismatch`] turns that into the reconciliation check a resolver
//! needs — `Some(inferred)` when the id names a DIFFERENT provider than the one
//! about to execute it.
//!
//! Which mismatch function to call depends on where the provider came from. A
//! standing config default loses to any shape; an explicit per-call routing
//! prefix is a human's statement about THIS call and loses only to a catalogue
//! fact, never to the dotted-vendor guess. So: no prefix given →
//! [`shape_mismatch`]; prefix given → [`conclusive_shape_mismatch`]. The two
//! strengths are [`ShapeEvidence`].
//!
//! This is the mirror of [`crate::inference::tier`]: `ModelTier::resolve` maps
//! a provider to a model id, this maps a model id back to its provider.
//!
//! 🔴 **Call this only on an id whose routing prefix you have already
//! stripped.** Slash-form is read as an OpenRouter slug here, while
//! [`ProviderId::from_slug_prefix`] reads the same `anthropic/…` string as the
//! Anthropic first-party family. Both are right for their own question — a
//! routing prefix is what the OPERATOR wrote to pin a provider, an id shape is
//! what the PROVIDER's own catalogue looks like. A consumer strips its own
//! prefix table first, then asks this module about what remains.
//!
//! [`ProviderId::from_slug_prefix`]: crate::inference::registry::ProviderId::from_slug_prefix
//!
//! Test: the inline `tests` module below.

use super::registry::ProviderId;

/// Bedrock cross-region inference-profile prefixes.
///
/// Why: a Bedrock model id that carries one of these is unmistakably Bedrock —
/// no other provider in the registry uses region-scoped dotted ids. Bedrock
/// itself REQUIRES one on Anthropic models, so `trusty-review`'s Bedrock
/// provider validates against this same list rather than keeping a second copy.
/// What: the region scopes AWS publishes inference profiles under.
/// Test: `us_profile_id_is_bedrock`; consumer-side,
/// `trusty_review::llm::bedrock::tests::bedrock_us_prefix_validation`.
pub const BEDROCK_INFERENCE_PROFILE_PREFIXES: &[&str] = &["us.", "eu.", "ap.", "jp.", "global."];

/// Vendor segments that open a Bedrock foundation-model id.
///
/// Why: an un-profiled Bedrock id (`anthropic.claude-sonnet-4-6`) is still
/// unmistakably Bedrock — the dotted `vendor.model` spelling belongs to no
/// other provider here. Matching it lets the resolver name the mismatch
/// instead of handing the id to an aggregator that would 404 on it.
/// What: the vendors AWS publishes foundation models under. A dotted id whose
/// first segment is not in this list stays ambiguous.
/// Test: `dotted_vendor_id_is_bedrock`, `unknown_dotted_prefix_is_ambiguous`.
const BEDROCK_VENDOR_SEGMENTS: &[&str] = &[
    "ai21",
    "amazon",
    "anthropic",
    "cohere",
    "deepseek",
    "luma",
    "meta",
    "mistral",
    "openai",
    "qwen",
    "stability",
    "twelvelabs",
    "writer",
];

/// The provider-native prefix every Fireworks model id carries.
const FIREWORKS_NATIVE_PREFIX: &str = "accounts/fireworks/models/";

/// How strongly a model id's spelling names its provider.
///
/// Why: not every signal here is equally good, and the difference decides
/// whether inference may overrule a human. A slash-form slug or a region-scoped
/// inference profile belongs to exactly one provider's catalogue. A dotted
/// `vendor.model` id is a guess from a vendor-name list, and a guess must not
/// veto an operator who typed a routing prefix on this very call.
/// What: the two strengths [`classify_model_shape`] reports.
/// Test: `dotted_vendor_evidence_is_probable`,
/// `slug_and_profile_evidence_is_conclusive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeEvidence {
    /// The spelling belongs to exactly one provider's catalogue: a
    /// `vendor/model` slug, the Fireworks-native path, or a region-scoped
    /// Bedrock inference profile.
    Conclusive,
    /// A dotted `vendor.model` id whose first segment is a known Bedrock
    /// vendor. Strong enough to outrank a standing config default, not strong
    /// enough to outrank an explicit per-call routing prefix.
    Probable,
}

/// Classify `model`'s provider and how strongly its spelling says so.
///
/// Why: [`infer_provider_from_model_shape`] answers "whose is this", but a
/// resolver deciding whether to REJECT an operator's explicit prefix needs to
/// know whether the answer is a catalogue fact or a vendor-name guess (#6114).
/// What: `Some((provider, evidence))`, or `None` when the id could belong to
/// several providers (`claude-opus-4-5-20260101`, `gpt-5.4-mini`) — the case
/// where a caller's configured default legitimately decides.
///
/// The input must already have had the caller's own routing prefix stripped —
/// see the module docs.
/// Test: `slug_and_profile_evidence_is_conclusive`,
/// `dotted_vendor_evidence_is_probable`.
pub fn classify_model_shape(model: &str) -> Option<(ProviderId, ShapeEvidence)> {
    let id = model.trim();
    if id.is_empty() {
        return None;
    }
    if id.starts_with(FIREWORKS_NATIVE_PREFIX) {
        return Some((ProviderId::Fireworks, ShapeEvidence::Conclusive));
    }
    if id.contains('/') {
        return Some((ProviderId::OpenRouter, ShapeEvidence::Conclusive));
    }
    if BEDROCK_INFERENCE_PROFILE_PREFIXES
        .iter()
        .any(|pfx| id.starts_with(pfx))
    {
        return Some((ProviderId::Bedrock, ShapeEvidence::Conclusive));
    }
    let vendor = id.split('.').next().unwrap_or(id);
    if vendor.len() < id.len() && BEDROCK_VENDOR_SEGMENTS.contains(&vendor) {
        return Some((ProviderId::Bedrock, ShapeEvidence::Probable));
    }
    None
}

/// Infer the provider whose namespace `model` belongs to, from its shape alone.
///
/// Why: a resolver that cannot tell `anthropic/claude-opus-4.8` from
/// `us.anthropic.claude-opus-4-8` has no way to notice that the id and the
/// provider about to run it disagree, so it runs the wrong model silently
/// (#6114). This is the form to use when NO explicit routing prefix was given —
/// there is no human choice to overrule, so both evidence strengths count.
/// What: [`classify_model_shape`] with the evidence discarded.
///
/// The input must already have had the caller's own routing prefix stripped —
/// see the module docs.
/// Test: `openrouter_slug_is_openrouter`, `us_profile_id_is_bedrock`,
/// `dotted_vendor_id_is_bedrock`, `fireworks_native_id_is_fireworks`,
/// `bare_ids_are_ambiguous`.
pub fn infer_provider_from_model_shape(model: &str) -> Option<ProviderId> {
    classify_model_shape(model).map(|(provider, _)| provider)
}

/// Report the provider `model`'s shape names, when that is not `provider`.
///
/// Why: this is the reconciliation a resolver owes its caller — the run must
/// fail naming both the id and the provider rather than execute a different
/// model than the one requested (#6114).
/// What: `Some(inferred)` when [`infer_provider_from_model_shape`] names a
/// provider other than `provider`; `None` when they agree or the id is
/// ambiguous. Both evidence strengths count, so use this only where `provider`
/// came from a config default — see [`conclusive_shape_mismatch`] for the
/// explicit-prefix case.
/// Test: `mismatch_flags_openrouter_slug_on_bedrock`,
/// `mismatch_is_none_when_shape_agrees`, `mismatch_is_none_for_ambiguous_id`.
pub fn shape_mismatch(provider: ProviderId, model: &str) -> Option<ProviderId> {
    match infer_provider_from_model_shape(model) {
        Some(inferred) if inferred != provider => Some(inferred),
        _ => None,
    }
}

/// The same reconciliation, but only on evidence strong enough to overrule a
/// human.
///
/// Why: an operator who writes `openrouter/anthropic.claude-x` has stated the
/// provider for this call. Rejecting that because a dotted first segment
/// happens to match a Bedrock vendor name would turn a working configuration
/// into a hard error on a guess. The catalogue-fact shapes still win: nothing
/// makes `bedrock/anthropic/claude-opus-4.8` runnable, and refusing it is the
/// whole point of #6114.
/// What: `Some(inferred)` only when the shape disagrees with `provider` AND the
/// evidence is [`ShapeEvidence::Conclusive`].
/// Test: `conclusive_mismatch_ignores_a_probable_shape`,
/// `conclusive_mismatch_still_flags_a_slug_on_bedrock`.
pub fn conclusive_shape_mismatch(provider: ProviderId, model: &str) -> Option<ProviderId> {
    match classify_model_shape(model) {
        Some((inferred, ShapeEvidence::Conclusive)) if inferred != provider => Some(inferred),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrouter_slug_is_openrouter() {
        // The #6114 id itself: an OpenRouter slug that used to fall through to
        // whatever default provider the path carried.
        assert_eq!(
            infer_provider_from_model_shape("anthropic/claude-opus-4.8"),
            Some(ProviderId::OpenRouter)
        );
        assert_eq!(
            infer_provider_from_model_shape("openai/gpt-5.4-mini-20260317"),
            Some(ProviderId::OpenRouter)
        );
    }

    #[test]
    fn us_profile_id_is_bedrock() {
        for id in [
            "us.anthropic.claude-sonnet-4-6",
            "eu.anthropic.claude-haiku-4-5",
            "global.anthropic.claude-opus-4-8",
        ] {
            assert_eq!(
                infer_provider_from_model_shape(id),
                Some(ProviderId::Bedrock),
                "{id} must read as Bedrock"
            );
        }
    }

    #[test]
    fn dotted_vendor_id_is_bedrock() {
        assert_eq!(
            infer_provider_from_model_shape("anthropic.claude-sonnet-4-6"),
            Some(ProviderId::Bedrock)
        );
        assert_eq!(
            infer_provider_from_model_shape("amazon.nova-pro-v1:0"),
            Some(ProviderId::Bedrock)
        );
    }

    #[test]
    fn fireworks_native_id_is_fireworks() {
        assert_eq!(
            infer_provider_from_model_shape("accounts/fireworks/models/llama-v3p1-70b-instruct"),
            Some(ProviderId::Fireworks),
            "the fireworks-native id must not be read as an OpenRouter slug"
        );
    }

    #[test]
    fn bare_ids_are_ambiguous() {
        for id in ["", "   ", "claude-opus-4-5-20260101", "gpt-5.4-mini"] {
            assert_eq!(
                infer_provider_from_model_shape(id),
                None,
                "{id:?} must stay ambiguous so the configured default decides"
            );
        }
    }

    #[test]
    fn unknown_dotted_prefix_is_ambiguous() {
        // A dot alone is not a Bedrock signal — version-dotted slugs are common.
        assert_eq!(infer_provider_from_model_shape("llama-3.1-70b"), None);
    }

    #[test]
    fn mismatch_flags_openrouter_slug_on_bedrock() {
        assert_eq!(
            shape_mismatch(ProviderId::Bedrock, "anthropic/claude-opus-4.8"),
            Some(ProviderId::OpenRouter),
            "#6114: this pair must be reportable, not silently run on Bedrock"
        );
        assert_eq!(
            shape_mismatch(ProviderId::OpenRouter, "us.anthropic.claude-sonnet-4-6"),
            Some(ProviderId::Bedrock)
        );
    }

    #[test]
    fn mismatch_is_none_when_shape_agrees() {
        assert_eq!(
            shape_mismatch(ProviderId::Bedrock, "us.anthropic.claude-sonnet-4-6"),
            None
        );
        assert_eq!(
            shape_mismatch(
                ProviderId::Fireworks,
                "accounts/fireworks/models/llama-v3p1-70b-instruct"
            ),
            None
        );
    }

    #[test]
    fn mismatch_is_none_for_ambiguous_id() {
        assert_eq!(shape_mismatch(ProviderId::Bedrock, "claude-opus-4-8"), None);
        assert_eq!(shape_mismatch(ProviderId::Anthropic, ""), None);
    }

    #[test]
    fn slug_and_profile_evidence_is_conclusive() {
        for id in [
            "anthropic/claude-opus-4.8",
            "us.anthropic.claude-sonnet-4-6",
            "accounts/fireworks/models/llama-v3p1-70b-instruct",
        ] {
            let (_, evidence) = classify_model_shape(id).expect("{id} must classify");
            assert_eq!(
                evidence,
                ShapeEvidence::Conclusive,
                "{id} belongs to exactly one catalogue"
            );
        }
    }

    #[test]
    fn dotted_vendor_evidence_is_probable() {
        let (provider, evidence) =
            classify_model_shape("anthropic.claude-sonnet-4-6").expect("must classify");
        assert_eq!(provider, ProviderId::Bedrock);
        assert_eq!(
            evidence,
            ShapeEvidence::Probable,
            "a vendor-name match is a guess, not a catalogue fact"
        );
    }

    /// #6114 (code-critic MEDIUM 2): the dotted-vendor guess must not veto an
    /// operator's explicit routing prefix.
    ///
    /// Why: `openrouter/anthropic.claude-x` states the provider for this call.
    /// Turning it into a hard error because `anthropic` appears in a Bedrock
    /// vendor list would break a working configuration on a guess.
    /// What: asserts the conclusive-only check stays silent on a `Probable`
    /// shape, in both directions.
    /// Test: this test itself; the resolver-level proof lives in both consumer
    /// crates' `*_prefix_survives_a_probable_*` tests.
    #[test]
    fn conclusive_mismatch_ignores_a_probable_shape() {
        assert_eq!(
            conclusive_shape_mismatch(ProviderId::OpenRouter, "anthropic.claude-sonnet-4-6"),
            None,
            "an explicit prefix outranks the dotted-vendor guess"
        );
        assert_eq!(
            conclusive_shape_mismatch(ProviderId::Anthropic, "amazon.nova-pro-v1:0"),
            None
        );
        // The plain form still reports it — a config default does NOT outrank it.
        assert_eq!(
            shape_mismatch(ProviderId::OpenRouter, "anthropic.claude-sonnet-4-6"),
            Some(ProviderId::Bedrock)
        );
    }

    #[test]
    fn conclusive_mismatch_still_flags_a_slug_on_bedrock() {
        assert_eq!(
            conclusive_shape_mismatch(ProviderId::Bedrock, "anthropic/claude-opus-4.8"),
            Some(ProviderId::OpenRouter),
            "nothing makes an OpenRouter slug runnable on Bedrock"
        );
        assert_eq!(
            conclusive_shape_mismatch(ProviderId::OpenRouter, "us.anthropic.claude-sonnet-4-6"),
            Some(ProviderId::Bedrock)
        );
        assert_eq!(
            conclusive_shape_mismatch(
                ProviderId::OpenRouter,
                "accounts/fireworks/models/llama-v3p1-70b-instruct"
            ),
            Some(ProviderId::Fireworks)
        );
    }
}
