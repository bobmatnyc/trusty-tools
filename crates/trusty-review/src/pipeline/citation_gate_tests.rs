//! Unit tests for the line-citation gate (#8905).

use super::*;
use crate::models::{Effort, Finding, Verdict};
use crate::pipeline::diff_analyzer::models::{
    DroppedFile, FileDisposition, FilteredDiff, FilteredFile, FilteredHunk,
};
use std::collections::HashMap;

// ─── Fixtures ────────────────────────────────────────────────────────────────

/// The line of `src/billing.rs` that holds the summing code.
const SUM_LINE: u32 = 30;

/// The body every "correct" finding carries: a quote of [`SUM_LINE`].
const SUM_QUOTE: &str = "`amounts.iter().sum::<u64>()` can overflow on large invoices.";

fn hunk(header: &str, lines: Vec<String>) -> FilteredHunk {
    FilteredHunk {
        header: header.to_string(),
        lines,
        substantive_confidence: 1.0,
        reason_kept: "test".to_string(),
    }
}

fn lines(raw: &[&str]) -> Vec<String> {
    raw.iter().map(|l| l.to_string()).collect()
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

fn billing_file() -> FilteredFile {
    file(
        "src/billing.rs",
        FileDisposition::Kept,
        vec![hunk("@@ -0,0 +1,40 @@", billing_lines())],
    )
}

fn billing_index() -> LineIndex {
    LineIndex::from_filtered(&diff(vec![billing_file()]))
}

/// `src/billing.rs` plus a one-line `src/other.rs` holding `other_line`.
fn two_file_index(other_line: &str) -> LineIndex {
    let other = file(
        "src/other.rs",
        FileDisposition::Kept,
        vec![hunk("@@ -0,0 +1,1 @@", lines(&[other_line]))],
    );
    LineIndex::from_filtered(&diff(vec![billing_file(), other]))
}

fn finding(file: &str, line: Option<u32>, body: &str) -> Finding {
    let mut f = Finding::new(file, "overflow", body, "use checked_add", 0.9, Effort::High);
    f.line = line;
    f
}

/// Run the gate and return `(dropped, reanchored)`.
fn gate(findings: &mut Vec<Finding>, index: &LineIndex) -> (usize, usize) {
    let report = enforce_line_citations(findings, index);
    (report.dropped, report.reanchored)
}

fn gate_one(file: &str, line: Option<u32>, body: &str, index: &LineIndex) -> Vec<Finding> {
    let mut findings = vec![finding(file, line, body)];
    gate(&mut findings, index);
    findings
}

// ─── #8905 fail-first cases (a)–(d) ────────────────────────────────────────────

#[test]
fn a_finding_cited_twelve_lines_off_is_reanchored() {
    let mut findings = vec![finding("src/billing.rs", Some(SUM_LINE - 12), SUM_QUOTE)];
    assert_eq!(gate(&mut findings, &billing_index()), (0, 1));
    assert_eq!(findings[0].line, Some(SUM_LINE));
    assert_eq!(
        findings[0].citation_correction,
        Some(CitationCorrection {
            from_line: Some(SUM_LINE - 12),
            to_line: SUM_LINE,
            removed_code: false,
        })
    );
}

#[test]
fn a_finding_whose_quoted_code_is_absent_is_dropped() {
    let body = "`flush_all()` is never awaited, so the ledger write is lost.";
    let mut findings = vec![finding("src/billing.rs", Some(SUM_LINE), body)];
    assert_eq!(gate(&mut findings, &billing_index()), (1, 0));
}

#[test]
fn a_finding_cited_beyond_eof_is_dropped() {
    // The quote IS in the file, so only the #4999/#5023 line check can drop it.
    let mut findings = vec![finding("src/billing.rs", Some(90), SUM_QUOTE)];
    assert_eq!(gate(&mut findings, &billing_index()), (1, 0));
}

#[test]
fn a_finding_with_no_anchor_is_dropped() {
    let body = "This total can overflow on large invoices.";
    let mut findings = vec![finding("src/billing.rs", Some(SUM_LINE), body)];
    assert_eq!(gate(&mut findings, &billing_index()), (1, 0));
}

// ─── Critic round (#8905 rows 1–3, 6, 7) ───────────────────────────────────────

/// #8949 ruling (a), replacing #8905 row 1's all-of rule: one quote on the
/// cited line keeps the finding, marked partial and advisory only.
#[test]
fn a_finding_with_one_real_and_one_illustrative_snippet_is_kept_marked() {
    let body = "`amounts.iter().sum::<u64>()` then `ledger.flush_all()` loses the total.";
    let mut findings = vec![finding("src/billing.rs", Some(SUM_LINE), body)];
    let report = enforce_line_citations(&mut findings, &billing_index());
    assert_eq!((report.dropped, report.partial), (0, 1));
    let f = &findings[0];
    assert!(f.citation_partial, "{f:?}");
    assert_eq!(f.line, Some(SUM_LINE));
    assert!(f.effort != Effort::High && f.confidence <= 0.65, "{f:?}");
    assert!(
        f.description.contains("Citation partly unverified"),
        "{f:?}"
    );
}

/// #8949 fix 1: a drop names the fragment that failed and keeps the finding.
#[test]
fn a_dropped_finding_names_its_missing_snippet() {
    let body = "`ledger.flush_all()` is never awaited, so the total is lost.";
    let mut findings = vec![finding("src/billing.rs", Some(SUM_LINE), body)];
    let report = enforce_line_citations(&mut findings, &billing_index());
    assert!(findings.is_empty());
    let withheld = &report.withheld_findings[0];
    assert_eq!(
        withheld.missing_fragment.as_deref(),
        Some("ledger.flush_all()")
    );
    assert_eq!(withheld.reason, SNIPPET_ABSENT);
    assert_eq!(withheld.finding.description, body);
}

/// #8949 fix 3: a double-quoted English phrase is not required code.
#[test]
fn a_double_quoted_prose_phrase_is_not_a_required_snippet() {
    for body in [
        "`amounts.iter().sum::<u64>()` can overflow, so the invoice is \"silently truncated\".",
        "`let total = amounts.iter().sum::<u64>();` never logs \"total overflowed\" here.",
    ] {
        let kept = gate_one("src/billing.rs", Some(SUM_LINE), body, &billing_index());
        assert_eq!(kept.len(), 1, "{body}");
        assert!(!kept[0].citation_partial, "{body}: {:?}", kept[0]);
    }
}

/// Error arm (#8949): a partial finding whose present quote is ambiguous off
/// the cited line still drops, naming the missing fragment.
#[test]
fn a_partial_finding_that_cannot_be_placed_is_dropped() {
    let body = "`(input)` is unchecked before `ledger.flush_all()`.";
    let mut findings = vec![finding("src/billing.rs", Some(SUM_LINE), body)];
    let report = enforce_line_citations(&mut findings, &billing_index());
    assert!(findings.is_empty(), "{findings:?}");
    assert_eq!(
        report.withheld_findings[0].missing_fragment.as_deref(),
        Some("ledger.flush_all()")
    );
}

/// Row 1: the critic's input — a quote absent from the file, whose words are
/// on the cited line, does not verify.
#[test]
fn a_fabricated_quote_does_not_verify_through_its_own_identifiers() {
    let body = "`amounts.iter().sum::<u64>().unwrap()` panics on overflow.";
    let kept = gate_one("src/billing.rs", Some(SUM_LINE), body, &billing_index());
    assert!(kept.is_empty(), "{kept:?}");
}

/// Row 2. Fail-open mutation: `Run::new(false, …)` in `flush_removed`.
#[test]
fn an_old_side_line_number_never_satisfies_a_citation() {
    let index = LineIndex::from_filtered(&diff(vec![file(
        "src/load.rs",
        FileDisposition::Kept,
        vec![hunk(
            "@@ -100,3 +150,3 @@",
            lines(&[
                " fn load(x: &str) -> u32 {",
                "-  let n = parse(x).unwrap();",
                "+  let n = parse(x)?;",
                "   n",
            ]),
        )],
    )]));
    let kept = gate_one(
        "src/load.rs",
        Some(101),
        "`parse(x).unwrap()` panics.",
        &index,
    );
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].line, Some(151));
    assert!(kept[0].citation_correction.is_some_and(|c| c.removed_code));
}

