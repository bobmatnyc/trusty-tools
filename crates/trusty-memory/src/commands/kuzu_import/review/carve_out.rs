//! The verbatim-admit carve-out and its high-entropy threshold (#8624, PR-C).
//!
//! Why: owner ruling 2026-09-25 lets a reviewer admit a refused row without
//! redaction only for `mixed_case_alnum` rows and non-high-entropy
//! `base64_blob` rows; the drawer carries `kuzu-review:override` so the
//! admission can be retracted.
//! What: a span is high-entropy when its ROUNDED `ent_tenths` is at least
//! [`HIGH_ENTROPY_TENTHS`] and its length is at least
//! [`HIGH_ENTROPY_MIN_LEN`] (Architect ruling 08:48Z). [`span_admissible`]
//! admits a `mixed_case_alnum` span whatever its shape and a `base64_blob`
//! span only when it is not high-entropy; every other rule is refused.
//! [`row_admissible`] requires every span of the row to pass and refuses a
//! row with no span.
//! Test: `carve_out_threshold_is_pinned_at_45_tenths_and_24_chars`,
//! `carve_out_admits_low_entropy_and_refuses_at_the_threshold`,
//! `carve_out_admits_a_high_entropy_mixed_case_alnum_span`.

use crate::engine::filter::SecretRule;

/// 4.5 bits/char, in the tenths `classify_secret` reports.
pub(crate) const HIGH_ENTROPY_TENTHS: u16 = 45;
/// A span shorter than this is never high-entropy.
pub(crate) const HIGH_ENTROPY_MIN_LEN: usize = 24;

/// Whether a span of `len` chars at `ent_tenths` is high-entropy.
pub(crate) fn is_high_entropy(len: usize, ent_tenths: u16) -> bool {
    ent_tenths >= HIGH_ENTROPY_TENTHS && len >= HIGH_ENTROPY_MIN_LEN
}

/// Whether one span, by its rule label and shape, may be admitted verbatim.
pub(crate) fn span_admissible(rule: &str, len: usize, ent_tenths: u16) -> bool {
    if rule == SecretRule::MixedCaseAlnum.label() {
        true
    } else if rule == SecretRule::Base64Blob.label() {
        !is_high_entropy(len, ent_tenths)
    } else {
        false
    }
}

/// Whether a row whose spans are `(rule label, len, ent_tenths)` may be
/// admitted verbatim: at least one span, and every span admissible.
///
/// Test: `carve_out_admits_low_entropy_and_refuses_at_the_threshold`.
pub(crate) fn row_admissible<'a>(spans: impl IntoIterator<Item = (&'a str, usize, u16)>) -> bool {
    let mut any = false;
    for (rule, len, ent) in spans {
        if !span_admissible(rule, len, ent) {
            return false;
        }
        any = true;
    }
    any
}
