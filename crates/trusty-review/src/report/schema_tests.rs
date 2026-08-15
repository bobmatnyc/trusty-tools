//! Tests for the shared schema-tag parse (#5747).
//!
//! Why: both artifact loaders decide cross-process compatibility from this one
//! function, so its edge cases are worth pinning in one place rather than
//! re-deriving them from each loader's behaviour.
//! What: covers the tag spellings the two producers write and the shapes that
//! resolve to no major at all.
//! Test: included as `#[cfg(test)] mod schema_tests` from `schema.rs`.

use super::major;

#[test]
fn plain_and_v_prefixed_majors_parse() {
    assert_eq!(major("v0"), Some(0));
    assert_eq!(major("0"), Some(0));
    assert_eq!(major("v9"), Some(9));
    assert_eq!(major("12"), Some(12));
}

/// A later producer's added field arrives as a bumped MINOR. It must resolve to
/// the major this build already reads, or `#[serde(default)]` buys nothing.
#[test]
fn a_newer_minor_resolves_to_its_major() {
    assert_eq!(major("v0.3"), Some(0));
    assert_eq!(major("0.3.1"), Some(0));
    assert_eq!(major("v0-rc1"), Some(0));
    assert_eq!(major("v0+build7"), Some(0));
}

/// `analyze-live-v0` is a real tag this crate writes for the in-memory live
/// path, and it resolves to no major — which is why a loader that meets it in a
/// file must refuse rather than assume.
#[test]
fn an_uninterpretable_tag_has_no_major() {
    assert_eq!(major(""), None);
    assert_eq!(major("v"), None);
    assert_eq!(major("analyze-live-v0"), None);
    assert_eq!(major("analyze-2027"), None);
    assert_eq!(major("corpus-v9"), None);
}
