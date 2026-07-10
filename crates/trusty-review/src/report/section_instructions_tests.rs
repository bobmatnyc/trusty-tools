//! Tests for the section-instruction registry (#2357 layered-instructions).
//!
//! Why: the whole layering system (generic → template override → analyst
//! overlay) depends on `resolve` always returning a complete map and on id
//! validation rejecting typos rather than silently accepting them.
//! What: covers id validation, default coverage of every section, and the
//! resolve merge (no overrides, and a partial override touching only one id).
//! Test: included as `#[cfg(test)] mod tests` from `section_instructions.rs`.

use std::collections::BTreeMap;

use super::*;

/// Why: the parser must accept exactly the three shipped ids and reject others.
/// What: asserts each `ALL_SECTION_IDS` entry validates and a typo does not.
/// Test: this test itself.
#[test]
fn validates_known_ids() {
    for id in ALL_SECTION_IDS {
        assert!(is_valid_section_id(id), "'{id}' must be a valid section id");
    }
    assert!(!is_valid_section_id("executive_summry"));
    assert!(!is_valid_section_id(""));
}

/// Why: every synthesized section must have a shipped default — a missing one
/// would silently fall back to an empty instruction.
/// What: asserts `default_instruction` returns non-empty text for every id.
/// Test: this test itself.
#[test]
fn defaults_cover_all_sections() {
    for id in ALL_SECTION_IDS {
        let text = default_instruction(id).unwrap_or_else(|| panic!("missing default for {id}"));
        assert!(!text.trim().is_empty());
    }
    assert!(default_instruction("not-a-section").is_none());
}

/// Why: a bare `--synthesize` run (no template override) must use the generic
/// defaults verbatim.
/// What: `resolve` with an empty override map equals the defaults.
/// Test: this test itself.
#[test]
fn resolve_uses_defaults_when_no_overrides() {
    let resolved = resolve(&BTreeMap::new());
    assert_eq!(resolved.len(), ALL_SECTION_IDS.len());
    for id in ALL_SECTION_IDS {
        assert_eq!(resolved[*id], default_instruction(id).unwrap());
    }
}

/// Why: a template overriding ONE section must not disturb the others.
/// What: an override for `executive_summary` only changes that entry.
/// Test: this test itself.
#[test]
fn resolve_partial_override_only_replaces_that_section() {
    let mut overrides = BTreeMap::new();
    overrides.insert(
        EXECUTIVE_SUMMARY.to_string(),
        "CAST-flavoured text".to_string(),
    );
    let resolved = resolve(&overrides);
    assert_eq!(resolved[EXECUTIVE_SUMMARY], "CAST-flavoured text");
    assert_eq!(resolved[TOP_RISKS], default_instruction(TOP_RISKS).unwrap());
    assert_eq!(
        resolved[FINDING_ELABORATION],
        default_instruction(FINDING_ELABORATION).unwrap()
    );
}
