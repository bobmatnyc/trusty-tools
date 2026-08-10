//! Unit tests for `verification_notice` (#4044).

use super::*;
use crate::models::{Effort, Finding};

fn finding(file: &str, kind: &str) -> Finding {
    Finding::new(file, kind, "description", "suggestion", 0.8, Effort::High)
}

/// The PR #5308 shape: a summary naming finding #1 as a merge blocker, where
/// finding index 0 was refuted by the verifier.
fn refuted_first_of_three() -> Vec<Finding> {
    let mut findings = vec![
        finding("src/a.rs", "missing await"),
        finding("src/b.rs", "unchecked index"),
        finding("src/c.rs", "race"),
    ];
    findings[0].verified = Some(VerifyOutcome::Refuted);
    findings[2].verified = Some(VerifyOutcome::Confirmed);
    findings
}

#[test]
fn prepends_banner_naming_each_refuted_finding() {
    let body = "finding #1 is a high-effort defect ... both must be resolved before merge.";
    let out = prepend_verification_notice(body, &refuted_first_of_three());

    assert!(
        out.starts_with(NOTICE_SENTINEL),
        "banner must lead the body"
    );
    assert!(
        out.contains("finding #1 — `src/a.rs`: missing await"),
        "the banner must name the refuted finding by the index the prose uses:\n{out}"
    );
    assert!(
        out.contains("1 of 3 finding(s) were REFUTED"),
        "the banner must state how many of how many:\n{out}"
    );
    assert!(
        out.ends_with(body),
        "the original summary must survive verbatim"
    );
}

#[test]
fn no_banner_when_nothing_was_refuted() {
    let mut findings = refuted_first_of_three();
    findings[0].verified = Some(VerifyOutcome::Confirmed);
    let body = "All three findings stand.";
    assert_eq!(prepend_verification_notice(body, &findings), body);
}

#[test]
fn no_banner_when_there_are_no_findings() {
    assert_eq!(prepend_verification_notice("LGTM", &[]), "LGTM");
}

#[test]
fn error_refuted_is_not_named_as_a_non_blocker() {
    // `ErrorRefuted` / `TruncationRefuted` mean "we could not reach the
    // verifier" (#726, #1876) and `rederive_verdict` path (c) deliberately
    // PRESERVES the escalation for them — calling them non-blockers would
    // contradict the verdict this same review reports.
    let mut findings = vec![finding("src/a.rs", "missing await")];
    findings[0].verified = Some(VerifyOutcome::ErrorRefuted {
        error_class: "RateLimited".to_string(),
    });
    assert_eq!(prepend_verification_notice("body", &findings), "body");

    findings[0].verified = Some(VerifyOutcome::TruncationRefuted);
    assert_eq!(prepend_verification_notice("body", &findings), "body");
}

#[test]
fn prepend_is_idempotent() {
    let findings = refuted_first_of_three();
    let once = prepend_verification_notice("body", &findings);
    let twice = prepend_verification_notice(&once, &findings);
    assert_eq!(
        once, twice,
        "a second finalise must not stack a second banner"
    );
}
