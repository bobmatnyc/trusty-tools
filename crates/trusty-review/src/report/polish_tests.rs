//! Tests for the post-render polish pass (#2342 comment strip + omit-empty).
//!
//! Why: the polish pass is what turns a filled skeleton into a lean document —
//! its comment stripping (dataset markers preserved), marker-row dropping,
//! empty-section collapse, and gaps-list regeneration must be pinned precisely.
//! What: exercises `strip_template_comments` and `polish` on crafted markdown.
//! Test: included as `#[cfg(test)] mod tests` from `polish.rs`.

use super::{polish, strip_template_comments};
use crate::report::fill::HONESTY_MARKER;

/// Why: inline instructional comments must never reach the output.
/// What: asserts a mid-line and a whole-line instructional comment are removed.
/// Test: this test itself.
#[test]
fn strips_instructional_comments() {
    let input = "# Title\n<!-- One paragraph, deal-relevant -->\nBody text <!-- extend to 4-5 rows --> here.\n";
    let out = strip_template_comments(input);
    assert!(!out.contains("One paragraph"));
    assert!(!out.contains("extend to"));
    assert!(out.contains("# Title"));
    assert!(out.contains("Body text"));
    assert!(out.contains("here."));
}

/// Why: `<!-- dataset: … -->` markers are semantic (downstream tooling lifts
/// tables by them) and MUST survive stripping.
/// What: asserts a dataset marker is preserved while a sibling comment is gone.
/// Test: this test itself.
#[test]
fn keeps_dataset_markers() {
    let input = "<!-- dataset: loc | chart: bar -->\n| a |\n<!-- drop me -->\n";
    let out = strip_template_comments(input);
    assert!(out.contains("<!-- dataset: loc | chart: bar -->"));
    assert!(!out.contains("drop me"));
}

/// Why: comments inside fenced code blocks are out of scope — code samples must
/// render verbatim.
/// What: asserts a comment inside a ``` fence is preserved.
/// Test: this test itself.
#[test]
fn keeps_comments_in_code_fences() {
    let input = "```\n<!-- keep me in code -->\n```\n";
    let out = strip_template_comments(input);
    assert!(out.contains("<!-- keep me in code -->"));
}

/// Why: an instructional comment may embed a literal `<!-- dataset: … -->`
/// example; balancing must strip the whole outer comment, leaking no residue.
/// What: asserts a comment with an embedded close is fully removed.
/// Test: this test itself.
#[test]
fn strips_comment_with_embedded_close() {
    let input = "A <!-- Add `<!-- dataset: x -->` blocks here --> B\n";
    let out = strip_template_comments(input);
    assert!(out.contains("A "));
    assert!(out.contains(" B"));
    assert!(!out.contains("blocks here"));
    assert!(!out.contains("dataset:"));
}

/// Why: a metadata row whose value is only the honesty marker is empty
/// scaffolding — it must be dropped and the field recorded under Data gaps, while
/// rows with real values survive.
/// What: polishes a two-column table with one filled and one marker row.
/// Test: this test itself.
#[test]
fn drops_marker_rows() {
    let input = format!(
        "## 1. Metadata\n\n| Field | Value |\n|---|---|\n| Vendor | trusty-review |\n| Client | {HONESTY_MARKER} |\n\n## 8. Gaps & Caveats\n\n- x\n"
    );
    let out = polish(&input);
    assert!(out.contains("| Vendor | trusty-review |"));
    assert!(!out.contains(&format!("Client | {HONESTY_MARKER}")));
    assert!(out.contains("Data gaps: Client."));
}

/// Why: a section left with no data after row/block dropping must collapse to a
/// single line, not stand as an empty skeleton.
/// What: polishes a document with an empty Section 6 between two headings and
/// asserts the collapse line plus the gap entry.
/// Test: this test itself.
#[test]
fn collapses_empty_section() {
    let input = "## 6. Risk Registers\n\n## 7. Appendix\n\ncontent\n\n## 8. Gaps & Caveats\n\nplaceholder\n";
    let out = polish(input);
    assert!(out.contains("_No data available"));
    assert!(out.contains("Data gaps: 6. Risk Registers"));
    // Section 7 has content and is preserved.
    assert!(out.contains("content"));
}

/// Why: the Gaps & Caveats section must list every dropped field compactly rather
/// than as a wall of markers.
/// What: asserts dropped fields from multiple sources appear in one Data gaps line
/// and the original placeholder bullets are gone.
/// Test: this test itself.
#[test]
fn gaps_section_lists_dropped_fields() {
    let input = format!(
        "## 1. Meta\n\n| Field | Value |\n|---|---|\n| Client | {HONESTY_MARKER} |\n| Analyst | {HONESTY_MARKER} |\n\n## 8. Gaps & Caveats\n\n- {HONESTY_MARKER}\n- {HONESTY_MARKER}\n"
    );
    let out = polish(&input);
    // Both dropped fields are named in the compact Data gaps line; the exact tail
    // may also carry the now-empty section (all its rows were markers here).
    let gaps_line = out
        .lines()
        .find(|l| l.starts_with("Data gaps:"))
        .expect("gaps line");
    assert!(gaps_line.contains("Client"));
    assert!(gaps_line.contains("Analyst"));
    assert!(!out.contains(HONESTY_MARKER));
}

/// Why: with no gaps at all, the section states so rather than an empty list.
/// What: polishes a document whose only table row is fully populated.
/// Test: this test itself.
#[test]
fn gaps_section_reports_no_gaps() {
    let input = "## 1. Meta\n\n| Field | Value |\n|---|---|\n| Vendor | trusty-review |\n\n## 8. Gaps & Caveats\n\nplaceholder\n";
    let out = polish(input);
    assert!(out.contains("No material data gaps"));
}
