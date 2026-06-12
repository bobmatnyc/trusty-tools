//! Unit tests for [`EdgeKind`], [`EdgeKindError`], and related helpers.
//!
//! Why: extracted from `edge_kind.rs` to keep that file under the 500-line cap
//! (issue #610) while preserving full test coverage for the Phase E validation
//! additions (issue #818) and the union-coverage gate (issue #815).
//! What: covers `score_multiplier`, `tag`/`from_tag` round-trips, serde
//! round-trips, union coverage, the validated `custom` constructor, and
//! malformed-tag rejection.
//! Test: this file IS the test suite for `edge_kind.rs`.

use super::{EdgeKind, EdgeKindError};

#[test]
fn edge_kind_score_multiplier_known_values() {
    assert!((EdgeKind::Implements.score_multiplier() - 0.85).abs() < 1e-6);
    assert!((EdgeKind::UsesType.score_multiplier() - 0.75).abs() < 1e-6);
    assert!((EdgeKind::TestedBy.score_multiplier() - 0.80).abs() < 1e-6);
    assert!((EdgeKind::Documents.score_multiplier() - 0.65).abs() < 1e-6);
    assert!((EdgeKind::ReferencesConcept.score_multiplier() - 0.60).abs() < 1e-6);
    // Phase D data-flow variants (issue #817).
    assert!((EdgeKind::Writes.score_multiplier() - 0.90).abs() < 1e-6);
    assert!((EdgeKind::Reads.score_multiplier() - 0.80).abs() < 1e-6);
    assert!((EdgeKind::AccessesResource.score_multiplier() - 0.75).abs() < 1e-6);
    // Default wildcard branch (0.70 is intentional for untuned variants).
    assert!((EdgeKind::CallsFunction.score_multiplier() - 0.70).abs() < 1e-6);
    assert!((EdgeKind::Calls.score_multiplier() - 0.70).abs() < 1e-6);
    // Phase E escape hatch (issue #818): Custom uses conservative 0.70.
    assert!((EdgeKind::Custom("foo".to_string()).score_multiplier() - 0.70).abs() < 1e-6);
}

/// `Custom("foo")` ⇄ `"custom:foo"` round-trip (issue #818).
#[test]
fn edge_kind_custom_tag_round_trip() {
    let v = EdgeKind::Custom("foo".to_string());
    let tag = v.tag();
    assert_eq!(tag.as_ref(), "custom:foo");
    let back = EdgeKind::from_tag(&tag).expect("Custom should parse from custom: tag");
    assert_eq!(back, v);
}

/// A bare unrecognized PascalCase tag does NOT become Custom (issue #818,
/// Option H: only `"custom:"`-prefixed tags round-trip).
#[test]
fn edge_kind_unknown_bare_tag_returns_none() {
    assert!(EdgeKind::from_tag("UnknownFutureEdge").is_none());
    assert!(EdgeKind::from_tag("SomeTypo").is_none());
}

/// Named tags do NOT get confused with the `custom:` prefix.
#[test]
fn edge_kind_named_tag_not_treated_as_custom() {
    let tag = EdgeKind::CallsFunction.tag();
    assert_eq!(tag.as_ref(), "CallsFunction");
    let back = EdgeKind::from_tag(&tag).expect("named tag round-trips");
    assert_eq!(back, EdgeKind::CallsFunction);
}

/// Every variant round-trips through `serde_json` (guards the on-disk format).
#[test]
fn edge_kind_serde_round_trip() {
    let variants = vec![
        // Phase A/B/C
        EdgeKind::CallsFunction,
        EdgeKind::CalledByFunction,
        EdgeKind::Implements,
        EdgeKind::UsesType,
        EdgeKind::Derives,
        EdgeKind::ModuleContains,
        EdgeKind::ReExports,
        EdgeKind::RaisesError,
        EdgeKind::Configures,
        EdgeKind::TestedBy,
        EdgeKind::TestUsesFixture,
        EdgeKind::CoOccursInTest,
        EdgeKind::Documents,
        EdgeKind::ReferencesConcept,
        EdgeKind::Aliases,
        EdgeKind::ErrorDescribes,
        // Language-neutral structural (former KgEdgeKind + graph::EdgeKind)
        EdgeKind::Contains,
        EdgeKind::Imports,
        EdgeKind::Exports,
        EdgeKind::Calls,
        EdgeKind::Extends,
        EdgeKind::References,
        EdgeKind::Tests,
        EdgeKind::DependsOn,
        EdgeKind::GeneratedFrom,
        EdgeKind::RuntimeObservationFor,
        // Phase D data-flow (issue #817)
        EdgeKind::Reads,
        EdgeKind::Writes,
        EdgeKind::AccessesResource,
        // Phase E escape hatch (issue #818)
        EdgeKind::Custom("reads_table".to_string()),
    ];
    for v in &variants {
        let json = serde_json::to_string(v).expect("serialize EdgeKind");
        let back: EdgeKind = serde_json::from_str(&json).expect("deserialize EdgeKind");
        assert_eq!(*v, back, "round-trip failed for {json}");
    }
}

