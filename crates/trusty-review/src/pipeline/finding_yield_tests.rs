//! Unit tests for `pipeline::finding_yield` (#11525).
//!
//! What: row extraction from a `ReviewResult`, ledger round trip, per-PR and
//! per-repo aggregation, and the missing-ledger and bad-line cases.
//! Test: this is the test module.

use super::*;

fn result(repo: &str, pr: u64, ts: &str, posted: usize, refuted: usize) -> ReviewResult {
    let mut r = ReviewResult::new("acme", repo, pr, "title", "url");
    r.timestamp = ts.to_string();
    r.findings_count = posted;
    if refuted > 0 {
        r.withheld_by_reason.insert("refuted".to_string(), refuted);
    }
    // Withheld for another reason: never counted as refuted.
    r.withheld_by_reason.insert("line_citation".to_string(), 4);
    r
}

#[test]
fn yield_row_reads_posted_and_refuted() {
    let row = YieldRow::from_result(&result("api", 3, "2026-11-19T00:00:00Z", 2, 3));
    assert_eq!((row.pr, row.posted, row.refuted), (3, 2, 3));
}

/// A review with refuted findings is counted, and aggregates split per PR and
/// per repo.
#[test]
fn append_then_summarize_aggregates_per_pr_and_repo() {
    let dir = tempfile::tempdir().expect("tempdir");
    for r in [
        result("api", 1, "2026-11-19T01:00:00Z", 2, 2),
        result("api", 1, "2026-11-19T02:00:00Z", 1, 0),
        result("api", 2, "2026-11-19T03:00:00Z", 0, 1),
        result("web", 9, "2026-11-19T00:30:00Z", 3, 0),
    ] {
        append_yield_row(&r, dir.path());
    }
    let s = summarize_yield(dir.path()).expect("summary");
    assert_eq!(
        (s.total.reviews, s.total.posted, s.total.refuted),
        (4, 6, 3)
    );
    assert!((s.total.posted_per_review - 1.5).abs() < 1e-9);
    assert!((s.total.refuted_share - 3.0 / 9.0).abs() < 1e-9);

    let keys: Vec<&str> = s.by_repo.iter().map(|a| a.key.as_str()).collect();
    assert_eq!(keys, ["acme/api", "acme/web"]);
    assert_eq!((s.by_repo[0].posted, s.by_repo[0].refuted), (3, 3));

    // Most recent PR first.
    let prs: Vec<&str> = s.by_pr.iter().map(|a| a.key.as_str()).collect();
    assert_eq!(prs, ["acme/api#2", "acme/api#1", "acme/web#9"]);
    assert_eq!(
        (s.by_pr[1].reviews, s.by_pr[1].posted, s.by_pr[1].refuted),
        (2, 3, 2)
    );
}

#[test]
fn summarize_missing_ledger_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        summarize_yield(dir.path()).expect("summary"),
        YieldSummary::default()
    );
}

#[test]
fn summarize_skips_bad_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    append_yield_row(&result("api", 1, "t", 1, 1), dir.path());
    let path = ledger_path(dir.path());
    let mut text = std::fs::read_to_string(&path).expect("read");
    text.push_str("not json\n");
    std::fs::write(&path, text).expect("write");
    let s = summarize_yield(dir.path()).expect("summary");
    assert_eq!((s.total.reviews, s.skipped_lines), (1, 1));
}

/// `write_review_log` writes the yield row, so every logged review is counted.
#[test]
fn write_review_log_appends_a_yield_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    crate::pipeline::output::write_review_log(&result("api", 5, "t", 1, 2), dir.path());
    let s = summarize_yield(dir.path()).expect("summary");
    assert_eq!((s.total.reviews, s.total.refuted), (1, 2));
}
