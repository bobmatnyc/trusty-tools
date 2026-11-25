//! The model vendor a Bedrock model id names (#10759).
//!
//! Why: Anthropic models on Bedrock need a cross-region inference profile,
//! but other vendors' open-weight models (Qwen3-Coder, Mistral Devstral) run
//! on demand under their bare foundation-model id, and most of them reject a
//! forced `toolChoice`. Both decisions need the vendor, read one way.
//! What: [`bedrock_vendor`] and [`is_non_anthropic_foundation_id`], built on
//! the shared `classify_model_shape` so the vendor list has one definition.
//! Test: `non_anthropic_bedrock_reviewer_routes_through_converse_and_parses`,
//! `bedrock_vendor_reads_every_id_shape`.

use trusty_common::inference::{ProviderId, ShapeEvidence, classify_model_shape};

use super::arn;
use crate::llm::BEDROCK_MODEL_PREFIX;

/// The vendor that owns Anthropic models on Bedrock.
pub(crate) const ANTHROPIC_VENDOR: &str = "anthropic";

/// Whether `id` is a dotted `vendor.model` Bedrock foundation-model id.
fn is_foundation_id(id: &str) -> bool {
    matches!(
        classify_model_shape(id),
        Some((ProviderId::Bedrock, ShapeEvidence::Probable))
    )
}

/// The vendor segment of a Bedrock model id, or `None` when the id hides it.
///
/// What: strips a `bedrock/` prefix, reads the model out of an ARN, then
/// takes the vendor of `vendor.model` or of `<region>.vendor.model`. An
/// application-inference-profile ARN, or an id with no known vendor, is
/// `None`.
/// Test: `bedrock_vendor_reads_every_id_shape`.
pub(crate) fn bedrock_vendor(model: &str) -> Option<&str> {
    let model = model.strip_prefix(BEDROCK_MODEL_PREFIX).unwrap_or(model);
    let id = match arn::parse(model) {
        None => model,
        Some(parsed) => parsed.priced_model()?,
    };
    let foundation = if is_foundation_id(id) {
        id
    } else {
        id.split_once('.')?.1
    };
    if !is_foundation_id(foundation) {
        return None;
    }
    foundation.split_once('.').map(|(vendor, _)| vendor)
}

/// Whether `model_id` is a bare foundation-model id from a vendor other than
/// Anthropic, which Bedrock Converse runs on demand without a profile.
///
/// Why: the provider used to require a `us.`-style profile on every id, an
/// Anthropic-only rule that refused Qwen3-Coder and Devstral (#10759).
/// What: `true` for `qwen.…` or `mistral.…`; `false` for `anthropic.…` (which
/// still needs a profile), profiled ids, ARNs and unknown vendors.
/// Test: `non_anthropic_bedrock_reviewer_routes_through_converse_and_parses`.
pub(crate) fn is_non_anthropic_foundation_id(model_id: &str) -> bool {
    is_foundation_id(model_id)
        && model_id
            .split_once('.')
            .is_some_and(|(vendor, _)| vendor != ANTHROPIC_VENDOR)
}
