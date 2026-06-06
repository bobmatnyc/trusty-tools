//! CTRL supervisor — `om session run` workflow executor with retry and escalation (#408).
//!
//! Why: `om session new` is fire-and-forget — it spawns a session and returns. There is
//! no way to ask the harness "drive this task to completion and tell me what happened".
//! The supervisor closes that gap: it creates a CTRL session record, invokes the
//! prescriptive workflow as a subprocess, parses `workflow-report.md`, and either
//! reports success, retries with an amended task on partial/failure (up to a
//! caller-specified `max_attempts`), or escalates with a "Blocked: <reason>" message
//! plus the last-known next-steps so the user can pick up where the loop gave up.
//!
//! What: This module is split into focused submodules:
//! - `types` — outcome/classification value types shared by parser + policy + loop
//! - `report` — `workflow-report.md` parsing helpers
//! - `policy` — partial-reason classification, retry policy, retry-task builders
//! - `runner` — the `CtrlSupervisor` create-session / spawn / parse / retry loop
//!
//! Test: `supervisor/tests.rs` covers parsing and policy without shelling out.
//! End-to-end behavior is exercised manually by `om session run`.

mod policy;
mod report;
mod runner;
mod types;

#[cfg(test)]
mod tests;

// Public surface — preserve the pre-split API for downstream callers.
pub use policy::{
    amend_task_for_retry, build_targeted_retry_task, classify_partial_reason, should_retry,
};
pub use report::parse_workflow_report;
pub use runner::CtrlSupervisor;
pub use types::{PartialReason, ReportSummary, RetryDecision, SupervisorOutcome, WorkflowOutcome};
