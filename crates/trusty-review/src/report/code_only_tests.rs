//! Unit tests for the code-only template transform (#6669).
//!
//! Why: the rules this module enforces are what keeps a code-only report
//! honest — a heading is never dropped, a non-code section never reads as a
//! failed measurement, and a partial section never reads as validated.
//! What: one test per rule in [`super::apply`], plus the two marker-parsing
//! edge cases a template author can hit.
//! Test: this file.

use super::*;

/// Why: a render that did not ask for code-only must be untouched — the
/// markers are ordinary comments the polish pass already removes, so this
/// transform owes byte-identical output.
/// What: a template carrying both region kinds, transformed with `false`.
/// Test: this test itself.
#[test]
fn disabled_returns_the_source_unchanged() {
    let tpl = "## A\n<!-- code_only:non_code a corpus -->\n| x |\n<!-- code_only:end -->\n";
    assert_eq!(apply(tpl, false), tpl);
}

/// Why: the whole point — a section nothing in a repository can answer states
/// its boundary rather than rendering an empty table.
/// What: the region's body is gone, the heading survives, and the boundary
/// sentence names the marker's own reason.
/// Test: this test itself.
#[test]
fn non_code_body_is_replaced_by_the_boundary() {
    let tpl = "## Peer Benchmark\n\n<!-- code_only:non_code CAST's proprietary corpus -->\n\
               | Criterion | Quartile |\n|---|---|\n| TQI | {{q}} |\n<!-- code_only:end -->\n\n## Next\n";
    let out = apply(tpl, true);
    assert!(
        out.contains("## Peer Benchmark"),
        "the heading survives: {out}"
    );
    assert!(
        out.contains(OUT_OF_SCOPE_LEAD),
        "the boundary is stated: {out}"
    );
    assert!(
        out.contains("requires CAST's proprietary corpus"),
        "the marker's own reason is used: {out}"
    );
    assert!(
        !out.contains("{{q}}"),
        "no placeholder survives to be filled: {out}"
    );
    assert!(out.contains("## Next"), "the next section survives: {out}");
}

/// Why: a partial section IS code-derived, so deleting its data would throw
/// away a real measurement; what it lacks is cross-validation.
/// What: the body is kept verbatim and the provenance line follows it.
/// Test: this test itself.
#[test]
fn partial_keeps_its_body_and_gains_the_note() {
    let tpl = "### 6.7 Remediation Economics\n\n<!-- code_only:partial -->\n| Tier | Cost |\n\
               |---|---|\n| Immediate | {{c}} |\n<!-- code_only:end -->\n";
    let out = apply(tpl, true);
    assert!(out.contains("| Immediate | {{c}} |"), "body kept: {out}");
    assert!(out.contains(PARTIAL_NOTE), "note appended: {out}");
    assert!(
        out.find("| Immediate").unwrap() < out.find(PARTIAL_NOTE).unwrap(),
        "the note follows the data it qualifies: {out}"
    );
}

/// Why: a template typo must never truncate a report — failing open here costs
/// one unmarked section; failing closed would cost every section after it.
/// What: an opening marker with no `code_only:end` leaves the text alone.
/// Test: this test itself.
#[test]
fn an_unclosed_region_is_passed_through() {
    let tpl = "## A\n<!-- code_only:non_code a corpus -->\n| x |\n";
    let out = apply(tpl, true);
    assert!(out.contains("| x |"), "the body survives: {out}");
    assert!(
        !out.contains(OUT_OF_SCOPE_LEAD),
        "no boundary is invented for a region that has no end: {out}"
    );
}

/// Why: a marker long enough to need wrapping in the template must not render
/// its newlines into the middle of a sentence.
/// What: a two-line reason renders as one space-joined sentence.
/// Test: this test itself.
#[test]
fn a_marker_reason_is_whitespace_normalized() {
    let tpl = "<!-- code_only:non_code interviews with\n     the delivery team -->\nx\n\
               <!-- code_only:end -->\n";
    let out = apply(tpl, true);
    assert!(
        out.contains("requires interviews with the delivery team,"),
        "{out}"
    );
}

/// Why: an author who marks a section without spelling out why still gets a
/// stated boundary rather than a dangling "requires ,".
/// What: the default reason fills in.
/// Test: this test itself.
#[test]
fn a_non_code_marker_with_no_reason_uses_the_default() {
    let out = apply(
        "<!-- code_only:non_code -->\nx\n<!-- code_only:end -->\n",
        true,
    );
    assert!(
        out.contains("requires interviews or operational data,"),
        "{out}"
    );
}

/// Why: `parse_section_instructions` and the dataset-marker preservation both
/// scan the same comment stream; this transform must not disturb either.
/// What: an `instruct:` block and a `dataset:` marker survive verbatim.
/// Test: this test itself.
#[test]
fn other_comments_are_left_alone() {
    let tpl = "<!-- instruct:top_risks Be terse. -->\n<!-- dataset: x | chart: bar -->\n";
    assert_eq!(apply(tpl, true), tpl);
}

/// Why: two regions in one document is the normal case (the CAST template has
/// five), so the scanner must resume correctly after each `end`.
/// What: both regions are transformed and the text between them survives.
/// Test: this test itself.
#[test]
fn consecutive_regions_are_each_transformed() {
    let tpl = "<!-- code_only:non_code a corpus -->\nA\n<!-- code_only:end -->\nMIDDLE\n\
               <!-- code_only:partial -->\nB\n<!-- code_only:end -->\n";
    let out = apply(tpl, true);
    assert!(out.contains("MIDDLE"), "{out}");
    assert!(!out.contains("\nA\n"), "the non-code body is gone: {out}");
    assert!(out.contains("B"), "the partial body is kept: {out}");
    assert_eq!(out.matches(PARTIAL_NOTE).count(), 1, "{out}");
    assert_eq!(out.matches(OUT_OF_SCOPE_LEAD).count(), 1, "{out}");
}

/// Why: nesting is the one malformed shape that used to fail SILENTLY. The
/// outer region closed at the INNER region's `code_only:end`, so the rest of
/// the outer body flowed into the report as literal template text and the
/// outer's real end marker dangled — and no `warn!` fired, because an end
/// marker HAD been found. Failing open on one section is this module's
/// doctrine; rewriting a different span than the author marked is not.
/// What: an opening marker inside an open region leaves the OUTER region
/// untransformed — its whole body survives and no boundary is stated for it.
/// Test: this test itself.
#[test]
fn a_nested_region_leaves_the_outer_region_untransformed() {
    let tpl = "## Outer\n<!-- code_only:non_code a corpus -->\nOUTER-A\n\
               <!-- code_only:partial -->\nINNER\n<!-- code_only:end -->\n\
               OUTER-B\n<!-- code_only:end -->\n## Next\n";
    let out = apply(tpl, true);
    assert!(
        out.contains("OUTER-A"),
        "the outer body before the nested marker survives: {out}"
    );
    assert!(
        out.contains("OUTER-B"),
        "the outer body after the nested region survives rather than being cut \
         adrift by the inner end marker: {out}"
    );
    assert!(out.contains("INNER"), "the inner body survives: {out}");
    assert!(
        !out.contains(OUT_OF_SCOPE_LEAD),
        "no boundary is stated for a region whose extent is ambiguous: {out}"
    );
    assert!(out.contains("## Next"), "the next section survives: {out}");
}
