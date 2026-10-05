//! Unit tests for the #9188 result contract (`withheld_contract.rs`).

use super::*;
use crate::models::Effort;
use crate::pipeline::citation_gate::gate_posted_findings_with_index;
use crate::pipeline::diff_analyzer::DiffAnalyzer;

/// A 12-line new `src/billing.rs`; line 10 sums the amounts.
fn billing_diff() -> String {
    let mut diff = String::from(
        "diff --git a/src/billing.rs b/src/billing.rs\n--- a/src/billing.rs\n+++ b/src/billing.rs\n@@ -0,0 +1,12 @@\n",
    );
    for i in 1..=12 {
        if i == 10 {
            diff.push_str("+    let total = amounts.iter().sum::<u64>();\n");
        } else {
            diff.push_str(&format!("+    let value_{i} = step_{i}(input);\n"));
        }
    }
    diff
}

async fn billing() -> FilteredDiff {
    DiffAnalyzer::default().analyze(&billing_diff()).await
}

fn finding(line: u32, body: &str) -> Finding {
    let mut f = Finding::new("src/billing.rs", "overflow", body, "", 0.9, Effort::Medium);
    f.line = Some(line);
    f
}

fn withheld(reason: &str) -> WithheldFinding {
    WithheldFinding {
        finding: finding(10, "`x`"),
        reason: reason.to_string(),
        missing_fragment: None,
    }
}

#[test]
fn a_withheld_review_reports_typed_withheld_counts() {
    let mut result = ReviewResult::new("acme", "api", 7, "t", "u");
    result.withheld_findings = vec![
        withheld(REFUTED_REASON),
        withheld("a snippet the finding quotes is not in the cited file"),
        withheld(REFUTED_REASON),
    ];
    sync_withheld_counts(&mut result);
    assert_eq!(result.withheld_count, 3);
    let json = serde_json::to_value(&result).expect("serialize");
    assert_eq!(json["withheld_count"], 3);
    assert_eq!(json["withheld_by_reason"]["refuted"], 2);
    assert_eq!(json["withheld_by_reason"]["line_citation"], 1);
}

/// #9188 L in isolation: a survivor whose citation the gate would move does
/// not resolve where it stands, so it is withheld and the review settles.
#[tokio::test]
async fn withhold_unresolved_withholds_a_survivor_off_its_line() {
    let index = LineIndex::from_filtered(&billing().await);
    let mut result = ReviewResult::new("acme", "api", 7, "t", "u");
    result.verdict = Verdict::RequestChanges;
    result.grade = Some("C".to_string());
    result.findings = vec![
        finding(10, "`amounts.iter().sum::<u64>()` can overflow."),
        finding(3, "`amounts.iter().sum::<u64>()` can overflow."),
    ];
    assert_eq!(withhold_unresolved(&mut result, &index), 1);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].line, Some(10));
    assert!(
        result.withheld_findings[0]
            .reason
            .starts_with(UNRESOLVED_AT_HEAD_REASON)
    );
}

/// #9188 L, failing through L alone: the gate moves a two-line `[code: …]`
/// range onto a quote spanning three lines and keeps the finding as
/// re-anchored, but the rewritten range does not contain the whole quote
/// (#9188 I). Only the re-check withholds it. `run_review` cannot reach this
/// shape today: `citation_check` (#4042) withholds every ranged locator first.
#[tokio::test]
async fn withhold_unresolved_withholds_a_range_the_gate_rewrote_short() {
    let index = LineIndex::from_filtered(&billing().await);
    let mut result = ReviewResult::new("acme", "api", 7, "t", "u");
    result.verdict = Verdict::RequestChanges;
    result.findings = vec![finding(
        10,
        "`amounts.iter().sum::<u64>()` overflows [code: `src/billing.rs:2-3` — \"sum::<u64>(); let value_11 = step_11(input); let value_12\"].",
    )];
    gate_posted_findings_with_index(&mut result, &index);
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
    assert!(
        result.findings[0]
            .description
            .contains("`src/billing.rs:10-11`")
    );

    assert_eq!(withhold_unresolved(&mut result, &index), 1);
    assert!(result.findings.is_empty());
    assert!(
        result.withheld_findings[0]
            .reason
            .starts_with(UNRESOLVED_AT_HEAD_REASON)
    );
}

/// #9188 L, criterion 4: an error reading the cited file withholds, never keeps.
#[tokio::test]
async fn withhold_unresolved_fails_closed_on_a_file_outside_the_diff() {
    let index = LineIndex::from_filtered(&billing().await);
    let mut result = ReviewResult::new("acme", "api", 7, "t", "u");
    result.verdict = Verdict::RequestChanges;
    let mut f = finding(10, "`amounts.iter().sum::<u64>()` can overflow.");
    f.file = "src/absent.rs".to_string();
    result.findings = vec![f];
    assert_eq!(withhold_unresolved(&mut result, &index), 1);
    assert_eq!(result.verdict, Verdict::Unknown);
    assert_eq!(result.grade, None);
}

