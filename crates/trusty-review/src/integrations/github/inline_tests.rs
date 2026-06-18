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
    // The inline index set records identity (original indices 0 and 2), not the
    // off-diff finding at index 1 — this is the authoritative partition the
    // summary body uses to avoid coordinate-based silent omission (#1414).
    assert_eq!(plan.inline_indices, vec![0, 2]);
}

#[test]
fn build_inline_plan_same_line_distinct_findings_keep_distinct_indices() {
    // Two distinct findings at the SAME (file, line): both are anchorable, so both
    // go inline here and get distinct indices.  The key property is that
    // `inline_indices` keys on identity (index), never the shared coordinate — so a
    // consumer can tell exactly which findings were placed, with no collision.
    let c = CommentableLines::from_unified_diff(sample_diff());
    let findings = vec![
        finding_at("src/db.rs", Some(11)),
        finding_at("src/db.rs", Some(11)),
    ];
    let plan = build_inline_plan(&findings, &c);
    assert_eq!(plan.comments.len(), 2);
    assert_eq!(
        plan.inline_indices,
        vec![0, 1],
        "distinct same-line findings get distinct indices, not a collapsed coordinate"
    );
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

// ── #1415: committable suggestion blocks ─────────────────────────────────────

#[test]
fn render_emits_suggestion_block() {
    // A finding with concrete replacement code emits a ```suggestion block.
    let mut f = finding_at("src/db.rs", Some(11));
    f.suggested_replacement = Some("let sql = bind(\"SELECT ?\", input);".to_string());
    let body = render_finding_comment(&f);
    assert!(
        body.contains("```suggestion\n"),
        "must open a suggestion fence: {body}"
    );
    assert!(
        body.contains("let sql = bind(\"SELECT ?\", input);"),
        "must contain the replacement code: {body}"
    );
    // When a committable block is shown, the prose `_Fix:_` line is not also shown.
    assert!(
        !body.contains("_Fix:_"),
        "committable block replaces the prose fix: {body}"
    );
}

#[test]
fn render_falls_back_to_prose_fix() {
    // No suggested_replacement → prose `_Fix:_`, no suggestion block.
    let f = finding_at("src/db.rs", Some(11));
    let body = render_finding_comment(&f);
    assert!(!body.contains("```suggestion"), "no block: {body}");
    assert!(body.contains("_Fix:_"), "prose fix present: {body}");
}

#[test]
fn suggestion_with_fence_is_rejected() {
    // A replacement containing a ``` fence must degrade to prose, never break the
    // suggestion block.
    let mut f = finding_at("src/db.rs", Some(11));
    f.suggested_replacement = Some("```rust\nlet x = 1;\n```".to_string());
    let body = render_finding_comment(&f);
    assert!(
        !body.contains("```suggestion"),
        "fenced replacement must not emit a suggestion block: {body}"
    );
    assert!(body.contains("_Fix:_"), "degrades to prose: {body}");
}

#[test]
fn empty_replacement_is_not_committable() {
    assert!(!suggestion_is_committable(""));
    assert!(!suggestion_is_committable("   "));
    assert!(suggestion_is_committable("let x = 1;"));
}

// ── #1416: consequence + uncertainty signalling ──────────────────────────────

#[test]
fn render_includes_consequence() {
    let mut f = finding_at("src/db.rs", Some(11));
    f.consequence = "panics on empty input".to_string();
    let body = render_finding_comment(&f);
    assert!(
        body.contains("_Why it matters:_ panics on empty input"),
        "consequence line must appear: {body}"
    );
}

#[test]
fn render_omits_consequence_when_empty() {
    let f = finding_at("src/db.rs", Some(11)); // consequence defaults to ""
    let body = render_finding_comment(&f);
    assert!(
        !body.contains("_Why it matters:_"),
        "no consequence line when empty: {body}"
    );
}

#[test]
fn low_confidence_finding_is_hedged() {
    // Confidence below the threshold → the description is hedged ("This may…").
    let mut f = finding_at("src/db.rs", Some(11));
    f.confidence = 0.4;
    let body = render_finding_comment(&f);
    assert!(
        body.to_lowercase().contains("this may be an issue"),
        "low-confidence finding must be hedged: {body}"
    );
}

#[test]
fn high_confidence_finding_is_asserted() {
    // Confidence at/above the threshold → asserted, not hedged.
    let mut f = finding_at("src/db.rs", Some(11));
    f.confidence = 0.9;
    let body = render_finding_comment(&f);
    assert!(
        !body.to_lowercase().contains("this may be an issue"),
        "high-confidence finding must not be hedged: {body}"
    );
    // The bare description still appears.
    assert!(body.contains("SQL injection"), "{body}");
}

#[test]
fn already_hedged_not_double_hedged() {
    // A low-confidence finding whose description already hedges is not re-hedged.
    let mut f = Finding::new(
        "src/db.rs",
        "logic",
        "May overflow for large inputs",
        "clamp it",
        0.3,
        Effort::Low,
    );
    f.line = Some(11);
    let body = render_finding_comment(&f);
    assert!(
        !body.contains("This may be an issue: may overflow"),
        "already-hedged description must not be double-hedged: {body}"
    );
    assert!(body.contains("May overflow for large inputs"), "{body}");
}

#[test]
fn uncertainty_threshold_is_point_six() {
    // Lock the documented threshold so a silent change is caught.
    assert_eq!(UNCERTAINTY_THRESHOLD, 0.6);
}

// ── #1420: nit-volume cap + rollup ───────────────────────────────────────────

/// A diff that adds `n` lines to src/big.rs so each nit gets a distinct anchor.
fn diff_with_n_added(n: u32) -> String {
    let mut s = String::from("+++ b/src/big.rs\n");
    s.push_str(&format!("@@ -0,0 +1,{n} @@\n"));
    for i in 0..n {
        s.push_str(&format!("+line {i}\n"));
    }
    s
}

fn nit_at(line: u32) -> Finding {
    let mut f = Finding::new(
        "src/big.rs",
        "nit",
        "minor style",
        "tweak",
        0.9,
        Effort::Low,
    );
    f.line = Some(line);
    f
}

#[test]
fn nit_cap_keeps_first_n_inline() {
    let diff = diff_with_n_added(20);
    let c = CommentableLines::from_unified_diff(&diff);
    // Exactly MAX_INLINE_NITS nits → all inline, none suppressed.
    let findings: Vec<Finding> = (1..=MAX_INLINE_NITS as u32).map(nit_at).collect();
    let plan = build_inline_plan(&findings, &c);
    assert_eq!(plan.comments.len(), MAX_INLINE_NITS);
    assert_eq!(plan.suppressed_nits, 0);
    assert!(plan.suppressed_nits_line().is_none());
}

#[test]
fn nit_cap_rolls_up_overflow() {
    let diff = diff_with_n_added(30);
    let c = CommentableLines::from_unified_diff(&diff);
    // MAX_INLINE_NITS + 7 nits → first N inline, 7 rolled up.
    let total = MAX_INLINE_NITS as u32 + 7;
    let findings: Vec<Finding> = (1..=total).map(nit_at).collect();
    let plan = build_inline_plan(&findings, &c);
    assert_eq!(plan.comments.len(), MAX_INLINE_NITS, "cap inline nits at N");
    assert_eq!(plan.suppressed_nits, 7, "overflow rolled up");
    let line = plan.suppressed_nits_line().expect("rollup line");
    assert!(line.contains("+7 more minor nits suppressed"), "{line}");
}

#[test]
fn nit_cap_does_not_suppress_high_severity() {
    let diff = diff_with_n_added(30);
    let c = CommentableLines::from_unified_diff(&diff);
    // 10 nits (over the cap) interleaved with 3 medium findings; only nits cap.
    let mut findings: Vec<Finding> = (1..=10).map(nit_at).collect();
    for line in [11, 12, 13] {
        let mut f = Finding::new("src/big.rs", "bug", "real", "fix", 0.9, Effort::Medium);
        f.line = Some(line);
        findings.push(f);
    }
    let plan = build_inline_plan(&findings, &c);
    // 5 nits inline + 3 medium inline = 8; 5 nits suppressed.
    assert_eq!(plan.comments.len(), MAX_INLINE_NITS + 3);
    assert_eq!(plan.suppressed_nits, 5);
}

#[test]
fn suppressed_nits_line_singular() {
    let diff = diff_with_n_added(30);
    let c = CommentableLines::from_unified_diff(&diff);
    let total = MAX_INLINE_NITS as u32 + 1;
    let findings: Vec<Finding> = (1..=total).map(nit_at).collect();
    let plan = build_inline_plan(&findings, &c);
    assert_eq!(plan.suppressed_nits, 1);
    let line = plan.suppressed_nits_line().expect("rollup line");
    assert!(line.contains("+1 more minor nit suppressed"), "{line}");
    assert!(!line.contains("nits"), "singular form: {line}");
}

#[test]
fn max_inline_nits_is_five() {
    // Lock the documented cap so a silent change is caught.
    assert_eq!(MAX_INLINE_NITS, 5);
}
