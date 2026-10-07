//! Unit tests for the reported finding severity (`severity.rs`, #9310).

use super::*;
use crate::models::{Verdict, WithheldFinding};
use crate::pipeline::grade::{derive_verdict, derive_verdict_with_grade};
use crate::pipeline::letter_grade::Grade;

/// A finding at `src/a.rs:3` with `effort`, the `code_provable` flag, and a
/// well-formed `source_citation` when `cited`.
fn finding(effort: Effort, code_provable: bool, cited: bool) -> Finding {
    let mut f = Finding::new("src/a.rs", "overflow", "`a + b` overflows", "", 0.9, effort);
    f.line = Some(3);
    f.code_provable = code_provable;
    if cited {
        f.source_citation = Some("src/a.rs:3".to_string());
    }
    f
}

/// #9310 item 2.3: with no reviewer severity, it is derived from effort and
/// citability, and derivation never gives critical.
#[test]
fn severity_derived_from_effort_when_reviewer_omits_it() {
    for (effort, provable, cited, want) in [
        (Effort::Low, false, false, Severity::Low),
        (Effort::Low, true, true, Severity::Low),
        (Effort::Medium, false, false, Severity::Medium),
        (Effort::Medium, true, true, Severity::Medium),
        (Effort::High, true, false, Severity::High),
        (Effort::High, false, true, Severity::High),
        (Effort::High, true, true, Severity::High),
        (Effort::High, false, false, Severity::Medium),
    ] {
        let f = finding(effort.clone(), provable, cited);
        assert_eq!(f.severity, None);
        assert_eq!(
            effective_severity(&f),
            want,
            "{effort:?} provable={provable} cited={cited}"
        );
    }
}

/// #9310 items 2.2 and 2.4: a reviewer severity is kept up to the ceiling its
/// effort allows. Deriving from effort alone reads `high` for the first row.
#[test]
fn reviewer_severity_is_carried_up_to_the_effort_ceiling() {
    for (effort, provable, given, want) in [
        (Effort::High, true, Severity::Critical, Severity::Critical),
        (Effort::High, true, Severity::High, Severity::High),
        (Effort::High, true, Severity::Low, Severity::Low),
        (Effort::High, false, Severity::Critical, Severity::Medium),
        (Effort::Medium, true, Severity::Critical, Severity::Medium),
        (Effort::Low, true, Severity::High, Severity::Low),
    ] {
        let mut f = finding(effort.clone(), provable, false);
        f.severity = Some(given);
        assert_eq!(
            effective_severity(&f),
            want,
            "{effort:?} provable={provable} given={given}"
        );
    }
}

/// #9310 item 2.4: a reviewer's critical finding whose effort a gate demoted
/// from High to Medium is stamped `medium`; undemoted, it stays `critical`.
#[test]
fn demoted_effort_clamps_emitted_severity() {
    let mut critical = finding(Effort::High, true, false);
    critical.severity = Some(Severity::Critical);
    let mut demoted = critical.clone();
    crate::pipeline::evidence_admission::demote_to_unverifiable_advisory(&mut demoted);
    assert_eq!(demoted.effort, Effort::Medium);

    let mut result = ReviewResult::new("acme", "api", 7, "t", "u");
    result.findings = vec![critical, demoted];
    stamp_severities(&mut result);
    let stamped: Vec<_> = result.findings.iter().map(|f| f.severity).collect();
    assert_eq!(
        stamped,
        vec![Some(Severity::Critical), Some(Severity::Medium)]
    );
}

/// Withheld findings are stamped too, and a second stamp changes nothing.
#[test]
fn stamp_severities_covers_withheld_findings_and_is_idempotent() {
    let mut result = ReviewResult::new("acme", "api", 7, "t", "u");
    result.findings = vec![finding(Effort::Medium, false, false)];
    result.withheld_findings = vec![WithheldFinding {
        finding: finding(Effort::High, true, false),
        reason: "x".to_string(),
        missing_fragment: None,
    }];
    stamp_severities(&mut result);
    let once = serde_json::to_value(&result).expect("serialize");
    assert_eq!(once["findings"][0]["severity"], "medium", "{once}");
    assert_eq!(
        once["withheld_findings"][0]["finding"]["severity"], "high",
        "{once}"
    );
    stamp_severities(&mut result);
    assert_eq!(serde_json::to_value(&result).expect("serialize"), once);
}

/// #9310 item 2.5: severity never moves a verdict or a grade. The same
/// findings with their severities scrambled derive the same verdict and grade.
/// No finding has High effort, so a floor that read a `critical` severity as
/// High would move these below-BLOCK verdicts.
#[test]
fn severity_never_changes_the_verdict() {
    let base = vec![
        finding(Effort::Medium, true, false),
        finding(Effort::Medium, false, true),
        finding(Effort::Low, true, false),
        finding(Effort::Low, false, false),
    ];
    assert_ne!(derive_verdict(Verdict::Approve, &base), Verdict::Block);
    let grade: Grade = "B".parse().expect("grade");
    let want_graded = derive_verdict_with_grade(Verdict::Approve, grade, &base);
    let want_plain = derive_verdict(Verdict::Approve, &base);
    let options = [
        None,
        Some(Severity::Low),
        Some(Severity::Medium),
        Some(Severity::High),
        Some(Severity::Critical),
    ];
    for shift in 0..options.len() {
        let mut scrambled = base.clone();
        for (i, f) in scrambled.iter_mut().enumerate() {
            f.severity = options[(i + shift) % options.len()];
        }
        assert_eq!(
            derive_verdict_with_grade(Verdict::Approve, grade, &scrambled),
            want_graded,
            "shift {shift}"
        );
        assert_eq!(
            derive_verdict(Verdict::Approve, &scrambled),
            want_plain,
            "shift {shift}"
        );
    }
}
