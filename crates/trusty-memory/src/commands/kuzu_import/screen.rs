//! The secret screen for store-supplied strings, with its rule class (#277).
//!
//! Why (#277 MEDIUM-1, MEDIUM-3): every string a kuzu store supplies that
//! becomes palace content, a tag or a triple must pass the palace's own secret
//! screen, and an operator reading the report must be able to tell which kind
//! of credential the screen saw without the report printing it.
//! What: [`screen`] asks `trusty_common`'s [`check_secret`] whether a string
//! is refused — that decision is never made here — and, when it is, labels
//! the first offending token with a [`SecretRule`]. The detector reports a
//! yes/no and a redacted preview, not the branch that fired, so the label is
//! read from the token's shape: a provider prefix, an AWS key id, a base64
//! symbol, else a mixed-case alphanumeric run. A refused string whose tokens
//! each pass alone is labelled [`SecretRule::Other`].
//! Test: `screen_labels_each_detector_rule_class`,
//! `store_supplied_tags_and_triples_pass_the_secret_screen`.

use std::collections::BTreeMap;

use trusty_common::memory_core::filter::{check_secret, find_secret_token};

/// Provider key prefixes the detector refuses at a token or delimiter start.
const PROVIDER_PREFIXES: &[&str] = &[
    "sk-",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
];
/// AWS access key id prefixes; the id is 20 upper-case letters and digits.
const AWS_PREFIXES: &[&str] = &["AKIA", "ASIA"];
const AWS_KEY_ID_LEN: usize = 20;

/// Which detector rule refused a string. Only the label is ever reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SecretRule {
    ProviderPrefix,
    AwsKeyId,
    Base64Blob,
    MixedCaseAlnum,
    Other,
}

impl SecretRule {
    /// The label the report prints.
    pub fn label(self) -> &'static str {
        match self {
            Self::ProviderPrefix => "provider_prefix",
            Self::AwsKeyId => "aws_key_id",
            Self::Base64Blob => "base64_blob",
            Self::MixedCaseAlnum => "mixed_case_alnum",
            Self::Other => "other",
        }
    }
}

/// Refusal counts by rule label.
pub type RuleTally = BTreeMap<&'static str, usize>;

/// Add one refusal under `rule` to `tally`.
pub fn tally(t: &mut RuleTally, rule: SecretRule) {
    *t.entry(rule.label()).or_default() += 1;
}

/// The rule class of the secret screen's refusal of `s`; `None` when it passes.
///
/// Test: `screen_labels_each_detector_rule_class`.
pub fn screen(s: &str) -> Option<SecretRule> {
    check_secret(s).err()?;
    // The same tokenisation `find_secret_token` applies, so each candidate is
    // judged by the detector alone before it is labelled.
    let token = s
        .split(|c: char| c.is_whitespace() || c == '`')
        .map(|raw| {
            raw.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_')))
        })
        .find(|t| find_secret_token(t).is_some());
    Some(token.map_or(SecretRule::Other, classify))
}

/// Label a token the detector refused.
fn classify(token: &str) -> SecretRule {
    let lower = token.to_ascii_lowercase();
    let at_boundary = |p: &str| {
        lower
            .match_indices(p)
            .any(|(i, _)| i == 0 || lower[..i].ends_with(['-', '_', '.']))
    };
    if PROVIDER_PREFIXES.iter().any(|p| at_boundary(p)) {
        SecretRule::ProviderPrefix
    } else if token.len() == AWS_KEY_ID_LEN
        && AWS_PREFIXES.iter().any(|p| token.starts_with(p))
        && token
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        SecretRule::AwsKeyId
    } else if token.contains(['+', '/', '=']) {
        SecretRule::Base64Blob
    } else {
        SecretRule::MixedCaseAlnum
    }
}
