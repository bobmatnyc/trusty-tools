//! Unit tests for the line-citation gate (#8905).

use super::*;
use crate::models::{Effort, Finding};
use crate::pipeline::diff_analyzer::models::{
    DroppedFile, FileDisposition, FilteredDiff, FilteredFile, FilteredHunk,
};
use std::collections::HashMap;

// ─── Fixtures ────────────────────────────────────────────────────────────────

/// The line of `BILLING` that holds the summing code the findings describe.
const SUM_LINE: u32 = 30;

fn hunk(header: &str, lines: Vec<String>) -> FilteredHunk {
    FilteredHunk {
        header: header.to_string(),
        lines,
        substantive_confidence: 1.0,
        reason_kept: "test".to_string(),
    }
}

fn file(name: &str, disposition: FileDisposition, hunks: Vec<FilteredHunk>) -> FilteredFile {
    FilteredFile {
        filename: name.to_string(),
        status: "added".to_string(),
        disposition,
        hunks,
        dropped_hunks: Vec::new(),
        summary_line: Some("summary".to_string()),
    }
}

fn diff(files: Vec<FilteredFile>) -> FilteredDiff {
    FilteredDiff {
        files,
        dropped_files: Vec::new(),
        drop_hunk_counts: HashMap::new(),
        original_byte_size: 0,
        filtered_byte_size: 0,
    }
}

/// `src/billing.rs`: a new 40-line file; line [`SUM_LINE`] sums the amounts.
fn billing_lines() -> Vec<String> {
    (1..=40)
        .map(|i| {
            if i == SUM_LINE {
                "+    let total = amounts.iter().sum::<u64>();".to_string()
            } else {
                format!("+    let value_{i} = step_{i}(input);")
            }
        })
        .collect()
}

fn billing_index() -> LineIndex {
    LineIndex::from_filtered(&diff(vec![file(
        "src/billing.rs",
        FileDisposition::Kept,
        vec![hunk("@@ -0,0 +1,40 @@", billing_lines())],
    )]))
}

fn finding(file: &str, line: Option<u32>, body: &str) -> Finding {
    let mut f = Finding::new(file, "overflow", body, "use checked_add", 0.9, Effort::High);
    f.line = line;
    f
}

fn gate(findings: &mut Vec<Finding>, index: &LineIndex) -> GateReport {
    enforce_line_citations(findings, index)
}

// ─── #8905 fail-first cases (a)–(d) ────────────────────────────────────────────

#[test]
fn a_finding_cited_twelve_lines_off_is_reanchored() {
    let mut findings = vec![finding(
        "src/billing.rs",
        Some(SUM_LINE - 12),
        "`amounts.iter().sum::<u64>()` can overflow on large invoices.",
    )];
    let report = gate(&mut findings, &billing_index());

    assert_eq!(
        report,
        GateReport {
            dropped: 0,
            reanchored: 1
        }
    );
    assert_eq!(findings[0].line, Some(SUM_LINE));
    assert_eq!(
        findings[0].citation_correction,
        Some(CitationCorrection {
            from_line: Some(SUM_LINE - 12),
            to_line: SUM_LINE,
        })
    );
}

#[test]
fn a_finding_whose_quoted_code_is_absent_is_dropped() {
    let mut findings = vec![finding(
        "src/billing.rs",
        Some(SUM_LINE),
        "`flush_all()` is never awaited, so the ledger write is lost.",
    )];
    let report = gate(&mut findings, &billing_index());

    assert_eq!(
        report,
        GateReport {
            dropped: 1,
            reanchored: 0
        }
    );
    assert!(findings.is_empty());
}

#[test]
fn a_finding_cited_beyond_eof_is_dropped() {
    // The quote IS in the file, so only the #4999/#5023 line check can drop it.
    let mut findings = vec![finding(
        "src/billing.rs",
        Some(90),
        "`amounts.iter().sum::<u64>()` can overflow on large invoices.",
    )];
    let report = gate(&mut findings, &billing_index());

    assert_eq!(
        report,
        GateReport {
            dropped: 1,
            reanchored: 0
        }
    );
    assert!(findings.is_empty());
}

#[test]
fn a_finding_with_no_anchor_is_dropped() {
    let mut findings = vec![finding(
        "src/billing.rs",
        Some(SUM_LINE),
        "This total can overflow on large invoices.",
    )];
    let report = gate(&mut findings, &billing_index());

    assert_eq!(
        report,
        GateReport {
            dropped: 1,
            reanchored: 0
        }
    );
    assert!(findings.is_empty());
}

// ─── Fail closed (#8905 item 4) ────────────────────────────────────────────────

/// Fail-open mutation this pins: `Err(_) => kept.push(f)` in the error arm of
/// `enforce_line_citations`.
#[test]
fn a_file_with_a_malformed_hunk_header_fails_closed() {
    let index = LineIndex::from_filtered(&diff(vec![file(
        "src/billing.rs",
        FileDisposition::Kept,
        vec![hunk("@@ not a header @@", billing_lines())],
    )]));
    let mut findings = vec![finding(
        "src/billing.rs",
        Some(SUM_LINE),
        "`amounts.iter().sum::<u64>()` can overflow on large invoices.",
    )];

    assert_eq!(gate(&mut findings, &index).dropped, 1);
    assert!(findings.is_empty());
}