#[test]
fn a_removed_line_counts_only_at_its_new_side_position() {
    let index = LineIndex::from_filtered(&diff(vec![file(
        "src/a.rs",
        FileDisposition::Kept,
        vec![hunk(
            "@@ -10,3 +10,2 @@",
            lines(&[" fn run() {", "-    guard.check()?;", "     work();"]),
        )],
    )]));
    let mut findings = vec![finding(
        "src/a.rs",
        Some(11),
        "Removing `guard.check()?` drops auth.",
    )];
    assert_eq!(gate(&mut findings, &index), (0, 1));
    assert_eq!(
        findings[0].citation_correction,
        Some(CitationCorrection {
            from_line: Some(11),
            to_line: 11,
            removed_code: true,
        })
    );
}

/// Row 3: `b'e'` inside a double-quoted excerpt is not a snippet of its own.
#[test]
fn a_char_literal_in_an_excerpt_is_not_a_snippet() {
    let body = "Bad match [code: `src/billing.rs:17` — \"if b == b'e'\"] here.";
    let kept = gate_one("src/billing.rs", Some(17), body, &billing_index());
    assert!(kept.is_empty(), "{kept:?}");
}

/// Row 3: `?;` and `Ok(())` match almost anywhere, so they anchor nothing.
#[test]
fn a_generic_snippet_does_not_anchor() {
    let index = LineIndex::from_filtered(&diff(vec![file(
        "src/io.rs",
        FileDisposition::Kept,
        vec![hunk(
            "@@ -0,0 +1,3 @@",
            lines(&["+    read(a)?;", "+    write(b)?;", "+    Ok(())"]),
        )],
    )]));
    for body in ["`?;` swallows the error.", "`Ok(())` hides the failure."] {
        let kept = gate_one("src/io.rs", Some(1), body, &index);
        assert!(kept.is_empty(), "{body}: {kept:?}");
    }
}