/// #9188 D: a context citation whose excerpt is in the fetched context resolves.
#[tokio::test]
async fn a_gh_citation_in_the_fetched_context_resolves() {
    let body =
        "`amounts.iter().sum::<u64>()` can overflow [gh: #42 — \"overflow fixed by checked_add\"].";
    let filtered = billing().await;
    let mut result = ReviewResult::new("acme", "api", 7, "t", "u");
    result.verdict = Verdict::RequestChanges;
    result.findings = vec![finding(10, body)];
    let index = LineIndex::from_filtered(&filtered)
        .with_refs("Follows #42: overflow fixed by checked_add.");
    gate_posted_findings_with_index(&mut result, &index);
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);

    result.findings = vec![finding(10, body)];
    gate_posted_findings_with_index(&mut result, &LineIndex::from_filtered(&filtered));
    assert!(result.findings.is_empty());
}

#[tokio::test]
async fn unresolvable_survivors_counts_an_unresolved_finding() {
    let filtered = billing().await;
    let findings = vec![
        finding(10, "`amounts.iter().sum::<u64>()` can overflow."),
        finding(10, "`ledger.flush_all()` is never awaited."),
        finding(10, "The total can overflow."),
    ];
    assert_eq!(unresolvable_survivors(&findings, &filtered), 2);
}

/// #9188 A, J: no survivor and anything withheld is `Unknown` with no grade;
/// a review that withheld nothing keeps its verdict.
#[test]
fn settle_no_survivors_withholds_the_verdict_only_when_something_was_withheld() {
    let mut result = ReviewResult::new("acme", "api", 7, "t", "u");
    result.verdict = Verdict::Approve;
    result.grade = Some("A".to_string());
    settle_no_survivors(&mut result);
    assert_eq!(result.verdict, Verdict::Approve);

    result.withheld_findings.push(withheld(UNVERIFIABLE_REASON));
    settle_no_survivors(&mut result);
    assert_eq!(result.verdict, Verdict::Unknown);
    assert_eq!(result.grade, None);
    assert_eq!(
        result.error.as_deref(),
        Some("no verified findings, 1 withheld")
    );
}

/// Restore `prose` over a review whose one survivor is `survivor`, with
/// nothing withheld; returns the body.
fn restored(prose: &str, survivor: Finding, index: &LineIndex) -> String {
    let mut result = ReviewResult::new("acme", "api", 7, "t", "u");
    result.review_body = prose.to_string();
    result.findings = vec![survivor];
    let narrative = take_narrative(&mut result, prose).expect("non-empty prose");
    restore_narrative(&mut result, narrative, index);
    result.review_body
}

/// #9188 C, MEDIUM-1: on a review with nothing withheld, `host:port`
/// strings, a path outside the diff, and a range holding a survivor's line
/// are not unbacked citations, so the prose is returned unchanged. A diff
/// location no survivor backs still replaces it.
#[tokio::test]
async fn a_clean_review_keeps_prose_with_non_diff_locations() {
    let index = LineIndex::from_filtered(&billing().await);
    let survivor = || finding(10, "`amounts.iter().sum::<u64>()` can overflow.");
    for prose in [
        "Binds 127.0.0.1:8080 and calls example.com:443.",
        "See config/app.toml:12 for the limit.",
        "Overflow at src/billing.rs:9-11.",
        "Overflow at billing.rs:10.",
    ] {
        assert_eq!(restored(prose, survivor(), &index), prose);
    }
    let bracketed = finding(
        10,
        "Overflow [code: `src/billing.rs:9-11` — \"amounts.iter().sum::<u64>()\"].",
    );
    let prose = "Overflow at src/billing.rs:11.";
    assert_eq!(restored(prose, bracketed, &index), prose);

    let unbacked = "Overflow at src/billing.rs:3.";
    assert_ne!(restored(unbacked, survivor(), &index), unbacked);
}

#[test]
fn reason_class_names_every_producer() {
    for (reason, class) in [
        (
            "#4042 citation: cited file is not part of the diff at all",
            "citation_integrity",
        ),
        ("#4044 self-negated (marker \"x\")", "self_negated"),
        ("#1873 refuted absence claim: a.rs", "absence_claim"),
        (DUPLICATE_REASON, "duplicate"),
        (OVER_MAX_FINDINGS_REASON, "over_max_findings"),
        (REFUTED_REASON, "refuted"),
        (UNJUDGED_REASON, "unjudged"),
        (OVER_CAP_REASON, "over_cap"),
        (UNVERIFIABLE_REASON, "unverifiable"),
        (UNCONFIRMED_REASON, "unconfirmed"),
        (NO_VERIFIER_REASON, "no_verifier"),
        (
            "#9188 does not resolve at the head: x",
            "unresolved_at_head",
        ),
        (
            "a snippet the finding quotes is not in the cited file",
            "line_citation",
        ),
    ] {
        assert_eq!(reason_class(reason), class, "{reason}");
    }
}
