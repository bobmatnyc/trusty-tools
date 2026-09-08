//! Unit tests for the `issue_audit_recent` doctor fold (#7097).

use super::*;
use crate::core::issue_audit::{AuditRow, Verdict};

/// An audit whose every row passed.
fn clean(number: u64) -> IssueAudit {
    IssueAudit {
        number,
        rows: vec![
            AuditRow {
                requirement: "project",
                verdict: Verdict::Pass,
                detail: "trusty-mpm".to_string(),
            },
            AuditRow {
                requirement: "milestone",
                verdict: Verdict::Pass,
                detail: "Backlog · mpm/core".to_string(),
            },
        ],
    }
}

/// An audit failing one named requirement.
fn broken(number: u64, requirement: &'static str) -> IssueAudit {
    IssueAudit {
        number,
        rows: vec![AuditRow {
            requirement,
            verdict: Verdict::Fail,
            detail: "absent".to_string(),
        }],
    }
}

#[test]
fn issue_audit_clean_sweep_is_ok() {
    let check = build_issue_audit_check(Ok(vec![clean(7093), clean(7092)]));
    assert_eq!(check.status, CheckStatus::Ok);
    assert!(check.message.contains('2'), "{}", check.message);
}

#[test]
fn issue_audit_empty_window_is_ok() {
    // A week with no new issues is a clean week, not an unknown one.
    let check = build_issue_audit_check(Ok(Vec::new()));
    assert_eq!(check.status, CheckStatus::Ok);
}

#[test]
fn issue_audit_violations_warn_with_the_numbers() {
    let check = build_issue_audit_check(Ok(vec![
        clean(7093),
        broken(7104, "component label"),
        broken(7103, "milestone"),
    ]));
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.message.contains("#7104"), "{}", check.message);
    assert!(check.message.contains("#7103"), "{}", check.message);
    assert!(
        check.message.contains("component label"),
        "the warning names which requirement each issue missed: {}",
        check.message
    );
    assert!(
        !check.message.contains("#7093"),
        "a passing issue is not listed as a violation: {}",
        check.message
    );
}

#[test]
fn issue_audit_gh_failure_is_not_a_pass() {
    // #7097, the Fail-Open Check: an audit that could not run must never report
    // clean. Absent gh and unauthenticated gh both arrive here as an Err.
    for reason in [
        "`gh` is not installed or not on PATH.",
        "`gh issue list` failed (exit 4): gh auth login required",
    ] {
        let check = build_issue_audit_check(Err(reason.to_string()));
        assert_eq!(
            check.status,
            CheckStatus::Unknown,
            "a gh failure must be UNDETERMINED, not Ok: {reason}"
        );
        assert_ne!(check.status, CheckStatus::Ok);
        assert!(
            check.message.contains(reason),
            "the reason is quoted: {}",
            check.message
        );
        assert!(
            check.message.contains("not clean"),
            "the message must not read as a pass: {}",
            check.message
        );
    }
}

#[test]
fn issue_audit_caps_the_named_failures() {
    // A busy week leaves dozens of violations; naming all of them scrolls the
    // rest of `tm doctor` away.
    let audits: Vec<IssueAudit> = (0..NAMED_LIMIT + 5)
        .map(|i| broken(7000 + i as u64, "component label"))
        .collect();
    let check = build_issue_audit_check(Ok(audits));
    assert_eq!(check.status, CheckStatus::Warn);
    assert_eq!(
        check.message.matches('#').count(),
        NAMED_LIMIT,
        "only the first {NAMED_LIMIT} are named: {}",
        check.message
    );
    assert!(check.message.contains("and 5 more"), "{}", check.message);
    assert!(
        check
            .message
            .starts_with(&format!("{} of {}", NAMED_LIMIT + 5, NAMED_LIMIT + 5)),
        "the counts stay exact: {}",
        check.message
    );
}

#[test]
fn issue_audit_check_never_fails() {
    // Ticket hygiene must not turn `tm doctor` red — a red doctor is a signal
    // an operator drops everything for, and a missing milestone is not that.
    let probes = [
        Ok(vec![clean(1)]),
        Ok(vec![broken(2, "project")]),
        Ok(Vec::new()),
        Err("boom".to_string()),
    ];
    for probe in probes {
        let check = build_issue_audit_check(probe);
        assert_ne!(
            check.status,
            CheckStatus::Fail,
            "the check is advisory: {}",
            check.message
        );
        assert_eq!(check.name, CHECK_NAME);
    }
}