/// Row 3. Fail-open mutation: `o.len() >= 1` for `o.len() == 1` in the
/// re-anchor arm of `check_citation`.
#[test]
fn an_ambiguous_anchor_off_the_cited_line_is_dropped() {
    let kept = gate_one(
        "src/billing.rs",
        Some(SUM_LINE),
        "`(input)` is unchecked.",
        &billing_index(),
    );
    assert!(kept.is_empty(), "{kept:?}");
}

/// Row 6: a lineless `[code: …]` locator is checked in its own file.
#[test]
fn a_lineless_bracket_is_checked_in_its_own_file() {
    let body = format!("{SUM_QUOTE} [code: `src/other.rs` — \"never_called_helper()\"]");
    let kept = gate_one(
        "src/billing.rs",
        Some(SUM_LINE),
        &body,
        &two_file_index("+fn other_helper() {}"),
    );
    assert!(kept.is_empty(), "{kept:?}");
}

/// Row 6: an excerpt for `src/other.rs` does not anchor `src/billing.rs`.
#[test]
fn a_bracket_excerpt_anchors_only_its_own_file() {
    let body = "Overflow [code: `src/other.rs:1` — \"amounts.iter().sum::<u64>()\"]";
    let index = two_file_index("+let total = amounts.iter().sum::<u64>();");
    let kept = gate_one("src/billing.rs", Some(18), body, &index);
    assert!(kept.is_empty(), "{kept:?}");
}

/// Row 7: a padded locator is rewritten by byte range.
#[test]
fn a_padded_locator_is_rewritten_in_place() {
    let body =
        format!("{SUM_QUOTE} [code: ` src/billing.rs:18 ` — \"let total = amounts.iter()\"]");
    let kept = gate_one("src/billing.rs", Some(SUM_LINE), &body, &billing_index());
    assert_eq!(kept.len(), 1);
    assert!(
        kept[0].description.contains("`src/billing.rs:30`") && !kept[0].description.contains(":18"),
        "{}",
        kept[0].description
    );
}

// ─── Fail closed (#8905 item 4) ────────────────────────────────────────────────

