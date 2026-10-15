//! Span-level secret detection: where each hit is, and which rule fired (#8623).
//!
//! Why: [`super::check_secret`] answers yes or no for a whole string, and
//! [`super::find_secret_token`] previews only the first hit. The kuzu import
//! review (#277) needs every hit's byte range and rule, so it can report and
//! later mask each one. trusty-memory used to re-derive the rule from the
//! token's shape with its own copy of the prefix lists; this module is the one
//! rule source both crates read.
//! What: [`find_secret_spans`] reports one [`SecretSpan`] per hit. The verdict
//! for each token is [`super::looks_like_secret`]'s, unchanged; this module
//! only names the branch that fired ([`classify_token`]) and adds the
//! [`SecretRule::PrivateKeyBlock`] region rule. [`candidate_tokens`] is the one
//! tokenizer, shared with [`super::find_secret_token`].
//! Test: `spans_match_check_secret_over_the_filter_corpus`,
//! `spans_match_check_secret_over_generated_input`,
//! `secret_filter_version_pins_the_rule_inputs`.

use std::ops::Range;

use super::private_key::private_key_blocks;
use super::{
    carries_secret_prefix, is_aws_access_key_id, is_bare_google_doc_id, looks_like_secret,
    names_google_document,
};

/// Version of the rule set behind [`find_secret_spans`] and
/// [`super::check_secret`].
///
/// Why (#8623): a ledger that records "screened clean" must be able to tell
/// which detector said so, so a stricter detector can re-screen what an older
/// one admitted.
/// What: bump it in the same change as any edit that can alter which spans are
/// reported or which rule they carry — a prefix, a length, an allowlist, a
/// branch. The fingerprint test fails until the bump is made.
/// Test: `secret_filter_version_pins_the_rule_inputs`.
pub const SECRET_FILTER_VERSION: u32 = 1;

/// Which detector rule flagged a span. Only the label is ever reported; the
/// rule carries no part of the secret.
///
/// Why (#8623): moved from trusty-memory's kuzu import screen, where it was
/// guessed from the token's shape, so the label now comes from the branch that
/// actually fired.
/// What: one variant per detection branch, ordered as the detector consults
/// them. [`SecretRule::label`] is the stable report string.
/// Test: `each_rule_is_labelled_by_the_branch_that_fired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
// #8624: a new rule or field is not a breaking change for other crates.
#[non_exhaustive]
pub enum SecretRule {
    /// A provider key prefix (`sk-`, `ghp_`, …) at a token or delimiter start.
    ProviderPrefix,
    /// An AWS access key id (`AKIA…`/`ASIA…`, 20 uppercase alphanumerics).
    AwsKeyId,
    /// A PEM `-----BEGIN …PRIVATE KEY` region, terminated or not.
    PrivateKeyBlock,
    /// A JSON Web Token: `eyJ…` header and payload segments, base64url.
    Jwt,
    /// A base64 blob carrying `+`, `/` or `=`.
    Base64Blob,
    /// A mixed-case alphanumeric run with a digit, of credential length.
    MixedCaseAlnum,
}

impl SecretRule {
    /// Every rule, in declaration order.
    pub const ALL: [SecretRule; 6] = [
        Self::ProviderPrefix,
        Self::AwsKeyId,
        Self::PrivateKeyBlock,
        Self::Jwt,
        Self::Base64Blob,
        Self::MixedCaseAlnum,
    ];

    /// The label a report prints.
    pub fn label(self) -> &'static str {
        match self {
            Self::ProviderPrefix => "provider_prefix",
            Self::AwsKeyId => "aws_key_id",
            Self::PrivateKeyBlock => "private_key_block",
            Self::Jwt => "jwt",
            Self::Base64Blob => "base64_blob",
            Self::MixedCaseAlnum => "mixed_case_alnum",
        }
    }
}

/// One secret hit: its byte range in the screened string and its rule.
///
/// What: `range` is a byte range whose ends are char boundaries, so
/// `&content[span.range.clone()]` never panics. A token span covers the token
/// with its surrounding punctuation trimmed; a [`SecretRule::PrivateKeyBlock`]
/// span covers the whole block, widened to whole whitespace-separated chunks.
/// Test: `spans_are_ascending_char_boundary_ranges`.
#[derive(Debug, Clone, PartialEq, Eq)]
// #8624: see `SecretRule`.
#[non_exhaustive]
pub struct SecretSpan {
    pub range: Range<usize>,
    pub rule: SecretRule,
}

/// The tokens the detector judges, each with its byte range in `content`.
///
/// Why (#4312, #8623): one tokenizer for [`super::find_secret_token`] and
/// [`find_secret_spans`], so the verdict and the spans cannot disagree on
/// where a token starts or ends.
/// What: splits on whitespace or a backtick, trims every char that is not
/// ASCII alphanumeric, `-` or `_` from both ends, and skips empty tokens.
pub(crate) fn candidate_tokens(content: &str) -> impl Iterator<Item = (Range<usize>, &str)> {
    let base = content.as_ptr() as usize;
    content
        .split(|c: char| c.is_whitespace() || c == '`')
        .filter_map(move |raw| {
            let tok =
                raw.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_')));
            if tok.is_empty() {
                return None;
            }
            // `tok` is a sub-slice of `content`, so its address offset is its
            // byte offset, and both ends sit on char boundaries.
            let start = tok.as_ptr() as usize - base;
            Some((start..start + tok.len(), tok))
        })
}

