//! Unit tests for inline per-line review comments (#1414).
//!
//! Why: the finding→inline-comment mapping and the off-diff fallback are the
//! load-bearing correctness logic of #1414 — a mistake either posts comments on
//! lines GitHub rejects (failing the whole review) or drops findings silently.
//! What: exercises diff-index construction, on-diff mapping, off-diff fallback,
//! and the no-line fallback against representative unified diffs.
//! Test: this module.

use super::*;
use crate::models::{Effort, Finding};

/// A small two-file unified diff with added, context, and removed lines.
fn sample_diff() -> &'static str {
    "\
diff --git a/src/db.rs b/src/db.rs
index 1111111..2222222 100644
--- a/src/db.rs
+++ b/src/db.rs
@@ -10,3 +10,4 @@ fn query() {
 let conn = pool.get();
-let sql = format!(\"SELECT {}\", input);
+let sql = \"SELECT ?\";
+bind(input);
 run(sql);
diff --git a/src/util.rs b/src/util.rs
index 3333333..4444444 100644
--- a/src/util.rs
+++ b/src/util.rs
@@ -1,2 +1,3 @@
 fn a() {}
+fn b() {}
 fn c() {}
"
}

fn finding_at(file: &str, line: Option<u32>) -> Finding {
    let mut f = Finding::new(
        file,
        "security",
        "SQL injection via string interpolation",
        "Use a parameterised query",
        0.9,
        Effort::Medium,
    );
    f.line = line;
    f
}

#[test]
fn parse_hunk_new_start_basic() {
    assert_eq!(parse_hunk_new_start(" -10,3 +10,4 @@ fn query()"), Some(10));
}

#[test]
fn parse_hunk_new_start_without_count() {
    // `@@ -0,0 +1 @@` form (single-line new side, no `,count`).
    assert_eq!(parse_hunk_new_start(" -0,0 +1 @@"), Some(1));
}

#[test]
fn commentable_lines_indexes_added_and_context() {
    let c = CommentableLines::from_unified_diff(sample_diff());
    // src/db.rs hunk: new lines 10 (context), 11 (+), 12 (+), 13 (context).
    assert!(c.contains("src/db.rs", 10), "context line 10 anchorable");
    assert!(c.contains("src/db.rs", 11), "added line 11 anchorable");
    assert!(c.contains("src/db.rs", 12), "added line 12 anchorable");
    assert!(c.contains("src/db.rs", 13), "context line 13 anchorable");
    // src/util.rs hunk: new lines 1 (context), 2 (+), 3 (context).
    assert!(c.contains("src/util.rs", 2), "added line 2 anchorable");
    assert!(!c.is_empty());
}

#[test]
fn commentable_lines_excludes_removed_lines() {
    let c = CommentableLines::from_unified_diff(sample_diff());
    // The removed `format!` line has no new-side number; line 14 is past the hunk.
    assert!(
        !c.contains("src/db.rs", 14),
        "no anchor beyond the hunk's new-side range"
    );
    // A file not in the diff is never anchorable.
    assert!(!c.contains("src/other.rs", 10));
}

#[test]
fn commentable_lines_handles_multiple_hunks() {
    let c = CommentableLines::from_unified_diff(sample_diff());
    assert!(c.contains("src/db.rs", 11));
    assert!(c.contains("src/util.rs", 2));
    // Both files contributed anchors.
    assert!(
        c.len() >= 6,
        "expected anchors across both files, got {}",
        c.len()
    );
}

#[test]
fn build_inline_plan_maps_on_diff_finding() {
    let c = CommentableLines::from_unified_diff(sample_diff());
    let findings = vec![finding_at("src/db.rs", Some(11))];
    let plan = build_inline_plan(&findings, &c);
    assert_eq!(plan.comments.len(), 1, "on-diff finding posts inline");
    assert!(plan.summary_findings.is_empty());
    assert_eq!(plan.comments[0].path, "src/db.rs");
    assert_eq!(plan.comments[0].line, 11);
    assert!(plan.comments[0].body.contains("security"));
}

#[test]
fn build_inline_plan_off_diff_falls_back() {
    let c = CommentableLines::from_unified_diff(sample_diff());
    // Line 99 is not in any hunk → must fall back to summary, not break the post.
    let findings = vec![finding_at("src/db.rs", Some(99))];
    let plan = build_inline_plan(&findings, &c);
    assert!(plan.comments.is_empty(), "off-diff finding is not inline");
    assert_eq!(plan.summary_findings.len(), 1, "off-diff finding → summary");
}

#[test]
fn build_inline_plan_no_line_falls_back() {
    let c = CommentableLines::from_unified_diff(sample_diff());
    // File-level / general finding (no line) → summary body.
    let findings = vec![finding_at("src/db.rs", None)];
    let plan = build_inline_plan(&findings, &c);
    assert!(plan.comments.is_empty());
    assert_eq!(plan.summary_findings.len(), 1);
}

#[test]
fn build_inline_plan_preserves_order_and_splits() {
    let c = CommentableLines::from_unified_diff(sample_diff());
    let findings = vec![
        finding_at("src/db.rs", Some(11)),  // inline
        finding_at("src/db.rs", Some(500)), // off-diff → summary
        finding_at("src/util.rs", Some(2)), // inline
    ];
    let plan = build_inline_plan(&findings, &c);
    assert_eq!(plan.comments.len(), 2);
    assert_eq!(plan.summary_findings.len(), 1);
    assert_eq!(plan.comments[0].line, 11);
    assert_eq!(plan.comments[1].line, 2);
}

#[test]
fn render_finding_comment_includes_kind_and_fix() {
    let f = finding_at("src/db.rs", Some(11));
    let body = render_finding_comment(&f);
    assert!(body.contains("**security**"), "kind in bold: {body}");
    assert!(
        body.contains("SQL injection"),
        "description present: {body}"
    );
    assert!(body.contains("_Fix:_"), "fix line present: {body}");
}
