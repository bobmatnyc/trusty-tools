//! The secret screen for store-supplied strings, with its rule class (#277).
//!
//! Why (#277 MEDIUM-1, MEDIUM-3): every string a kuzu store supplies that
//! becomes palace content, a tag or a triple must pass the palace's own secret
//! screen, and an operator reading the report must be able to tell which kind
//! of credential the screen saw without the report printing it.
//! What: [`screen`] returns the rule of the first span `trusty_common`'s
//! [`find_secret_spans`] reports — the refusal and the rule are both decided
//! there (#8623), and this module keeps no rule logic of its own.
//! [`SecretRule`] is trusty-common's, re-exported for the report.
//! Test: `screen_labels_each_detector_rule_class`,
//! `store_supplied_tags_and_triples_pass_the_secret_screen`.

use std::collections::BTreeMap;

pub use crate::engine::filter::SecretRule;
use crate::engine::filter::{check_secret, find_secret_spans};

/// Refusal counts by rule label.
pub type RuleTally = BTreeMap<&'static str, usize>;

/// Add one refusal under `rule` to `tally`.
pub fn tally(t: &mut RuleTally, rule: SecretRule) {
    *t.entry(rule.label()).or_default() += 1;
}

/// The rule class of the secret screen's refusal of `s`; `None` when it passes.
///
/// What: the first span's rule. trusty-common pins that the span list is
/// empty exactly when `check_secret` accepts; should that ever break, a string
/// `check_secret` refuses is still refused here (#8623, fail closed), under the
/// mixed-case label.
/// Test: `screen_labels_each_detector_rule_class`.
pub fn screen(s: &str) -> Option<SecretRule> {
    match find_secret_spans(s).first() {
        Some(span) => Some(span.rule),
        None => check_secret(s).err().map(|_| SecretRule::MixedCaseAlnum),
    }
}
