//! Schema-tag parsing shared by the report's artifact loaders (#5747).
//!
//! Why: `report/ticketing.rs` and `report/metrics.rs` both read a JSON artifact
//! written by a separately versioned, separately installed producer, and both
//! must decide from a `schema_version` string whether this build can read the
//! file at all. #5405 added that check to the ticketing loader with a private
//! `schema_major`; #5747 needs the identical parse for metrics, and a second
//! independent copy of a rule about cross-process compatibility is exactly the
//! kind of silent drift the common-entry-point convention exists to prevent.
//!
//! What: [`major`], the one parse both loaders call.
//!
//! ## What this deliberately does not own
//!
//! The POLICY stays with each loader, because the two artifacts differ on it.
//! Both supply their own supported-major constant, and each decides for itself
//! what an absent tag means — ticketing refuses one (every producer writes the
//! tag), metrics accepts one as v0 (no producer in this workspace writes the
//! artifact at all, and the field shipped documented as informational). Folding
//! that decision in here would force one artifact's history onto the other.
//!
//! Test: `super::schema_tests`.

/// The major component of a schema tag, if it has one.
///
/// Why: the tag is a document the producer wrote, not a number, so a loader
/// reads only what it needs to decide compatibility — a `v0.3` written by a
/// later producer must resolve to the same major as `v0`, or every additive
/// change to the artifact would require a coordinated release of both binaries.
/// What: strips a leading `v`, takes everything up to the first `.`, `-`, or
/// `+`, and parses it. `None` for an empty, absent, or non-numeric tag — the
/// caller decides whether that is fatal.
/// Test: `super::schema_tests::{plain_and_v_prefixed_majors_parse,
/// a_newer_minor_resolves_to_its_major, an_uninterpretable_tag_has_no_major}`.
pub(crate) fn major(tag: &str) -> Option<u32> {
    tag.strip_prefix('v')
        .unwrap_or(tag)
        .split(['.', '-', '+'])
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod schema_tests;
