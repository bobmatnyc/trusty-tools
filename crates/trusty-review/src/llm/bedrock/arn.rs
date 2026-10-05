//! Bedrock model ARNs as trusty-review model ids (#9200).
//!
//! Why: an operator can name a Bedrock model by ARN — an application inference
//! profile tagged for cost allocation, a system inference profile, or a
//! foundation model. trusty-common's shape check routes those ARNs to
//! Bedrock; this module gives the Bedrock provider the three facts it needs
//! from one: the region the resource lives in, and which model id, if any, the
//! pricing table can price.
//! What: [`parse`] splits a well-formed ARN into region, resource kind and
//! resource name. Well-formedness is decided by
//! [`trusty_common::inference::classify_model_shape`], so this crate and the
//! shared router never disagree about which strings are ARNs.
//! Test: `arn_tests.rs`.

use trusty_common::inference::{ProviderId, ShapeEvidence, classify_model_shape};

/// Every Bedrock model ARN opens with this partition and service.
const ARN_HEAD: &str = "arn:aws:bedrock:";

/// The model resource an ARN names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArnKind {
    /// `application-inference-profile/<id>`: an operator-made profile whose id
    /// does not say which model it runs.
    ApplicationInferenceProfile,
    /// `inference-profile/<profile-id>`: a system profile such as
    /// `us.anthropic.claude-sonnet-4-6`.
    InferenceProfile,
    /// `foundation-model/<model-id>`: one model in one region.
    FoundationModel,
}

/// A parsed Bedrock model ARN, borrowing from the id it was parsed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BedrockArn<'a> {
    /// The AWS region field, e.g. `us-west-2`.
    pub(crate) region: &'a str,
    /// Which model resource the ARN names.
    pub(crate) kind: ArnKind,
    /// The resource id after the `/`.
    pub(crate) name: &'a str,
}

impl<'a> BedrockArn<'a> {
    /// The model id the pricing table should price, or `None` when the ARN
    /// hides it.
    ///
    /// Why: a system inference profile's or foundation model's ARN ends in the
    /// same id the pricing table already keys on, while an application
    /// inference profile's id is opaque — pricing it as any model would be a
    /// guess, and pricing it as unknown would be a silent $0.
    /// What: the resource name for `InferenceProfile` and `FoundationModel`;
    /// `None` for `ApplicationInferenceProfile`.
    /// Test: `arn_pricing_resolves_the_embedded_model_or_reports_unpriced`.
    pub(crate) fn priced_model(&self) -> Option<&'a str> {
        match self.kind {
            ArnKind::ApplicationInferenceProfile => None,
            ArnKind::InferenceProfile | ArnKind::FoundationModel => Some(self.name),
        }
    }
}

/// Parse `id` as a Bedrock model ARN, or `None` when it is not one.
///
/// Why: the region and the priced model both live inside the ARN; every other
/// id keeps the handling it had before ARN support, so a `None` here must mean
/// "not an ARN".
/// What: accepts only what `classify_model_shape` calls conclusive Bedrock and
/// that opens with `arn:aws:bedrock:`, then reads
/// `arn:aws:bedrock:<region>:<account>:<kind>/<name>`. `<name>` may contain
/// `:` (`amazon.nova-pro-v1:0`), so the id splits into six fields at most.
/// Surrounding whitespace is rejected rather than trimmed, because the id is
/// sent to Converse verbatim.
/// Test: `parse_reads_region_kind_and_name_from_each_arn_form`,
/// `parse_rejects_plain_ids_and_malformed_arns`.
pub(crate) fn parse(id: &str) -> Option<BedrockArn<'_>> {
    if !id.starts_with(ARN_HEAD) || id.trim() != id {
        return None;
    }
    if classify_model_shape(id) != Some((ProviderId::Bedrock, ShapeEvidence::Conclusive)) {
        return None;
    }
    let mut fields = id.splitn(6, ':');
    let region = fields.nth(3)?;
    let resource = fields.nth(1)?;
    let (kind, name) = resource.split_once('/')?;
    let kind = match kind {
        "application-inference-profile" => ArnKind::ApplicationInferenceProfile,
        "inference-profile" => ArnKind::InferenceProfile,
        "foundation-model" => ArnKind::FoundationModel,
        _ => return None,
    };
    Some(BedrockArn { region, kind, name })
}

#[cfg(test)]
#[path = "arn_tests.rs"]
mod tests;
