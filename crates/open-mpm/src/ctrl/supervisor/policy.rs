//! Smart-retry classification + policy + retry-task builders.
//!
//! Why: Separating "what happened" (classification), "what to do" (policy), and
//! "how to phrase the next attempt" (task builders) from the loop keeps each a
//! pure, unit-testable function and keeps `run()` readable.
//! What: `classify_partial_reason`, `should_retry`, `amend_task_for_retry`,
//! `build_targeted_retry_task`, plus the `bullet_list` prompt helper.
//! Test: `supervisor/tests.rs` (`test_classify_*`, `test_should_retry_decisions`,
//! `test_supervisor_retry_task_amendment`, `test_targeted_retry_task_*`).

use super::types::{PartialReason, ReportSummary, RetryDecision, WorkflowOutcome};

/// Classify why a non-success workflow ended the way it did.
///
/// Why: The smart retry policy needs to distinguish "we still owe code work"
/// from "QA tripped on noise we didn't introduce" from "QA caught a regression
/// we did introduce" so it can pick a targeted action instead of a blunt
/// re-run. The classifier inspects the parsed report (next-steps segregation,
/// summary text) and falls back to `Unknown` when signals conflict.
/// What: Returns `WorkflowCrash` if the report is missing; otherwise inspects
/// `out_of_scope_items` vs `in_scope_next_steps` and the summary text to pick
/// one of the four populated reasons. Heuristic-only — never panics.
/// Test: `test_classify_qa_preexisting_failures`, `test_classify_code_incomplete`,
/// `test_classify_qa_task_related_failures`, `test_classify_workflow_crash`.
pub fn classify_partial_reason(report: Option<&ReportSummary>) -> PartialReason {
    let Some(r) = report else {
        return PartialReason::WorkflowCrash;
    };

    let summary_lc = r.summary.to_ascii_lowercase();
    let mentions_qa_fail =
        summary_lc.contains("qa") && (summary_lc.contains("fail") || summary_lc.contains("failed"));
    let mentions_code_complete = summary_lc.contains("code phase")
        && (summary_lc.contains("complete")
            || summary_lc.contains("succeeded")
            || summary_lc.contains("done"));
    let mentions_changes_complete = summary_lc.contains("changes complete")
        || summary_lc.contains("implementation complete")
        || summary_lc.contains("task complete");

    let has_out_of_scope = !r.out_of_scope_items.is_empty();
    let has_in_scope = !r.in_scope_next_steps.is_empty();

    // Strongest signal: explicit out-of-scope items + nothing left in scope.
    if has_out_of_scope && !has_in_scope {
        return PartialReason::QaPreExistingFailures;
    }

    // Code phase reported complete + QA failed and the next-steps look
    // pre-existing → treat as pre-existing failures.
    if (mentions_code_complete || mentions_changes_complete) && mentions_qa_fail && has_out_of_scope
    {
        return PartialReason::QaPreExistingFailures;
    }

    // QA failed and remaining steps look like new failures (in scope) →
    // task-related QA failures.
    if mentions_qa_fail && has_in_scope && !has_out_of_scope {
        return PartialReason::QaTaskRelatedFailures;
    }

    // Anything still in scope → code incomplete.
    if has_in_scope {
        return PartialReason::CodeIncomplete;
    }

    // No structured next steps but we got a partial/fail outcome.
    if r.outcome == WorkflowOutcome::Partial || r.outcome == WorkflowOutcome::Fail {
        if mentions_qa_fail && (mentions_code_complete || mentions_changes_complete) {
            return PartialReason::QaPreExistingFailures;
        }
        return PartialReason::CodeIncomplete;
    }

    PartialReason::Unknown
}

/// Pure retry-policy decision: given a classified reason and budget, return
/// the action the supervisor should take.
///
/// Why: Keeping policy in a pure function makes it trivially unit-testable and
/// keeps `run()` readable.
/// What: See match arms — `QaPreExistingFailures` short-circuits to
/// `SuccessWithCaveats`; `WorkflowCrash` after exhausting attempts goes to
/// `Blocked`; in-budget partials retry (full or QA-only depending on cause).
/// Test: `test_should_retry_decisions`.
pub fn should_retry(reason: PartialReason, attempt: u32, max_attempts: u32) -> RetryDecision {
    match reason {
        PartialReason::QaPreExistingFailures => RetryDecision::SuccessWithCaveats,
        PartialReason::CodeIncomplete | PartialReason::Unknown if attempt < max_attempts => {
            RetryDecision::Retry
        }
        PartialReason::QaTaskRelatedFailures if attempt < max_attempts => {
            RetryDecision::RetryQaOnly
        }
        PartialReason::WorkflowCrash if attempt < max_attempts => RetryDecision::Retry,
        _ => RetryDecision::Blocked,
    }
}