#[test]
fn a_summary_only_file_fails_closed() {
    let mut filtered = diff(vec![file(
        "src/big.rs",
        FileDisposition::SummaryOnly,
        vec![],
    )]);
    filtered.dropped_files.push(DroppedFile {
        path: "Cargo.lock".to_string(),
        reason: "lockfile".to_string(),
    });
    let index = LineIndex::from_filtered(&filtered);
    let mut findings = vec![
        finding("src/big.rs", Some(3), "`parse_config(raw)` ignores errors."),
        finding("Cargo.lock", Some(3), "`serde_json` is pinned twice."),
        finding(
            "src/absent.rs",
            Some(3),
            "`parse_config(raw)` ignores errors.",
        ),
    ];

    assert_eq!(gate(&mut findings, &index).dropped, 3);
}

#[test]
fn a_finding_citing_no_file_is_dropped() {
    let mut findings = vec![finding(
        UNKNOWN_FILE_PLACEHOLDER,
        None,
        "`amounts.iter().sum::<u64>()` can overflow.",
    )];
    assert_eq!(gate(&mut findings, &billing_index()).dropped, 1);
}

// ─── Kept and re-anchored shapes ───────────────────────────────────────────────

#[test]
fn a_finding_whose_line_holds_its_code_is_kept_unchanged() {
    let mut findings = vec![finding(
        "src/billing.rs",
        Some(SUM_LINE),
        "`amounts.iter().sum::<u64>()` can overflow.",
    )];
    assert_eq!(gate(&mut findings, &billing_index()), GateReport::default());
    assert_eq!(findings[0].line, Some(SUM_LINE));
    assert_eq!(findings[0].citation_correction, None);
}

#[test]
fn an_identifier_anchor_reanchors_when_no_snippet_is_given() {
    let mut findings = vec![finding(
        "src/billing.rs",
        Some(3),
        "The call to step_12 discards its error.",
    )];
    assert_eq!(gate(&mut findings, &billing_index()).reanchored, 1);
    assert_eq!(findings[0].line, Some(12));
}

#[test]
fn a_finding_with_no_line_is_anchored_to_its_code() {
    let mut findings = vec![finding(
        "src/billing.rs",
        None,
        "`sum::<u64>()` can overflow.",
    )];
    assert_eq!(gate(&mut findings, &billing_index()).reanchored, 1);
    assert_eq!(findings[0].line, Some(SUM_LINE));
}

#[test]
fn a_bracket_citation_is_rewritten_to_the_verified_line() {
    let body = "Overflow [code: `src/billing.rs:18` — \"let total = amounts.iter()\"] here.";
    let mut findings = vec![finding("src/billing.rs", Some(SUM_LINE), body)];

    assert_eq!(gate(&mut findings, &billing_index()).reanchored, 1);
    assert!(
        findings[0].description.contains("`src/billing.rs:30`"),
        "{}",
        findings[0].description
    );
}

#[test]
fn a_removed_line_holds_on_the_old_side() {
    let index = LineIndex::from_filtered(&diff(vec![file(
        "src/a.rs",
        FileDisposition::Kept,
        vec![hunk(
            "@@ -10,3 +10,2 @@",
            vec![
                " fn run() {".to_string(),
                "-    guard.check()?;".to_string(),
                "     work();".to_string(),
            ],
        )],
    )]));
    let mut findings = vec![finding(
        "src/a.rs",
        Some(11),
        "Removing `guard.check()?` drops auth.",
    )];
    assert_eq!(gate(&mut findings, &index), GateReport::default());
}

#[test]
fn the_gate_is_idempotent() {
    let mut findings = vec![finding(
        "src/billing.rs",
        Some(SUM_LINE - 12),
        "`amounts.iter().sum::<u64>()` can overflow.",
    )];
    let index = billing_index();
    gate(&mut findings, &index);
    assert_eq!(gate(&mut findings, &index), GateReport::default());
    assert_eq!(findings[0].line, Some(SUM_LINE));
}

#[test]
fn parse_locator_reads_lines_and_ranges() {
    assert_eq!(
        parse_locator("a.rs:12").ok(),
        Some(("a.rs".to_string(), Some((12, 12))))
    );
    assert_eq!(
        parse_locator("a.rs:L3-L5").ok(),
        Some(("a.rs".to_string(), Some((3, 5))))
    );
    assert_eq!(
        parse_locator("docs/spec.md").ok(),
        Some(("docs/spec.md".to_string(), None))
    );
    assert!(matches!(
        parse_locator("a.rs:12-x"),
        Err(GateError::BadLocator(_))
    ));
}

#[test]
fn gate_posted_findings_relaxes_the_verdict_when_it_drops_every_finding() {
    let filtered = diff(vec![file(
        "src/billing.rs",
        FileDisposition::Kept,
        vec![hunk("@@ -0,0 +1,40 @@", billing_lines())],
    )]);
    let mut result = ReviewResult::new("acme", "api", 7, "t", "https://example.invalid/pr/7");
    result.verdict = crate::models::Verdict::Block;
    result.findings = vec![finding(
        "src/billing.rs",
        Some(SUM_LINE),
        "This can overflow.",
    )];

    assert_eq!(gate_posted_findings(&mut result, &filtered).dropped, 1);
    assert!(result.findings.is_empty());
    assert_eq!(result.verdict, crate::models::Verdict::Approve);
}
