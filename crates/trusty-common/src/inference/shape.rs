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

/// Infer the provider whose namespace `model` belongs to, from its shape alone.
///
/// Why: a resolver that cannot tell `anthropic/claude-opus-4.8` from
/// `us.anthropic.claude-opus-4-8` has no way to notice that the id and the
/// provider about to run it disagree, so it runs the wrong model silently
/// (#6114).
/// What: `Some(provider)` when the spelling is unambiguous —
/// `accounts/fireworks/models/…` is Fireworks, any other `vendor/model`
/// slash-form is an OpenRouter slug, and a region-profiled or dotted
/// `vendor.model` id is Bedrock. `None` when the id is bare and could belong to
/// several providers (`claude-opus-4-5-20260101`, `gpt-5.4-mini`), which is the
/// case where a caller's configured default legitimately decides.
///
/// The input must already have had the caller's own routing prefix stripped —
/// see the module docs.
/// Test: `openrouter_slug_is_openrouter`, `us_profile_id_is_bedrock`,
/// `dotted_vendor_id_is_bedrock`, `fireworks_native_id_is_fireworks`,
/// `bare_ids_are_ambiguous`.
pub fn infer_provider_from_model_shape(model: &str) -> Option<ProviderId> {
    let id = model.trim();
    if id.is_empty() {
        return None;
    }
    if id.starts_with(FIREWORKS_NATIVE_PREFIX) {
        return Some(ProviderId::Fireworks);
    }
    if id.contains('/') {
        return Some(ProviderId::OpenRouter);
    }
    if BEDROCK_INFERENCE_PROFILE_PREFIXES
        .iter()
        .any(|pfx| id.starts_with(pfx))
    {
        return Some(ProviderId::Bedrock);
    }
    let vendor = id.split('.').next().unwrap_or(id);
    if vendor.len() < id.len() && BEDROCK_VENDOR_SEGMENTS.contains(&vendor) {
        return Some(ProviderId::Bedrock);
    }
    None
}

/// Report the provider `model`'s shape names, when that is not `provider`.
///
/// Why: this is the reconciliation a resolver owes its caller — the run must
/// fail naming both the id and the provider rather than execute a different
/// model than the one requested (#6114).
/// What: `Some(inferred)` when [`infer_provider_from_model_shape`] names a
/// provider other than `provider`; `None` when they agree or the id is
/// ambiguous.
/// Test: `mismatch_flags_openrouter_slug_on_bedrock`,
/// `mismatch_is_none_when_shape_agrees`, `mismatch_is_none_for_ambiguous_id`.
pub fn shape_mismatch(provider: ProviderId, model: &str) -> Option<ProviderId> {
    match infer_provider_from_model_shape(model) {
        Some(inferred) if inferred != provider => Some(inferred),
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
}