/// Build the amended task string used on retry attempts N >= 2 (legacy / generic).
///
/// Why: Kept for the workflow-crash path where we don't have a parsed report
/// to drive a targeted retry. `build_targeted_retry_task` is the smarter
/// successor used when we do have classification.
/// What: Returns "[Attempt N of M] Previous attempt result: <outcome>.
/// Previous next steps: <steps|none>. Continuing task: <original task>".
/// Test: `test_supervisor_retry_task_amendment`.
pub fn amend_task_for_retry(
    original: &str,
    attempt: u32,
    max_attempts: u32,
    outcome: &str,
    next_steps: Option<&str>,
) -> String {
    let steps = next_steps
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "none provided".to_string());
    format!(
        "[Attempt {n} of {m}] Previous attempt result: {outcome}. \
         Previous next steps: {steps}. \
         Continuing task: {original}",
        n = attempt,
        m = max_attempts,
        outcome = outcome,
        steps = steps,
        original = original,
    )
}

/// Build a targeted retry task string driven by `PartialReason`.
///
/// Why: A blunt "re-run with prior next-steps" prompt confuses the LLM when
/// only a small slice of work remains, or when the failures are pre-existing
/// noise. Tailoring the prompt per reason produces tighter follow-up runs and
/// avoids re-doing already-completed work.
/// What:
/// - `CodeIncomplete` / `Unknown`: include completed work (so the LLM doesn't
///   redo it) + only in-scope next steps (so it focuses on remaining work).
/// - `QaTaskRelatedFailures`: instruct the LLM to fix the QA failures it
///   introduced without changing core deliverables.
/// - `WorkflowCrash`: degrade to the legacy generic amendment.
/// - `QaPreExistingFailures`: caller should use `SuccessWithCaveats`, not
///   retry — this function emits a generic prompt as a defensive default.
/// Test: `test_targeted_retry_task_code_incomplete`,
/// `test_targeted_retry_task_qa_related`.
pub fn build_targeted_retry_task(
    original: &str,
    attempt: u32,
    max_attempts: u32,
    reason: PartialReason,
    report: Option<&ReportSummary>,
) -> String {
    match reason {
        PartialReason::CodeIncomplete | PartialReason::Unknown => {
            let completed = report
                .map(|r| bullet_list(&r.completed_work))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "(not reported)".to_string());
            let remaining = report
                .map(|r| bullet_list(&r.in_scope_next_steps))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "(see original task)".to_string());
            format!(
                "[Attempt {n} of {m} — continuing incomplete work]\n\
                 Original task: {original}\n\n\
                 Previous attempt completed:\n{completed}\n\n\
                 Still needed (focus only on these):\n{remaining}\n\n\
                 Do NOT redo completed work. Focus only on the remaining items above.",
                n = attempt,
                m = max_attempts,
                original = original,
                completed = completed,
                remaining = remaining,
            )
        }
        PartialReason::QaTaskRelatedFailures => {
            let failures = report
                .map(|r| bullet_list(&r.in_scope_next_steps))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "(see workflow report)".to_string());
            format!(
                "[Attempt {n} of {m} — fixing QA failures introduced by previous attempt]\n\
                 Original task: {original}\n\n\
                 QA found these new failures introduced by the previous attempt:\n{failures}\n\n\
                 Fix them without changing the core task deliverables. \
                 Run the relevant tests to verify before finishing.",
                n = attempt,
                m = max_attempts,
                original = original,
                failures = failures,
            )
        }
        PartialReason::WorkflowCrash => amend_task_for_retry(
            original,
            attempt,
            max_attempts,
            "fail",
            report.and_then(|r| r.next_steps.as_deref()),
        ),
        PartialReason::QaPreExistingFailures => {
            // Defensive default: caller should declare SuccessWithCaveats, but
            // if they retry anyway, fall back to the generic amendment.
            amend_task_for_retry(
                original,
                attempt,
                max_attempts,
                "partial",
                report.and_then(|r| r.next_steps.as_deref()),
            )
        }
    }
}

/// Render a list of items as a markdown bullet list for prompt embedding.
fn bullet_list(items: &[String]) -> String {
    items
        .iter()
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!("- {}", s.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}
