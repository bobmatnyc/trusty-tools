//! Unit tests for the supervisor's pure parsing + policy helpers.
//!
//! Why: The report parser, classifier, retry policy, and task-amendment
//! builders are deterministic and must stay covered without shelling out to a
//! real workflow.
//! What: Co-locates the tests that previously lived in `supervisor.rs::tests`,
//! reaching the parser/policy/types via explicit `super::` paths.
//! Test: This module IS the test surface for `supervisor/{report,policy,types}`.

use super::policy::{
    amend_task_for_retry, build_targeted_retry_task, classify_partial_reason, should_retry,
};
use super::report::{detect_outcome, extract_section, parse_workflow_report};
use super::types::{PartialReason, ReportSummary, RetryDecision, WorkflowOutcome};

#[test]
fn test_parse_workflow_report_success() {
    let report = "# Workflow Report\n\
        \n\
        ## Task\n\
        Write a function.\n\
        \n\
        ## Final Verdict\n\
        `success` — all tests passed.\n\
        \n\
        ## Next Steps\n\
        *(none)*\n\
        \n\
        ## Summary\n\
        All 14 tests passed; the package is shippable.\n";

    let parsed = parse_workflow_report(report);
    assert_eq!(parsed.outcome, WorkflowOutcome::Success);
    assert!(
        parsed.summary.contains("14 tests passed"),
        "summary should be extracted, got: {}",
        parsed.summary
    );
    assert!(parsed.next_steps.is_none(), "*(none)* should be suppressed");
    assert!(parsed.in_scope_next_steps.is_empty());
    assert!(parsed.out_of_scope_items.is_empty());
}

#[test]
fn test_parse_workflow_report_partial() {
    let report = "# Workflow Report\n\
        \n\
        ## Final Verdict\n\
        `partial` — code phase produced no artifacts.\n\
        \n\
        ## Summary\n\
        The run is classified as **partial**: research and plan succeeded \
        but the code phase output was empty.\n\
        \n\
        ## Next Steps\n\
        - Re-run the code phase with a more explicit file-path prompt\n\
        - Ensure pytest collects at least one test\n";

    let parsed = parse_workflow_report(report);
    assert_eq!(parsed.outcome, WorkflowOutcome::Partial);
    assert!(parsed.summary.contains("partial"));
    let steps = parsed
        .next_steps
        .clone()
        .expect("partial report should yield next steps");
    assert!(steps.contains("Re-run the code phase"));
    assert!(steps.contains("pytest"));
    // Both bullets are in-scope for this task (no pre-existing/i18n hints).
    assert_eq!(parsed.in_scope_next_steps.len(), 2);
    assert!(parsed.out_of_scope_items.is_empty());
}

#[test]
fn test_supervisor_retry_task_amendment() {
    let amended = amend_task_for_retry(
        "Write a Python script that formats a markdown table.",
        2,
        3,
        "partial",
        Some("Add a unit test that exercises the table formatter."),
    );

    assert!(amended.starts_with("[Attempt 2 of 3]"));
    assert!(amended.contains("Previous attempt result: partial"));
    assert!(amended.contains("Add a unit test"));
    assert!(amended.contains("Continuing task: Write a Python script"));

    // No next steps provided — should fall back to "none provided".
    let amended_none = amend_task_for_retry("Original task body.", 3, 3, "fail", None);
    assert!(amended_none.contains("none provided"));
    assert!(amended_none.starts_with("[Attempt 3 of 3]"));
}

#[test]
fn detect_outcome_falls_back_to_inline_partial() {
    // No Final Verdict / Status section — only inline mention.
    let report = "## Summary\n\nThe run is classified as **partial** today.\n";
    assert_eq!(detect_outcome(report), WorkflowOutcome::Partial);
}

#[test]
fn extract_section_returns_none_for_missing_heading() {
    let report = "## Summary\nbody\n";
    assert!(extract_section(report, "Next Steps").is_none());
}

#[test]
fn test_parse_workflow_report_segregates_next_steps() {
    let report = "## Final Verdict\n\
        `partial` — qa failures.\n\
        \n\
        ## Summary\n\
        Code phase complete; QA failed on pre-existing i18n issues.\n\
        \n\
        ## Completed Work\n\
        - Implemented the new endpoint\n\
        - Added 3 unit tests\n\
        \n\
        ## Next Steps\n\
        - Fix pre-existing i18n translation gap in checkout flow\n\
        - Investigate flaky test in user-service (unrelated to this task)\n\
        - Add another test case for the new endpoint\n";
    let parsed = parse_workflow_report(report);
    assert_eq!(parsed.outcome, WorkflowOutcome::Partial);
    assert_eq!(parsed.completed_work.len(), 2);
    assert!(parsed.completed_work[0].contains("endpoint"));
    assert_eq!(parsed.out_of_scope_items.len(), 2, "i18n + flaky → out");
    assert_eq!(parsed.in_scope_next_steps.len(), 1, "the new test case");
    assert!(parsed.in_scope_next_steps[0].contains("Add another test"));
}

