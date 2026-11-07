//! Unit tests for pre-gate quote admission (#10782).

use super::*;
use crate::models::Effort;
use crate::pipeline::withheld_contract::{reason_class, withheld_by_reason};

fn finding(body: &str, quote: Option<&str>) -> Finding {
    let mut f = Finding::new("src/billing.rs", "overflow", body, "", 0.3, Effort::Low);
    f.line = Some(30);
    f.quoted_snippet = quote.map(str::to_string);
    f
}

fn admit(findings: &mut Vec<Finding>) -> Vec<WithheldFinding> {
    let mut withheld = Vec::new();
    let n = admit_quoted_findings(findings, &mut withheld);
    assert_eq!(n, withheld.len());
    withheld
}

/// #10782 error arm: a "confirm X" note with nothing to quote is withheld
/// before the gate, under `no_quote_pregate`, and counted.
#[test]
fn a_finding_without_a_quote_is_withheld_before_the_gate() {
    let mut findings = vec![
        finding("Confirm getMetricContext is imported in context.ts.", None),
        finding("x", Some("let total = amounts.iter().sum::<u64>();")),
    ];
    let withheld = admit(&mut findings);
    assert_eq!(findings.len(), 1);
    assert_eq!(withheld.len(), 1);
    assert_eq!(withheld[0].reason, NO_QUOTE_PREGATE_REASON);
    assert_eq!(reason_class(&withheld[0].reason), "no_quote_pregate");
    assert_eq!(withheld_by_reason(&withheld)["no_quote_pregate"], 1);
}

/// #10782 error arm: a blank quote, or one too short to be specific, is no
/// quote; the withheld record keeps what the reviewer sent.
#[test]
fn a_blank_or_unspecific_quote_counts_as_no_quote() {
    for sent in ["   ", "();", "}"] {
        let mut findings = vec![finding("Check the caller handles this.", Some(sent))];
        let withheld = admit(&mut findings);
        assert!(findings.is_empty(), "{sent:?}");
        assert_eq!(withheld[0].finding.quoted_snippet.as_deref(), Some(sent));
    }
}

/// #10782: a reply from a reviewer that predates the field takes the first
/// code its body quotes; a bare identifier is not a quote (#9188 E).
#[test]
fn a_reply_without_the_field_takes_its_quote_from_the_body() {
    let mut findings = vec![
        finding("`amounts.iter().sum::<u64>()` can overflow.", None),
        finding("Verify `step_4` is called.", None),
    ];
    let withheld = admit(&mut findings);
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0].quoted_snippet.as_deref(),
        Some("amounts.iter().sum::<u64>()")
    );
    assert_eq!(withheld.len(), 1);
}

/// #10782: a usable structured quote is kept exactly as sent, over any body
/// quote.
#[test]
fn a_structured_quote_is_kept_as_sent() {
    let quote = "let total =\n    amounts.iter().sum::<u64>();";
    let mut findings = vec![finding("`step_1(input)` first.", Some(quote))];
    assert!(admit(&mut findings).is_empty());
    assert_eq!(findings[0].quoted_snippet.as_deref(), Some(quote));
}
