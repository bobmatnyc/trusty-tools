//! Tests for the post-render polish pass (#2342 comment strip + omit-empty).
//!
//! Why: the polish pass is what turns a filled skeleton into a lean document —
//! its comment stripping (dataset markers preserved), marker-row dropping,
//! empty-section collapse, and gaps-list regeneration must be pinned precisely.
//! What: exercises `strip_template_comments` and `polish` on crafted markdown.
//! Test: included as `#[cfg(test)] mod tests` from `polish.rs`.

use super::{polish, polish_with_gaps, strip_template_comments};
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

/// Why: a template's `<!-- instruct:<section_id> ... -->` section-instruction
/// override (#2357 layered instructions) is an ordinary HTML comment to this
/// pass — it must be stripped exactly like any other instructional comment,
/// never leaking into a generated report even though `template::
/// parse_section_instructions` reads it from the raw pre-strip text.
/// What: a single-line and a multi-line `instruct:` block are both removed.
/// Test: this test itself.
#[test]
fn strips_instruct_override_blocks() {
    let input = "# Title\n<!-- instruct:executive_summary Lead with TQI. -->\nBody\n<!-- instruct:top_risks\nMulti-line\noverride body\n-->\nMore body.\n";
    let out = strip_template_comments(input);
    assert!(!out.contains("instruct:"));
    assert!(!out.contains("Lead with TQI"));
    assert!(!out.contains("Multi-line"));
    assert!(out.contains("# Title"));
    assert!(out.contains("Body"));
    assert!(out.contains("More body."));
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

/// Why: this is the direct regression test for the findings-rendering fix
/// (#2357 wave-3.2 defect #3) — a fenced code block containing a blank line
/// must survive the FULL polish pass (comment strip + omit-empty + section
/// collapse) byte-for-byte, never splicing a "No data available" line inside
/// or around it, and never having the fence markers themselves stripped.
/// What: a heading followed by a fenced block whose body has a blank line in
/// the middle; asserts the whole fence (including the blank line) survives
/// `polish()` unchanged and no collapse line appears anywhere near it.
/// Test: this test itself.
#[test]
fn fenced_code_with_blank_line_untouched() {
    let input = "## Findings\n\n1. **Some finding** — a description\n- **Evidence** (`a.rs:1`):\n```\nfunction f() {\n\n  return 1;\n}\n```\n";
    let out = polish(input);
    assert!(
        out.contains("```\nfunction f() {\n\n  return 1;\n}\n```"),
        "fenced block with its blank line must survive verbatim: {out}"
    );
    assert!(
        !out.contains("No data available"),
        "no gap splice around fenced content: {out}"
    );
}

/// Why: `omit_empty`'s bullet/table/marker-paragraph checks must never fire
/// INSIDE a fence — a fenced line that merely LOOKS like a marker bullet or a
/// table row must pass through untouched.
/// What: a fence body containing a line that looks like a marker-only bullet
/// (`- not stated in source data`) and a `|`-prefixed line; both survive
/// verbatim because they are inside the fence.
/// Test: this test itself.
#[test]
fn fenced_code_resembling_markers_is_untouched() {
    let input = format!("## Section\n\n```\n- {HONESTY_MARKER}\n| a | b |\n```\n");
    let out = polish(&input);
    assert!(out.contains(&format!("- {HONESTY_MARKER}")));
    assert!(out.contains("| a | b |"));
}

/// Why: a ```mermaid fenced block (#2366) must be OPAQUE to `polish` exactly like
/// an evidence fence — a `title`/`bar`-looking line, a `%%` comment, or a bare
/// heading-looking token inside it must never be interpreted as a marker, table
/// row, or heading and never trigger a spliced collapse line.
/// What: a section whose body is a mermaid block (with an inner blank line and a
/// `#`-looking comment) survives `polish()` verbatim with no gap splice.
/// Test: this test itself.
#[test]
fn mermaid_fence_is_opaque_to_polish() {
    let input = "## 7. Appendix\n\n\
        <!-- dataset: d | chart: bar | x: a | y: b -->\n\
        | A | B |\n|---|---|\n| x | 1 |\n\n\
        ```mermaid\n\
        xychart-beta\n\
        %% a comment\n\
        \n\
        x-axis [\"x\"]\n\
        bar [1]\n\
        ```\n";
    let out = polish(input);
    assert!(
        out.contains("```mermaid\nxychart-beta\n%% a comment\n\nx-axis [\"x\"]\nbar [1]\n```"),
        "mermaid fence survives polish verbatim: {out}"
    );
    assert!(
        !out.contains("No data available"),
        "no splice around mermaid: {out}"
    );
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
    assert!(out.contains("Data gaps: 1 unpopulated field/section — Client."));
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
    // One gap, so the line counts it in the singular (#5319).
    assert!(out.contains("Data gaps: 1 unpopulated field/section — 6. Risk Registers."));
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

/// Parse the `Data gaps:` line into its declared count and its listed items.
///
/// Why: #5319's whole point is that those two must agree, so the regression test
/// reads them back out of the rendered line exactly as a reader would.
/// What: returns `(declared_count, items)` from the single `Data gaps:` line.
fn parse_gaps_line(out: &str) -> (usize, Vec<&str>) {
    let line = out
        .lines()
        .find(|l| l.starts_with("Data gaps:"))
        .expect("gaps line");
    let (count_part, list_part) = line
        .trim_start_matches("Data gaps:")
        .split_once('—')
        .expect("count and list are separated by an em dash");
    let count = count_part
        .split_whitespace()
        .next()
        .expect("count token")
        .parse::<usize>()
        .expect("count token parses as a number");
    let items: Vec<&str> = list_part
        .trim()
        .trim_end_matches('.')
        .split(';')
        .map(str::trim)
        .collect();
    (count, items)
}

/// Why: #5319 — the line rendered as `Data gaps: 2. Executive Summary, …`, whose
/// leading section number a diligence reader parses as a count of two ahead of a
/// sixteen-name list. Nothing counted anything; the "2" was the first label's own
/// template numbering, and comma-joining it behind a colon manufactured a number
/// that contradicted the list.
/// What: collapses three numbered template sections, then asserts the declared
/// count equals the number of items the same line goes on to list.
/// Test: this test itself.
#[test]
fn gaps_line_count_matches_its_own_list() {
    let input = "## 2. Executive Summary\n\n## 6.2 Open-Source / CVE Exposure\n\n\
                 ## 6.3 License / IP Risk\n\n## 7. Graph-Ready Data Appendix\n\ncontent\n\n\
                 ## 8. Gaps & Caveats\n\nplaceholder\n";
    let out = polish(input);
    let (count, items) = parse_gaps_line(&out);
    assert_eq!(
        count,
        items.len(),
        "declared count must equal the listed items: {out}"
    );
    assert_eq!(count, 3, "three sections collapsed: {out}");
    // The section that supplied the misread "2." is still named in full.
    assert!(items.contains(&"2. Executive Summary"), "{out}");
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

/// Why: live-QA wave-2 defect #2 — a `##` parent heading immediately followed
/// by a populated `###` child must NOT be collapsed to "_No data available_"
/// just because the OLD single-pass scan terminated the parent's body scan at
/// the first heading of ANY level (finding zero lines before it).
/// What: polishes `## 4. Per-Application Scorecard` immediately followed by a
/// populated `### 4.N. Acme` child, and asserts the collapse line never appears
/// between the two headings while the child's real content survives.
/// Test: this test itself.
#[test]
fn parent_with_populated_child_not_collapsed() {
    let input = "## 4. Per-Application Scorecard\n\n### 4.N. Acme\n\n| Field | Value |\n|---|---|\n| Technology stack | Rust |\n\n## 5. Findings by Severity\n\nplaceholder\n\n## 8. Gaps & Caveats\n\nx\n";
    let out = polish(input);
    // The child's real content is present.
    assert!(out.contains("| Technology stack | Rust |"));
    // No collapse line was spuriously inserted between the two headings.
    let between = out
        .split("## 4. Per-Application Scorecard")
        .nth(1)
        .and_then(|s| s.split("### 4.N. Acme").next())
        .expect("region between parent and child heading");
    assert!(
        !between.contains("No data available"),
        "parent falsely collapsed above populated child: {between:?}"
    );
}

/// Why: live-QA wave-2 defect #4 — a bold pseudo-heading (`**Health-Factor
/// Scores**`) whose table was entirely dropped (all rows were honesty markers)
/// rendered with nothing beneath it and no collapse note, because the polish
/// pass only recognised `#`-headings as boundaries.
/// What: polishes a `###` section containing a populated bold pseudo-heading
/// (`**Profile**`) followed by an orphaned, empty one (`**Health-Factor
/// Scores**`); asserts the orphaned one collapses with a gap note while the
/// populated one survives untouched.
/// Test: this test itself.
#[test]
fn collapses_orphaned_bold_pseudo_heading() {
    let input = "### 4.N. Acme\n\n**Profile**\n\n| Field | Value |\n|---|---|\n| Technology stack | Rust |\n\n**Health-Factor Scores**\n\n## 5. Findings by Severity\n\nplaceholder\n\n## 8. Gaps & Caveats\n\nx\n";
    let out = polish(input);
    assert!(
        out.contains("| Technology stack | Rust |"),
        "Profile survives"
    );
    assert!(
        out.contains("No data available"),
        "orphaned bold heading collapses"
    );
    assert!(out.contains("Data gaps:"));
    let gaps_line = out
        .lines()
        .find(|l| l.starts_with("Data gaps:"))
        .expect("gaps line");
    assert!(gaps_line.contains("Health-Factor Scores"));
}

/// Why: a bold pseudo-heading must not falsely terminate the SHALLOWER `###`
/// section's own has-content check — the enclosing `### 4.N.` section is
/// non-empty here purely because of the nested `**Profile**` table, proving
/// the recursive has-content propagation works across the pseudo-heading
/// boundary in both directions.
/// What: polishes a document where the ONLY content anywhere under `### 4.N.`
/// is inside a bold pseudo-heading, and asserts the `###` heading itself is not
/// collapsed.
/// Test: this test itself.
#[test]
fn heading_with_only_pseudo_heading_content_not_collapsed() {
    let input = "### 4.N. Acme\n\n**Profile**\n\n| Field | Value |\n|---|---|\n| Technology stack | Rust |\n\n## 5. Findings by Severity\n\nplaceholder\n\n## 8. Gaps & Caveats\n\nx\n";
    let out = polish(input);
    let between = out
        .split("### 4.N. Acme")
        .nth(1)
        .and_then(|s| s.split("## 5. Findings by Severity").next())
        .expect("region under the ### heading");
    assert!(!between.contains("No data available"));
    assert!(between.contains("Rust"));
}

// ─── Declared (named) gaps — #5239 ──────────────────────────────────────────

/// A minimal document carrying the Gaps & Caveats heading the polish pass
/// rewrites.
fn doc_with_gaps_section() -> &'static str {
    "# R\n\n## 1. Metadata\n\n| Field | Value |\n|---|---|\n| Client | Acme |\n\n\
     ## 8. Gaps & Caveats\n\n- {{gap_1}}\n\n---\n*footer*\n"
}

/// Why: a stage that did not run must appear in the report as its own
/// statement, not be folded into the field-level `Data gaps:` list where it
/// would read as a missing table cell.
/// Test: itself.
#[test]
fn declared_gaps_render_as_bullets() {
    let declared = vec![
        "Stage `jira sync` did not complete — ticket correlation is not assessed.".to_string(),
        "trusty-analyze unreachable — no analysis pass ran for: Northwind Web.".to_string(),
    ];
    let out = polish_with_gaps(doc_with_gaps_section(), &declared);

    for line in &declared {
        assert!(out.contains(&format!("- {line}")), "missing {line}: {out}");
    }
    // The auto-collected list still renders, after the named bullets.
    let bullet_at = out.find("- Stage `jira sync`").expect("bullet present");
    if let Some(list_at) = out.find("Data gaps:") {
        assert!(bullet_at < list_at, "named gaps lead the section: {out}");
    }
}

/// Why: every existing caller passes no declared gaps; their output must not
/// move by a single byte.
/// Test: itself.
#[test]
fn empty_declared_gaps_are_byte_identical() {
    let doc = doc_with_gaps_section();
    assert_eq!(polish(doc), polish_with_gaps(doc, &[]));
}

/// Why: a custom template with no Gaps & Caveats heading must not silently
/// swallow a named gap — that is the exact failure mode #5239 exists to close.
/// Test: itself.
#[test]
fn declared_gaps_appended_when_template_has_no_section() {
    let doc = "# R\n\n## 1. Metadata\n\nnothing here\n";
    let declared = vec!["Stage `collect` did not complete.".to_string()];

    let out = polish_with_gaps(doc, &declared);

    assert!(out.contains("## Gaps & Caveats"), "{out}");
    assert!(out.contains("- Stage `collect` did not complete."), "{out}");
    // Unchanged when nothing was declared.
    assert_eq!(polish_with_gaps(doc, &[]), polish(doc));
}
