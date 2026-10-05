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
//! [`mask_account_ids`] hides the account id of any ARN in display and log
//! text; the id sent to AWS is never masked.
//! Test: `arn_tests.rs`.

use std::borrow::Cow;

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

/// Whether `model` is an application-inference-profile ARN, which no price
/// table can price because its id does not name the model it runs.
///
/// Test: `footer_masks_the_account_and_says_unpriced_for_an_application_profile`.
pub(crate) fn is_unpriced(model: &str) -> bool {
    parse(model).is_some_and(|arn| arn.priced_model().is_none())
}

/// What replaces an ARN's 12-digit account id in display and log text.
const ACCOUNT_MASK: &str = "****";

/// Replace the 12-digit account id of every ARN inside `text` with `****`.
///
/// Why: an ARN's account id identifies the AWS account behind a review, and it
/// would otherwise reach the posted PR footer, error strings and logs (#9200,
/// architect ruling 2026-10-05). AWS's own error messages can quote other ARNs
/// (an `sts` assumed role, say), so every ARN is masked, not only Bedrock's.
/// What: scans for `arn:<partition>:<service>:<region>:<12 digits>:` anywhere
/// in `text` and masks the digits. A region may be empty (`iam`, `sts`); an
/// empty account (a `foundation-model` ARN) has nothing to mask. Text with no
/// such span is returned borrowed and unchanged. Byte offsets only ever land
/// on ASCII bytes, so no input can panic it.
/// Test: `mask_hides_the_account_of_every_arn_form`,
/// `mask_leaves_plain_ids_and_non_arn_text_unchanged`,
/// `mask_survives_malformed_and_truncated_input`.
pub(crate) fn mask_account_ids(text: &str) -> Cow<'_, str> {
    let mut out = String::new();
    let mut copied = 0;
    let mut search = 0;
    while let Some(offset) = text[search..].find("arn:") {
        let start = search + offset;
        search = start + "arn:".len();
        if let Some((from, to)) = account_span(&text[start..]) {
            out.push_str(&text[copied..start + from]);
            out.push_str(ACCOUNT_MASK);
            copied = start + to;
            search = copied;
        }
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

/// The byte range of the account id in `arn`, which starts with `arn:`.
///
/// What: three `:`-terminated fields of `[a-z0-9-]` — partition and service
/// non-empty, region possibly empty — then exactly 12 ASCII digits and a `:`.
fn account_span(arn: &str) -> Option<(usize, usize)> {
    let bytes = arn.as_bytes();
    let mut i = "arn:".len();
    for min_len in [1, 1, 0] {
        let start = i;
        while bytes
            .get(i)
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        {
            i += 1;
        }
        if i - start < min_len || bytes.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
    }
    let end = i + 12;
    let account = bytes.get(i..end)?;
    (account.iter().all(u8::is_ascii_digit) && bytes.get(end) == Some(&b':')).then_some((i, end))
}

#[cfg(test)]
#[path = "arn_tests.rs"]
mod tests;