/// True when `tok` is a bare Google document id that `content` names as one,
/// the single refused shape the screen lets through (#8589).
///
/// What: `names_doc` caches the context test, which is asked at most once per
/// screened string and only for a token of the id shape.
pub(crate) fn admitted_as_doc_id(content: &str, tok: &str, names_doc: &mut Option<bool>) -> bool {
    is_bare_google_doc_id(tok) && *names_doc.get_or_insert_with(|| names_google_document(content))
}

/// True when `token` lies wholly inside one of `blocks`.
pub(crate) fn inside_block(blocks: &[Range<usize>], token: &Range<usize>) -> bool {
    blocks
        .iter()
        .any(|b| b.start <= token.start && token.end <= b.end)
}

/// True when `tok` has the shape of a JSON Web Token: two or three non-empty
/// `.`-separated base64url segments, the first two opening with `eyJ` (the
/// encoding of `{"`). Two segments is an unsigned token whose trailing `.` the
/// tokenizer trimmed.
fn is_jwt_shaped(tok: &str) -> bool {
    let segments: Vec<&str> = tok.split('.').collect();
    (2..=3).contains(&segments.len())
        && segments[..2].iter().all(|s| s.starts_with("eyJ"))
        && segments.iter().all(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        })
}

/// The rule of a token [`looks_like_secret`] flagged, read from the branch
/// that fired.
///
/// What: the detector consults the provider prefixes, then the AWS key-id
/// shape, then the base64 branch (which needs `+`, `/` or `=`), then the
/// mixed-case branch (whose charset excludes all three). A flagged token is
/// therefore a provider key, an AWS id, a base64 blob exactly when it carries
/// one of those symbols, or else a mixed-case run. A JWT is a mixed-case run
/// relabelled. Total over flagged tokens: there is no "unknown" rule.
fn rule_of_flagged(tok: &str) -> SecretRule {
    if carries_secret_prefix(&tok.to_ascii_lowercase()) {
        SecretRule::ProviderPrefix
    } else if is_aws_access_key_id(tok) {
        SecretRule::AwsKeyId
    } else if is_jwt_shaped(tok) {
        SecretRule::Jwt
    } else if tok.contains(['+', '/', '=']) {
        SecretRule::Base64Blob
    } else {
        SecretRule::MixedCaseAlnum
    }
}

/// The rule that flags `tok` on its own, or `None` when the detector admits it.
///
/// Why (#8623): the verdict stays [`looks_like_secret`]'s, so this function
/// can mislabel a hit but can never admit one the detector refuses.
/// What: `None` exactly when [`looks_like_secret`] is false; otherwise the
/// branch label from [`rule_of_flagged`]. Context rules (the #8589 Google
/// document-id skip, PEM blocks) are applied by [`find_secret_spans`], not here.
/// Test: `each_rule_is_labelled_by_the_branch_that_fired`,
/// `spans_match_check_secret_over_the_filter_corpus`.
pub(crate) fn classify_token(tok: &str) -> Option<SecretRule> {
    looks_like_secret(tok).then(|| rule_of_flagged(tok))
}

/// Every secret hit in `content`, as byte ranges with the rule that fired.
///
/// Why (#8623): the kuzu import review needs each hit, not the first one's
/// preview. The result is empty exactly when [`super::check_secret`] accepts
/// `content`, and its first span is the hit [`super::find_secret_token`]
/// previews.
/// What: reports each PEM private-key block as one span, then each token
/// outside a block that [`classify_token`] flags, minus a bare Google document
/// id that `content` names as one (#8589). Tokens inside a block are not
/// reported again. Spans are disjoint, ascending by start, and end on char
/// boundaries. Infallible: it never panics on any `&str`.
/// Test: `spans_match_check_secret_over_the_filter_corpus`,
/// `spans_match_check_secret_over_generated_input`,
/// `spans_are_ascending_char_boundary_ranges`, `private_key_block_is_one_span`,
/// `jwt_is_labelled_jwt`, `bare_doc_id_named_by_context_is_not_a_span`.
pub fn find_secret_spans(content: &str) -> Vec<SecretSpan> {
    let blocks = private_key_blocks(content);
    let mut names_doc: Option<bool> = None;
    let mut spans: Vec<SecretSpan> = blocks
        .iter()
        .map(|b| SecretSpan {
            range: b.clone(),
            rule: SecretRule::PrivateKeyBlock,
        })
        .collect();
    for (range, tok) in candidate_tokens(content) {
        if inside_block(&blocks, &range) {
            continue;
        }
        let Some(rule) = classify_token(tok) else {
            continue;
        };
        if admitted_as_doc_id(content, tok, &mut names_doc) {
            continue;
        }
        spans.push(SecretSpan { range, rule });
    }
    spans.sort_by_key(|s| s.range.start);
    spans
}

#[cfg(test)]
#[path = "spans_tests.rs"]
mod tests;