/// Assert that the canonical enum covers the full prior union (issue #815):
/// every variant is constructible and its `tag()` is non-empty and
/// round-trips through `from_tag`.
#[test]
fn edge_kind_union_coverage() {
    let variants: Vec<EdgeKind> = vec![
        EdgeKind::CallsFunction,
        EdgeKind::CalledByFunction,
        EdgeKind::Implements,
        EdgeKind::UsesType,
        EdgeKind::Derives,
        EdgeKind::ModuleContains,
        EdgeKind::ReExports,
        EdgeKind::RaisesError,
        EdgeKind::Configures,
        EdgeKind::TestedBy,
        EdgeKind::TestUsesFixture,
        EdgeKind::CoOccursInTest,
        EdgeKind::Documents,
        EdgeKind::ReferencesConcept,
        EdgeKind::Aliases,
        EdgeKind::ErrorDescribes,
        EdgeKind::Contains,
        EdgeKind::Imports,
        EdgeKind::Exports,
        EdgeKind::Calls,
        EdgeKind::Extends,
        EdgeKind::References,
        EdgeKind::Tests,
        EdgeKind::DependsOn,
        EdgeKind::GeneratedFrom,
        EdgeKind::RuntimeObservationFor,
        EdgeKind::Reads,
        EdgeKind::Writes,
        EdgeKind::AccessesResource,
        EdgeKind::Custom("any".to_string()),
    ];
    // 30 variants (29 named + Custom).
    assert_eq!(
        variants.len(),
        30,
        "update this count when adding a variant"
    );
    for v in &variants {
        let tag = v.tag();
        assert!(!tag.is_empty(), "tag() must be non-empty for {v:?}");
        let back = EdgeKind::from_tag(&tag)
            .unwrap_or_else(|| panic!("from_tag round-trip failed for {tag:?}"));
        assert_eq!(back, *v, "round-trip failed for {tag:?}");
    }
}

/// `EdgeKind::custom` rejects empty labels and labels with colons (issue #818).
#[test]
fn edge_kind_custom_constructor_rejects_invalid() {
    // Empty label.
    assert_eq!(EdgeKind::custom(""), Err(EdgeKindError::EmptyLabel));
    // Label containing ':' — would produce double-prefix on disk.
    assert_eq!(
        EdgeKind::custom("custom:foo"),
        Err(EdgeKindError::LabelContainsColon("custom:foo".to_string()))
    );
    assert_eq!(
        EdgeKind::custom("foo:bar"),
        Err(EdgeKindError::LabelContainsColon("foo:bar".to_string()))
    );
    // Valid label succeeds.
    assert_eq!(
        EdgeKind::custom("reads_table"),
        Ok(EdgeKind::Custom("reads_table".to_string()))
    );
    assert_eq!(
        EdgeKind::custom("my-rel"),
        Ok(EdgeKind::Custom("my-rel".to_string()))
    );
}

/// `from_tag` rejects malformed `"custom:"`-prefixed tags (issue #818).
#[test]
fn edge_kind_from_tag_rejects_malformed_custom() {
    // Empty suffix: "custom:" → None.
    assert!(
        EdgeKind::from_tag("custom:").is_none(),
        "empty custom suffix must return None"
    );
    // Suffix with colon: "custom:foo:bar" → None (double-prefix).
    assert!(
        EdgeKind::from_tag("custom:foo:bar").is_none(),
        "colon-containing custom suffix must return None"
    );
    // "custom:custom:foo" produced by naive Custom("custom:foo") → None.
    assert!(
        EdgeKind::from_tag("custom:custom:foo").is_none(),
        "double-prefix must return None"
    );
    // A valid custom tag still parses.
    assert_eq!(
        EdgeKind::from_tag("custom:reads_table"),
        Some(EdgeKind::Custom("reads_table".to_string()))
    );
}
