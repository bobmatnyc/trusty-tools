//! Unit tests for inline per-line review comments (#1414).
//!
//! Why: the finding→inline-comment mapping and the off-diff fallback are the
//! load-bearing correctness logic of #1414 — a mistake either posts comments on
//! lines GitHub rejects (failing the whole review) or drops findings silently.
//! What: exercises diff-index construction, on-diff mapping, off-diff fallback,
//! and the no-line fallback against representative unified diffs.
//! Test: this module.

use super::*;
use crate::models::{Effort, Finding, VerifyOutcome};

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

/// REGRESSION (#8949): a partly verified citation never posts inline.
#[test]
fn build_inline_plan_posts_a_partial_citation_in_the_body() {
    let c = CommentableLines::from_unified_diff(sample_diff());
    let mut partial = finding_at("src/db.rs", Some(11));
    partial.citation_partial = true;
    let plan = build_inline_plan(&[partial], &c);
    assert!(
        plan.comments.is_empty(),
        "a partial citation must not post inline"
    );
    assert_eq!(plan.summary_findings.len(), 1);
}

#[test]
fn build_inline_plan_no_line_falls_back() {
    let c = CommentableLines::from_unified_diff(sample_diff());
    // General finding (no line) on a file outside the diff → summary body.
    // #10762: a no-line finding on an in-diff file now anchors file-level.
    let findings = vec![finding_at("src/other.rs", None)];
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

// ── #3474: style findings are labelled informational ─────────────────────────

/// A style finding's comment says up front that it does not block.
///
/// Why: `grade::derive_verdict_with` already refuses to let a style nit move the
/// verdict, but the author reading the comment on the diff cannot see that. Left
/// unlabelled, a taste note reads exactly like a change request.
/// What: renders a `FindingCategory::Style` finding, asserts the informational
/// lead-in precedes the ordinary kind + description lead line.
#[test]
fn render_marks_style_finding_informational() {
    let f = finding_at("src/db.rs", Some(11)).with_category(crate::models::FindingCategory::Style);
    let body = render_finding_comment(&f);
    assert!(
        body.contains("Informational (style / preference)"),
        "style finding must be labelled informational: {body}"
    );
    assert!(
        body.contains("does not block"),
        "the label must say it does not block: {body}"
    );
    assert!(
        body.starts_with("> **Informational"),
        "the label must lead the comment, above the claim: {body}"
    );
    assert!(
        body.contains("**security**"),
        "the ordinary lead line still renders: {body}"
    );
}

/// A coverage-gap finding is labelled informational, but not as a style nit.
///
/// Why: #7036 made `TestCoverage` advisory, so it earns the same "does not
/// block" promise — but calling a missing test a "style / preference" nit would
/// misdescribe it and invite the author to discount a note worth acting on.
/// What: renders a `FindingCategory::TestCoverage` finding, asserts the
/// coverage-specific lead-in and the absence of the style wording.
#[test]
fn render_marks_test_coverage_finding_informational() {
    let f = finding_at("src/db.rs", Some(11))
        .with_category(crate::models::FindingCategory::TestCoverage);
    let body = render_finding_comment(&f);
    assert!(
        body.starts_with("> **Informational (test coverage) — does not block.**"),
        "a coverage gap must lead with the coverage-specific label (#7036): {body}"
    );
    assert!(
        !body.contains("style / preference"),
        "a coverage gap must not be labelled a taste note (#7036): {body}"
    );
    assert!(
        body.contains("**security**"),
        "the ordinary lead line still renders: {body}"
    );
}

/// An ordinary correctness finding gains no informational label.
///
/// Why: the label must be a signal, not decoration — if every comment carried it
/// the reader would learn nothing from it. This is the negative half of
/// `render_marks_style_finding_informational`.
#[test]
fn render_leaves_correctness_finding_unlabelled() {
    let f = finding_at("src/db.rs", Some(11));
    let body = render_finding_comment(&f);
    assert!(
        !body.contains("Informational"),
        "a correctness finding must not be labelled informational: {body}"
    );
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

// ─── Verification caveat on the rendered comment (#5312) ──────────────────────

/// A verifier-refuted finding renders with a caveat above the claim (#5312).
///
/// Why: before this, a finding carrying `verified: "refuted"` rendered
/// byte-identically to a surviving one, so a reader of the PR could not tell the
/// pipeline had disproved it.
/// What: renders a `Refuted` finding, asserts the body opens with the
/// verification blockquote and names the refutation, and asserts the same
/// finding reaching `build_inline_plan` carries the marker into the plan.
/// Test: this test itself.
#[test]
fn render_marks_refuted_finding() {
    let mut f = finding_at("src/db.rs", Some(11));
    f.verified = Some(VerifyOutcome::Refuted);

    let body = render_finding_comment(&f);
    assert!(
        body.starts_with("> **Verification:**"),
        "the caveat must lead the comment, not trail it: {body}"
    );
    assert!(
        body.contains("REFUTED"),
        "a refuted claim must say so: {body}"
    );

    let c = CommentableLines::from_unified_diff(sample_diff());
    let plan = build_inline_plan(std::slice::from_ref(&f), &c);
    assert_eq!(plan.comments.len(), 1, "still posted, but marked");
    assert!(
        plan.comments[0].body.contains("REFUTED"),
        "the plan must carry the marked body: {}",
        plan.comments[0].body
    );
}

/// A verifier-failure finding reads as unverified, never as disproved (#5312).
///
/// Why: `ErrorRefuted` means the verifier was never reached (#726, #1876) and
/// the verdict deliberately keeps the escalation, so calling the claim refuted
/// inline would contradict the verdict the same review reports.
/// What: renders an `ErrorRefuted` finding and asserts the caveat says
/// UNVERIFIED and names the error class rather than claiming a disproof.
/// Test: this test itself.
#[test]
fn render_marks_error_refuted_as_unverified() {
    let mut f = finding_at("src/db.rs", Some(11));
    f.verified = Some(VerifyOutcome::ErrorRefuted {
        error_class: "RateLimited".to_string(),
    });
    let body = render_finding_comment(&f);
    assert!(body.contains("UNVERIFIED"), "{body}");
    assert!(body.contains("RateLimited"), "{body}");
    assert!(
        !body.contains("REFUTED it"),
        "an unreachable verifier disproved nothing: {body}"
    );
}

/// Confirmed and unjudged findings render exactly as before (#5312).
///
/// Why: the caveat is owed only where the pipeline does not stand behind the
/// claim; adding it anywhere else would put a hedge on every comment.
/// What: renders a `Confirmed` finding and a finding with no outcome, asserting
/// neither carries the verification blockquote.
/// Test: this test itself.
#[test]
fn render_leaves_confirmed_and_unjudged_findings_unmarked() {
    let unjudged = finding_at("src/db.rs", Some(11));
    assert!(
        !render_finding_comment(&unjudged).contains("**Verification:**"),
        "no outcome recorded → no caveat"
    );

    let mut confirmed = finding_at("src/db.rs", Some(11));
    confirmed.verified = Some(VerifyOutcome::Confirmed);
    assert!(
        !render_finding_comment(&confirmed).contains("**Verification:**"),
        "a confirmed finding stands unqualified"
    );
}

/// REGRESSION (#10762): every finding not placed inline carries its reason.
///
/// Why: a finding that fell back to the summary, or past the nit cap, left no
/// reason, so a consumer could not tell an off-diff line from a suppressed nit.
/// What: plans a no-line, an off-diff and six on-diff nits, asserting
/// `no_line`, `off_diff` and one `nit_cap` entry with their indices; then
/// round-trips every reason string, `withheld:<class>` included, and reads an
/// unknown string as `Unknown`.
/// Test: this test itself.
#[test]
fn unanchored_reason_enum_covers_no_line_off_diff_nit_cap_withheld() {
    use crate::models::{UnanchoredFinding, UnanchoredReason};

    let c = CommentableLines::from_unified_diff(&diff_with_n_added(20));
    let mut findings = vec![nit_at(1), nit_at(99)];
    findings[0].line = None;
    // #10762: off the diff, so the no-line finding stays `no_line`, not file-level.
    findings[0].file = "src/other.rs".to_string();
    findings.extend((2..=MAX_INLINE_NITS as u32 + 2).map(nit_at));
    let plan = build_inline_plan(&findings, &c);
    let at = |finding_index, reason| UnanchoredFinding {
        finding_index,
        reason,
    };
    assert_eq!(
        plan.unanchored,
        vec![
            at(0, UnanchoredReason::NoLine),
            at(1, UnanchoredReason::OffDiff),
            at(MAX_INLINE_NITS + 2, UnanchoredReason::NitCap),
        ]
    );
    assert_eq!(plan.inline_indices.len() + plan.unanchored.len(), findings.len());

    let cases = [
        (UnanchoredReason::OffDiff, "off_diff"),
        (UnanchoredReason::NoLine, "no_line"),
        (UnanchoredReason::DeletedLine, "deleted_line"),
        (UnanchoredReason::NitCap, "nit_cap"),
        (UnanchoredReason::Withheld("refuted".into()), "withheld:refuted"),
    ];
    for (reason, text) in cases {
        let json = serde_json::to_value(&reason).expect("serialize");
        assert_eq!(json, text);
        let back: UnanchoredReason = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, reason);
    }
    let future: UnanchoredReason = serde_json::from_str("\"left_only\"").expect("tolerated");
    assert_eq!(future, UnanchoredReason::Unknown);
}

// ─── #10762: LEFT-side and file-level anchors ─────────────────────────────────

/// `src/del.rs` deletes old lines 5 and 6 (new line 5 is the context `tail`),
/// and `src/gone.rs` is deleted outright.
const DELETION_DIFF: &str = "\
diff --git a/src/del.rs b/src/del.rs
--- a/src/del.rs
+++ b/src/del.rs
@@ -4,4 +4,2 @@
 fn keep() {}
-fn dropped_a() {}
-fn dropped_b() {}
 fn tail() {}
diff --git a/src/gone.rs b/src/gone.rs
deleted file mode 100644
--- a/src/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-fn gone() {}
-fn also_gone() {}
";

/// REGRESSION (#10762): a finding at a removed line anchors on the LEFT side.
///
/// Why: removed lines were skipped and the old-side counter was not kept, so
/// a finding at a deleted line read as `off_diff`.
/// What: plans findings at a removed-only line, a deleted file's line, and a
/// removed line whose number is also a new-side line; asserts `LEFT` with the
/// old-side number for the first two and `RIGHT` for the third (new side
/// first), with nothing unanchored.
/// Test: this test itself.
#[test]
fn deleted_line_finding_anchors_left() {
    let c = CommentableLines::from_unified_diff(DELETION_DIFF);
    let findings = vec![
        finding_at("src/del.rs", Some(6)),
        finding_at("src/gone.rs", Some(1)),
        finding_at("src/del.rs", Some(5)),
    ];
    let plan = build_inline_plan(&findings, &c);

    assert!(plan.unanchored.is_empty(), "{:?}", plan.unanchored);
    assert_eq!(plan.inline_indices, vec![0, 1, 2]);
    let anchors: Vec<(&str, u32, AnchorSide)> = plan
        .comments
        .iter()
        .map(|c| (c.path.as_str(), c.line, c.side))
        .collect();
    assert_eq!(
        anchors,
        vec![
            ("src/del.rs", 6, AnchorSide::Left),
            ("src/gone.rs", 1, AnchorSide::Left),
            ("src/del.rs", 5, AnchorSide::Right),
        ]
    );
    assert!(plan.comments.iter().all(|c| c.subject_type.is_none()));
    let json = serde_json::to_value(AnchorSide::Left).expect("serialize");
    assert_eq!(json, "LEFT");
}

/// REGRESSION (#10762): a removed line that cannot anchor reads `deleted_line`.
///
/// Why: with no old-side index, every finding at a removed line read as
/// `off_diff`, though the line is in the diff.
/// What: indexes the deletion diff (removed old lines 5 and 6, context 4 not
/// removed); plans a `citation_partial` finding at removed-only line 6
/// (`deleted_line`) and one at line 5, which has a new-side match
/// (`off_diff`, as before).
/// Test: this test itself.
#[test]
fn removed_line_with_no_new_side_match_is_not_off_diff() {
    let c = CommentableLines::from_unified_diff(DELETION_DIFF);
    assert!(c.contains_removed("src/del.rs", 6));
    // The `deleted file mode` header after the hunk is not a context line 6.
    assert!(!c.contains("src/del.rs", 6), "line 6 has no new-side match");
    assert!(!c.contains_removed("src/del.rs", 4), "a context line is not removed");
    assert!(c.contains_removed("src/gone.rs", 2));

    let mut removed_only = finding_at("src/del.rs", Some(6));
    removed_only.citation_partial = true;
    let mut both_sides = finding_at("src/del.rs", Some(5));
    both_sides.citation_partial = true;
    let plan = build_inline_plan(&[removed_only, both_sides], &c);

    assert!(plan.comments.is_empty(), "a partial citation never posts inline");
    assert_eq!(
        plan.unanchored,
        vec![
            UnanchoredFinding {
                finding_index: 0,
                reason: UnanchoredReason::DeletedLine,
            },
            UnanchoredFinding {
                finding_index: 1,
                reason: UnanchoredReason::OffDiff,
            },
        ]
    );
    assert_eq!(plan.summary_findings.len(), 2);
}

/// REGRESSION (#10762): a no-line finding off the diff stays `no_line`.
///
/// Why: the file-level anchor applies only to a file the diff names; a
/// no-line finding on any other file must still read `no_line`.
/// What: asserts the diff names `src/db.rs` and not `src/other.rs`, then
/// plans a no-line finding on each: the first anchors file-level, the second
/// is `no_line` and goes to the summary.
/// Test: this test itself.
#[test]
fn no_line_and_file_not_in_diff_is_unanchored_no_line() {
    let c = CommentableLines::from_unified_diff(sample_diff());
    assert!(c.contains_file("src/db.rs"));
    assert!(!c.contains_file("src/other.rs"));

    let findings = vec![
        finding_at("src/db.rs", None),
        finding_at("src/other.rs", None),
    ];
    let plan = build_inline_plan(&findings, &c);

    assert_eq!(plan.inline_indices, vec![0]);
    assert_eq!(plan.comments[0].subject_type, Some(SubjectType::File));
    assert_eq!(
        plan.unanchored,
        vec![UnanchoredFinding {
            finding_index: 1,
            reason: UnanchoredReason::NoLine,
        }]
    );
    assert_eq!(plan.summary_findings.len(), 1);
    assert_eq!(plan.summary_findings[0].file, "src/other.rs");
}