/// Fail-open mutation this pins: `Err(_) => None` in the error arm of
/// `enforce_line_citations`.
#[test]
fn a_file_with_a_malformed_hunk_header_fails_closed() {
    let index = LineIndex::from_filtered(&diff(vec![file(
        "src/billing.rs",
        FileDisposition::Kept,
        vec![hunk("@@ not a header @@", billing_lines())],
    )]));
    let mut findings = vec![finding("src/billing.rs", Some(SUM_LINE), SUM_QUOTE)];
    assert_eq!(gate(&mut findings, &index), (1, 0));
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
    assert_eq!(gate(&mut findings, &index), (3, 0));
}

#[test]
fn a_finding_citing_no_file_is_dropped() {
    let mut findings = vec![finding(UNKNOWN_FILE_PLACEHOLDER, None, SUM_QUOTE)];
    assert_eq!(gate(&mut findings, &billing_index()), (1, 0));
}

// ─── Kept and re-anchored shapes ───────────────────────────────────────────────

#[test]
fn a_finding_whose_line_holds_its_code_is_kept_unchanged() {
    let mut findings = vec![finding("src/billing.rs", Some(SUM_LINE), SUM_QUOTE)];
    assert_eq!(gate(&mut findings, &billing_index()), (0, 0));
    assert_eq!(findings[0].citation_correction, None);
}

#[test]
fn an_identifier_anchor_reanchors_when_no_snippet_is_given() {
    let body = "The call to step_12 discards its error.";
    let kept = gate_one("src/billing.rs", Some(3), body, &billing_index());
    assert_eq!(kept[0].line, Some(12));
}

#[test]
fn a_finding_with_no_line_is_anchored_to_its_code() {
    let kept = gate_one(
        "src/billing.rs",
        None,
        "`sum::<u64>()` can overflow.",
        &billing_index(),
    );
    assert_eq!(kept[0].line, Some(SUM_LINE));
}

#[test]
fn a_bracket_citation_is_rewritten_to_the_verified_line() {
    let body = "Overflow [code: `src/billing.rs:18` — \"let total = amounts.iter()\"] here.";
    let kept = gate_one("src/billing.rs", Some(SUM_LINE), body, &billing_index());
    assert!(
        kept[0].description.contains("`src/billing.rs:30`"),
        "{}",
        kept[0].description
    );
}

#[test]
fn the_gate_is_idempotent() {
    let mut findings = vec![finding("src/billing.rs", Some(SUM_LINE - 12), SUM_QUOTE)];
    let index = billing_index();
    gate(&mut findings, &index);
    assert_eq!(gate(&mut findings, &index), (0, 0));
    assert_eq!(findings[0].line, Some(SUM_LINE));
}

#[test]
fn parse_locator_reads_lines_and_ranges() {
    let parsed = |l: &str| parse_locator(l).ok();
    assert_eq!(
        parsed("a.rs:12"),
        Some(("a.rs".to_string(), Some((12, 12))))
    );
    assert_eq!(
        parsed("a.rs:L3-L5"),
        Some(("a.rs".to_string(), Some((3, 5))))
    );
    assert_eq!(
        parsed("docs/spec.md"),
        Some(("docs/spec.md".to_string(), None))
    );
    assert!(matches!(
        parse_locator("a.rs:12-x"),
        Err(GateError::BadLocator(_))
    ));
}

// ─── Verdict after the gate (#8905 row 4) ──────────────────────────────────────

fn blocking_result(findings: Vec<Finding>) -> ReviewResult {
    let mut result = ReviewResult::new("acme", "api", 7, "t", "https://example.invalid/pr/7");
    result.verdict = Verdict::Block;
    result.grade = Some("F".to_string());
    result.findings = findings;
    result
}

/// Row 4. Fail-open mutation: `*verdict = Verdict::Approve` in the
/// no-survivors arm of `withhold_verdict`.
#[test]
fn gate_posted_findings_withholds_when_it_drops_every_finding() {
    let mut result = blocking_result(vec![finding(
        "src/billing.rs",
        Some(SUM_LINE),
        "An AWS secret key is committed in plain text.",
    )]);
    gate_posted_findings(&mut result, &diff(vec![billing_file()]));

    assert!(result.findings.is_empty());
    assert_eq!(result.verdict, Verdict::Unknown);
    assert_eq!(result.grade, None);
    assert!(
        result
            .review_body
            .starts_with("1 findings withheld: citation unverifiable"),
        "{}",
        result.review_body
    );
}