fn make_summary(
    outcome: WorkflowOutcome,
    summary: &str,
    in_scope: &[&str],
    out_of_scope: &[&str],
    completed: &[&str],
) -> ReportSummary {
    ReportSummary {
        outcome,
        summary: summary.to_string(),
        next_steps: None,
        completed_work: completed.iter().map(|s| s.to_string()).collect(),
        in_scope_next_steps: in_scope.iter().map(|s| s.to_string()).collect(),
        out_of_scope_items: out_of_scope.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn test_classify_qa_preexisting_failures() {
    let s = make_summary(
        WorkflowOutcome::Partial,
        "Code phase complete; QA failed on pre-existing i18n issues.",
        &[],
        &[
            "Fix pre-existing i18n gap",
            "Investigate unrelated flaky test",
        ],
        &["Implemented endpoint"],
    );
    assert_eq!(
        classify_partial_reason(Some(&s)),
        PartialReason::QaPreExistingFailures
    );
}

#[test]
fn test_classify_code_incomplete() {
    let s = make_summary(
        WorkflowOutcome::Partial,
        "Implementation only partially done; missing the validation layer.",
        &["Implement input validation", "Wire up the new route"],
        &[],
        &["Scaffolded module"],
    );
    assert_eq!(
        classify_partial_reason(Some(&s)),
        PartialReason::CodeIncomplete
    );
}

#[test]
fn test_classify_qa_task_related_failures() {
    let s = make_summary(
        WorkflowOutcome::Partial,
        "QA failed: 2 tests in the new module failed.",
        &[
            "Fix assertion in test_new_module::test_happy_path",
            "Fix off-by-one in test_new_module::test_edge_case",
        ],
        &[],
        &["Implemented new module"],
    );
    assert_eq!(
        classify_partial_reason(Some(&s)),
        PartialReason::QaTaskRelatedFailures
    );
}

#[test]
fn test_classify_workflow_crash() {
    assert_eq!(classify_partial_reason(None), PartialReason::WorkflowCrash);
}

#[test]
fn test_should_retry_decisions() {
    // Pre-existing failures → success-with-caveats regardless of attempt.
    assert_eq!(
        should_retry(PartialReason::QaPreExistingFailures, 1, 3),
        RetryDecision::SuccessWithCaveats
    );
    assert_eq!(
        should_retry(PartialReason::QaPreExistingFailures, 3, 3),
        RetryDecision::SuccessWithCaveats
    );
    // Code incomplete in budget → retry.
    assert_eq!(
        should_retry(PartialReason::CodeIncomplete, 1, 3),
        RetryDecision::Retry
    );
    // Code incomplete out of budget → blocked.
    assert_eq!(
        should_retry(PartialReason::CodeIncomplete, 3, 3),
        RetryDecision::Blocked
    );
    // QA-task failures in budget → RetryQaOnly.
    assert_eq!(
        should_retry(PartialReason::QaTaskRelatedFailures, 2, 3),
        RetryDecision::RetryQaOnly
    );
    // Workflow crash in budget → retry.
    assert_eq!(
        should_retry(PartialReason::WorkflowCrash, 1, 3),
        RetryDecision::Retry
    );
    // Workflow crash exhausted → blocked.
    assert_eq!(
        should_retry(PartialReason::WorkflowCrash, 3, 3),
        RetryDecision::Blocked
    );
    // Unknown in budget → retry, exhausted → blocked.
    assert_eq!(
        should_retry(PartialReason::Unknown, 1, 3),
        RetryDecision::Retry
    );
    assert_eq!(
        should_retry(PartialReason::Unknown, 3, 3),
        RetryDecision::Blocked
    );
}

#[test]
fn test_targeted_retry_task_code_incomplete() {
    let s = make_summary(
        WorkflowOutcome::Partial,
        "Implementation partial.",
        &["Wire up POST /foo handler", "Add validation"],
        &[],
        &["Created the foo module skeleton"],
    );
    let task = build_targeted_retry_task(
        "Build a foo service with POST /foo and validation.",
        2,
        3,
        PartialReason::CodeIncomplete,
        Some(&s),
    );
    assert!(task.starts_with("[Attempt 2 of 3 — continuing incomplete work]"));
    assert!(task.contains("Created the foo module skeleton"));
    assert!(task.contains("Wire up POST /foo handler"));
    assert!(task.contains("Add validation"));
    assert!(task.contains("Do NOT redo completed work"));
}

#[test]
fn test_targeted_retry_task_qa_related() {
    let s = make_summary(
        WorkflowOutcome::Partial,
        "QA failed: 1 test in the new module failed.",
        &["Fix assertion in test_handler::test_happy_path"],
        &[],
        &[],
    );
    let task = build_targeted_retry_task(
        "Add a new handler.",
        2,
        3,
        PartialReason::QaTaskRelatedFailures,
        Some(&s),
    );
    assert!(task.contains("[Attempt 2 of 3 — fixing QA failures introduced"));
    assert!(task.contains("test_handler::test_happy_path"));
    assert!(task.contains("without changing the core task deliverables"));
}

#[test]
fn test_success_with_caveats_decision_for_preexisting() {
    // Verifies that the policy + classifier compose to declare done-enough
    // when only out-of-scope failures remain.
    let s = make_summary(
        WorkflowOutcome::Partial,
        "Code phase complete; QA failed on pre-existing flaky tests.",
        &[],
        &["Pre-existing flaky test in legacy/foo.py"],
        &["Implemented requested change"],
    );
    let reason = classify_partial_reason(Some(&s));
    assert_eq!(reason, PartialReason::QaPreExistingFailures);
    assert_eq!(
        should_retry(reason, 1, 3),
        RetryDecision::SuccessWithCaveats
    );
}
