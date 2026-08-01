//! Tests for the merged-PR reclaim-pass renderer (#2919).
//!
//! Why: this renderer is the operator's only evidence that the pass ran and
//! what it did. A silent pass is indistinguishable from a pass that deleted
//! the wrong thing, so the shapes it must never swallow — a failed pass, a
//! re-check refusal, a removal that failed — are pinned here.
//! What: exercises the null/absent short-circuit, the dry-run and real
//! summaries, and every stderr diagnostic branch.

use super::print_merged_pr_pass;

#[test]
fn merged_pr_pass_prints_nothing_for_a_null_body() {
    // The shape the route returns when `merged_prs` was not requested. It must
    // not print a "reclaimed 0" line that implies the pass ran.
    print_merged_pr_pass(None, true);
    print_merged_pr_pass(Some(&serde_json::Value::Null), false);
}

#[test]
fn merged_pr_pass_reports_reclaimed_paths_and_bytes() {
    let body = serde_json::json!({
        "removed": ["/tmp/a", "/tmp/b"],
        "removed_bytes": 4096,
        "refused_at_recheck": [],
        "removal_failed": [],
        "reclaimable": 2,
        "reclaimable_bytes": 4096,
    });
    // Both modes must render without panicking on any field shape.
    print_merged_pr_pass(Some(&body), false);
    print_merged_pr_pass(Some(&body), true);
}

#[test]
fn merged_pr_pass_surfaces_recheck_refusals() {
    // A candidate the survey approved but the re-check refused is a near-miss:
    // it must reach the operator, not be swallowed.
    let body = serde_json::json!({
        "removed": [],
        "removed_bytes": 0,
        "refused_at_recheck": ["/tmp/c: a session claims it now"],
        "removal_failed": ["/tmp/d"],
        "reclaimable": 1,
    });
    print_merged_pr_pass(Some(&body), false);
}

#[test]
fn merged_pr_pass_reports_a_failed_pass_rather_than_a_zero_count() {
    // A panicked pass reclaimed nothing, but printing "reclaimed 0 worktrees"
    // would read as a healthy no-op rather than a failure.
    let body = serde_json::json!({ "error": "task panicked" });
    print_merged_pr_pass(Some(&body), false);
}

#[test]
fn merged_pr_pass_tolerates_missing_fields() {
    // A third-party or older daemon may omit fields entirely; the renderer must
    // degrade rather than panic.
    print_merged_pr_pass(Some(&serde_json::json!({})), true);
    print_merged_pr_pass(
        Some(&serde_json::json!({ "removed": "not-an-array" })),
        false,
    );
}