/// Row 4: a partial drop re-derives from the survivors and never approves.
#[test]
fn gate_posted_findings_never_approves_a_blocking_review() {
    let mut nit = finding("src/billing.rs", Some(SUM_LINE), SUM_QUOTE);
    nit.confidence = 0.3;
    nit.effort = Effort::Low;
    let blocker = finding(
        "src/billing.rs",
        Some(SUM_LINE),
        "An AWS secret key is committed in plain text.",
    );
    let mut result = blocking_result(vec![blocker, nit]);
    gate_posted_findings(&mut result, &diff(vec![billing_file()]));

    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.verdict, Verdict::Unknown);
}

/// #8949 fix 1: the review record carries each withheld finding.
#[test]
fn gate_posted_findings_records_the_withheld_finding() {
    let body = "`ledger.flush_all()` is never awaited, so the total is lost.";
    let mut result = blocking_result(vec![finding("src/billing.rs", Some(SUM_LINE), body)]);
    gate_posted_findings(&mut result, &diff(vec![billing_file()]));

    assert_eq!(result.withheld_findings.len(), 1);
    let json = serde_json::to_value(&result).expect("serialise review result");
    assert_eq!(
        json["withheld_findings"][0]["missing_fragment"],
        "ledger.flush_all()"
    );
}

/// An APPROVE* review whose one finding is advisory.
fn approve_star_result(f: Finding) -> ReviewResult {
    let mut result = blocking_result(vec![f]);
    result.verdict = Verdict::ApproveWithReservations;
    result.grade = Some("C+".to_string());
    result
}

/// #8949 ruling (b): dropping only advisory findings keeps APPROVE*.
#[test]
fn approve_star_survives_when_only_advisory_findings_are_dropped() {
    let mut nit = finding(
        "src/billing.rs",
        Some(SUM_LINE),
        "`ledger.flush_all()` is slow.",
    );
    nit.confidence = 0.3;
    nit.effort = Effort::Low;
    let mut result = approve_star_result(nit);
    gate_posted_findings(&mut result, &diff(vec![billing_file()]));

    assert!(result.findings.is_empty());
    assert_eq!(result.verdict, Verdict::ApproveWithReservations);
    assert_eq!(result.grade.as_deref(), Some("C+"));
    assert_eq!(result.error, None);
}

/// Error arm (#8949 ruling (b)): a dropped finding that could escalate on its
/// own still withholds an APPROVE* review.
#[test]
fn approve_star_is_withheld_when_a_dropped_finding_could_escalate() {
    let blocker = finding(
        "src/billing.rs",
        Some(SUM_LINE),
        "`ledger.flush_all()` loses data.",
    );
    let mut result = approve_star_result(blocker);
    gate_posted_findings(&mut result, &diff(vec![billing_file()]));

    assert_eq!(result.verdict, Verdict::Unknown);
    assert_eq!(result.grade, None);
}

/// Error arm (#8949 ruling (a)): a partial finding is advisory, so it cannot
/// hold a blocking verdict, and the review never relaxes to APPROVE on it.
#[test]
fn a_partial_finding_cannot_carry_a_blocking_verdict() {
    let body = "`amounts.iter().sum::<u64>()` then `ledger.flush_all()` loses the total.";
    let mut result = blocking_result(vec![finding("src/billing.rs", Some(SUM_LINE), body)]);
    gate_posted_findings(&mut result, &diff(vec![billing_file()]));

    assert_eq!(result.findings.len(), 1);
    assert!(result.findings[0].citation_partial);
    assert_eq!(result.verdict, Verdict::Unknown);
    assert!(
        result
            .review_body
            .starts_with("1 findings kept with a partly unverified citation (advisory)"),
        "{}",
        result.review_body
    );
}

/// #8949: a log line carries at most 120 characters of a quoted fragment.
#[test]
fn log_excerpt_caps_a_long_fragment() {
    let long = "x".repeat(300);
    assert_eq!(verdict::log_excerpt(&long).chars().count(), 121);
    assert_eq!(verdict::log_excerpt("short"), "short");
}
