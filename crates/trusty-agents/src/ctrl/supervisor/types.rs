//! Supervisor outcome / classification value types.
//!
//! Why: The supervisor branches on small categorical states (workflow outcome,
//! partial reason, retry decision, final outcome). Isolating those enums/structs
//! from the loop, parser, and policy keeps each concern under the line cap and
//! lets the parser + policy share the same vocabulary without a circular dep.
//! What: `WorkflowOutcome`, `ReportSummary`, `PartialReason`, `RetryDecision`,
//! and `SupervisorOutcome`.
//! Test: Exercised by the parser/policy unit tests in `supervisor/tests.rs`.

use uuid::Uuid;

/// Outcome categorisation parsed from a workflow report.
///
/// Why: The supervisor branches on three states (continue retrying / stop /
/// escalate); a small enum keeps that decision explicit instead of stringly
/// typed comparisons scattered through `run()`.
/// What: Three variants matching the strings the workflow engine writes
/// (`success`, `partial`, `fail`).
/// Test: `test_parse_workflow_report_success`, `test_parse_workflow_report_partial`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowOutcome {
    Success,
    Partial,
    Fail,
}

impl WorkflowOutcome {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            WorkflowOutcome::Success => "success",
            WorkflowOutcome::Partial => "partial",
            WorkflowOutcome::Fail => "fail",
        }
    }
}

/// Structured view of `<out-dir>/workflow-report.md`.
///
/// Why: The supervisor consumes several pieces from the report (outcome,
/// summary, completed work, segregated next steps) and we want them returned
/// from a single parse pass we can unit-test without spawning a real workflow.
/// The smart-retry logic (#408 follow-up) needs to know which next-steps items
/// are in scope vs. pre-existing so the retry can be targeted (or skipped
/// entirely when remaining failures are out of scope).
/// What: Outcome plus optional summary, the verbatim next-steps blob, plus
/// derived `completed_work`, `in_scope_next_steps`, and `out_of_scope_items`
/// extracted via simple heuristics (see `is_out_of_scope_step`).
/// Test: `test_parse_workflow_report_success`, `test_parse_workflow_report_partial`,
/// `test_parse_workflow_report_segregates_next_steps`.
#[derive(Debug, Clone)]
pub struct ReportSummary {
    pub outcome: WorkflowOutcome,
    pub summary: String,
    pub next_steps: Option<String>,
    /// Items extracted from `## Completed` / `## Completed Work` (or, lacking
    /// that, the first paragraph of `## Summary`). Best-effort, may be empty.
    pub completed_work: Vec<String>,
    /// Subset of `next_steps` that look like real follow-up work for the
    /// original task — empty if all remaining items are out of scope.
    pub in_scope_next_steps: Vec<String>,
    /// Subset of `next_steps` flagged as pre-existing / unrelated noise.
    pub out_of_scope_items: Vec<String>,
}

/// Why a workflow returned a non-success outcome.
///
/// Why: Naive retry re-runs the entire workflow regardless of cause; the smart
/// supervisor needs to distinguish "we still owe work" from "QA failed on
/// pre-existing problems we shouldn't fix" so it can either escalate, declare
/// done-enough, or run a targeted retry.
/// What: Five categorical reasons derived from `classify_partial_reason`.
/// Test: `test_classify_qa_preexisting_failures`, `test_classify_code_incomplete`,
/// `test_classify_qa_task_related_failures`, `test_classify_workflow_crash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialReason {
    /// Code phase didn't finish the originally requested task.
    CodeIncomplete,
    /// QA reported failures that look pre-existing / unrelated to the task.
    QaPreExistingFailures,
    /// QA reported failures that look like regressions introduced by this run.
    QaTaskRelatedFailures,
    /// No workflow-report.md was produced — the workflow itself crashed.
    WorkflowCrash,
    /// Couldn't classify confidently (treated like CodeIncomplete by retry policy).
    Unknown,
}

/// Decision the supervisor makes after classifying a partial/fail outcome.
///
/// Why: Splitting "what happened" (`PartialReason`) from "what to do"
/// (`RetryDecision`) keeps `should_retry` a pure function of (reason, attempt,
/// budget) and avoids tangling policy with parsing.
/// What: Four decisions covering the smart-retry policy.
/// Test: `test_should_retry_decisions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Re-run the full workflow with an amended task focused on remaining work.
    Retry,
    /// Re-run only the QA phase / fix-introduced-failures path (we currently
    /// implement this as a full re-run with a QA-focused amendment, but we
    /// surface the intent so callers / tests can reason about it).
    RetryQaOnly,
    /// Done enough — return Success-with-caveats.
    SuccessWithCaveats,
    /// Give up — escalate to Blocked.
    Blocked,
}

/// Final result of a supervised run.
///
/// Why: The CLI dispatcher needs both the outcome and the session id (so users
/// can refer to it via `om session list`) plus enough context to print a
/// useful message.
/// What: Two variants — success carries the workflow summary, blocked carries
/// the last-attempt reason and the number of attempts consumed.
/// Test: Surfaced by the CLI integration; the parsing layer is unit-tested.
pub enum SupervisorOutcome {
    Success {
        summary: String,
        session_id: Uuid,
        attempts: u32,
    },
    /// The task is "done enough" — code phase succeeded, but QA surfaced
    /// failures that are out of scope (pre-existing, unrelated). Supervisor
    /// declines to retry indefinitely on noise it didn't introduce.
    SuccessWithCaveats {
        summary: String,
        caveats: Vec<String>,
        session_id: Uuid,
        attempts: u32,
    },
    Blocked {
        reason: String,
        attempts: u32,
        session_id: Uuid,
    },
}
