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
//! What: `CtrlSupervisor` owns the project dir, task text, agent, and attempt budget.
//! `run()` performs the create-session / spawn-workflow / parse-report / retry loop and
//! returns a `SupervisorOutcome` (success or blocked). Pure parsing helpers
//! (`parse_workflow_report`, `amend_task_for_retry`) are exposed for unit testing.
//!
//! Test: `test_parse_workflow_report_success`, `test_parse_workflow_report_partial`,
//! `test_supervisor_retry_task_amendment` cover parsing and amendment without
//! shelling out. End-to-end behavior is exercised manually by `om session run`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::process::Command;
use uuid::Uuid;

use crate::ctrl_session::{Session, SessionStatus, SessionStore};

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
    fn as_str(&self) -> &'static str {
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

/// Configuration + state for a single `om session run` invocation.
///
/// Why: Bundles all the inputs the loop needs (project, task, attempt budget)
/// and the resolved session record so `run()` reads top-to-bottom.
/// What: Constructed via `new`; `run()` consumes self and drives the loop.
/// Test: `test_supervisor_retry_task_amendment` verifies the retry-task
/// amendment helper directly.
pub struct CtrlSupervisor {
    pub project_dir: PathBuf,
    pub task: String,
    pub agent: String,
    pub max_attempts: u32,
    pub session_name: Option<String>,
    pub port: u16,
}

impl CtrlSupervisor {
    /// Construct a supervisor with the supplied configuration.
    ///
    /// Why: Centralises defaulting (max_attempts, port) so callers in main.rs
    /// don't repeat the same defaults.
    /// What: Returns a populated struct; does not touch disk yet.
    /// Test: Indirect — exercised by the CLI smoke path.
    pub fn new(
        project_dir: PathBuf,
        task: String,
        agent: String,
        max_attempts: u32,
        session_name: Option<String>,
        port: u16,
    ) -> Self {
        Self {
            project_dir,
            task,
            agent,
            max_attempts: max_attempts.max(1),
            session_name,
            port,
        }
    }

    /// Drive the create-session / run-workflow / parse / retry loop.
    ///
    /// Why: Single entry point so the CLI dispatcher just awaits one future.
    /// What: Creates a CTRL session record, then for each attempt spawns
    /// `open-mpm --workflow prescriptive --project-dir ... --task ... --out-dir ...`,
    /// reads the workflow report, and either returns Success, retries with an
    /// amended task, or returns Blocked once `max_attempts` is exhausted. The
    /// session status is updated to `Idle` (success) or `Terminated` (blocked
    /// — we co-opt the existing variant since there is no `Blocked` state on
    /// `SessionStatus`; the intent is recorded by the supervisor's own outcome).
    /// Test: Manual end-to-end. Pure helpers (`parse_workflow_report`,
    /// `amend_task_for_retry`) cover the deterministic logic in unit tests.
    pub async fn run(self) -> Result<SupervisorOutcome> {
        let name = self
            .session_name
            .clone()
            .unwrap_or_else(|| derive_session_name(&self.task));

        let session = Session::new(
            self.project_dir.clone(),
            name,
            self.agent.clone(),
            self.port,
        );
        let session_id = session.id;
        SessionStore::upsert(session)
            .with_context(|| "failed to persist supervised session record")?;

        let mut current_task = self.task.clone();
        let mut last_reason = String::from("workflow did not produce a report");

        for attempt in 1..=self.max_attempts {
            let tmp_dir = std::env::temp_dir().join(format!("om-session-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&tmp_dir)
                .with_context(|| format!("creating supervisor out-dir {}", tmp_dir.display()))?;

            let exit_status = spawn_workflow(&self.project_dir, &current_task, &tmp_dir).await?;

            let report_path = tmp_dir.join("workflow-report.md");
            let summary_opt = if report_path.exists() {
                let content = std::fs::read_to_string(&report_path).with_context(|| {
                    format!("reading workflow report {}", report_path.display())
                })?;
                Some(parse_workflow_report(&content))
            } else {
                None
            };

            // Success short-circuit.
            if let Some(s) = summary_opt.as_ref()
                && s.outcome == WorkflowOutcome::Success
            {
                if let Some(mut sess) = SessionStore::find(&session_id) {
                    sess.status = SessionStatus::Idle;
                    let _ = SessionStore::upsert(sess);
                }
                return Ok(SupervisorOutcome::Success {
                    summary: s.summary.clone(),
                    session_id,
                    attempts: attempt,
                });
            }

            // Classify the partial / fail / crash and ask the policy what to do.
            let reason = classify_partial_reason(summary_opt.as_ref());
            let decision = should_retry(reason, attempt, self.max_attempts);

            // Update last_reason for blocked-path messaging.
            last_reason = match (summary_opt.as_ref(), reason) {
                (Some(s), _) => format!(
                    "outcome={} reason={:?} — {}",
                    s.outcome.as_str(),
                    reason,
                    s.summary.lines().next().unwrap_or("").trim()
                ),
                (None, _) => format!(
                    "workflow crashed (exit={}) — no workflow-report.md",
                    exit_status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "<signal>".into())
                ),
            };

            match decision {
                RetryDecision::SuccessWithCaveats => {
                    let summary = summary_opt
                        .as_ref()
                        .map(|s| s.summary.clone())
                        .unwrap_or_default();
                    let caveats = summary_opt
                        .as_ref()
                        .map(|s| s.out_of_scope_items.clone())
                        .unwrap_or_default();
                    if let Some(mut sess) = SessionStore::find(&session_id) {
                        sess.status = SessionStatus::Idle;
                        let _ = SessionStore::upsert(sess);
                    }
                    return Ok(SupervisorOutcome::SuccessWithCaveats {
                        summary,
                        caveats,
                        session_id,
                        attempts: attempt,
                    });
                }
                RetryDecision::Retry | RetryDecision::RetryQaOnly => {
                    current_task = build_targeted_retry_task(
                        &self.task,
                        attempt + 1,
                        self.max_attempts,
                        reason,
                        summary_opt.as_ref(),
                    );
                    continue;
                }
                RetryDecision::Blocked => {
                    break;
                }
            }
        }

        // Mark session blocked (best-effort; ignore errors).
        let _ = SessionStore::mark_blocked(&session_id);

        Ok(SupervisorOutcome::Blocked {
            reason: last_reason,
            attempts: self.max_attempts,
            session_id,
        })
    }
}

/// Spawn `open-mpm --workflow prescriptive ...` as a child and await its exit.
///
/// Why: Encapsulating the subprocess invocation keeps `run()` linear and lets
/// us inherit stderr (so the user sees workflow logs in real time) while
/// piping nothing to stdin.
/// What: Uses `std::env::current_exe()` to re-invoke the same binary, passes
/// `--workflow prescriptive`, the project dir, the task, and out-dir.
/// Test: Exercised by manual `om session run` runs; not unit-tested because
/// it shells out.
async fn spawn_workflow(
    project_dir: &Path,
    task: &str,
    out_dir: &Path,
) -> Result<std::process::ExitStatus> {
    let exe = std::env::current_exe().context("locating open-mpm executable")?;
    let status = Command::new(exe)
        .arg("--workflow")
        .arg("prescriptive")
        .arg("--project-dir")
        .arg(project_dir)
        .arg("--task")
        .arg(task)
        .arg("--out-dir")
        .arg(out_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("spawning prescriptive workflow subprocess")?;
    Ok(status)
}

/// Derive a short, human-readable session name from a task description.
///
/// Why: Falling back to `format!("session-{uuid8}")` works but produces
/// indistinguishable names in `om session list`. A truncated slug of the task
/// text helps users recognise their runs at a glance.
/// What: Lowercases, keeps alphanumerics + dashes, collapses whitespace, and
/// truncates to 32 chars. Empty input falls back to `supervised-<uuid8>`.
/// Test: Indirect — covered by the CLI smoke path.
fn derive_session_name(task: &str) -> String {
    let slug: String = task
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let collapsed: String = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let trimmed: String = collapsed.chars().take(32).collect();
    if trimmed.is_empty() {
        format!("supervised-{}", &Uuid::new_v4().to_string()[..8])
    } else {
        format!("run-{}", trimmed)
    }
}

/// Parse a `workflow-report.md` into a structured outcome + summary + next-steps.
///
/// Why: The workflow engine writes a free-form markdown report whose authoritative
/// outcome label appears either as `**Final Verdict**`, a `**Status**:` field, or
/// inline near the start (e.g. "classified as **partial**"). We need a single
/// tolerant parser that picks the strongest signal it can find.
/// What: Walks the report text, prefers explicit structured markers (`Final Verdict`
/// or `Status`), then falls back to scanning for `success` / `partial` / `fail` in
/// the body. Captures the `## Summary` body (or first non-heading paragraph as a
/// fallback) and the `## Next Steps` section verbatim if present.
/// Test: `test_parse_workflow_report_success`, `test_parse_workflow_report_partial`.
pub fn parse_workflow_report(content: &str) -> ReportSummary {
    let outcome = detect_outcome(content);
    let summary = extract_section(content, "Summary")
        .or_else(|| extract_section(content, "Final Verdict"))
        .unwrap_or_else(|| {
            content
                .lines()
                .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                .unwrap_or("")
                .trim()
                .to_string()
        });
    let next_steps = extract_section(content, "Next Steps").and_then(|s| {
        let trimmed = s.trim();
        // The example reports use "*(none)*" to mean no next steps; suppress it.
        if trimmed.is_empty() || trimmed == "*(none)*" || trimmed.eq_ignore_ascii_case("none") {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

    let completed_work = extract_section(content, "Completed Work")
        .or_else(|| extract_section(content, "Completed"))
        .map(|s| extract_bullets(&s))
        .unwrap_or_default();

    let bullets: Vec<String> = next_steps
        .as_deref()
        .map(extract_bullets)
        .unwrap_or_default();

    let mut in_scope = Vec::new();
    let mut out_of_scope = Vec::new();
    for b in bullets {
        if is_out_of_scope_step(&b) {
            out_of_scope.push(b);
        } else {
            in_scope.push(b);
        }
    }

    ReportSummary {
        outcome,
        summary,
        next_steps,
        completed_work,
        in_scope_next_steps: in_scope,
        out_of_scope_items: out_of_scope,
    }
}

/// Split a free-form markdown blob into bullet items (one per line).
///
/// Why: Both `## Next Steps` and `## Completed` are typically markdown bullet
/// lists. We need each item on its own to apply per-item heuristics. Lines that
/// don't look like bullets are accepted as a single item so we don't drop
/// content from prose-style sections.
/// What: For each line, strips leading `-`, `*`, `+`, or `1.` markers and
/// trims; empty lines are skipped. If no bullets are found, returns the trimmed
/// blob as a single item (when non-empty).
/// Test: Indirect via `test_parse_workflow_report_segregates_next_steps`.
fn extract_bullets(blob: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut had_any_bullet = false;
    for line in blob.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let stripped = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
            .map(|s| s.to_string());
        if let Some(s) = stripped {
            had_any_bullet = true;
            out.push(s);
            continue;
        }
        // Numbered list ("1. foo")
        if let Some(rest) = strip_numbered_prefix(trimmed) {
            had_any_bullet = true;
            out.push(rest);
            continue;
        }
        // Continuation / prose line — append to last bullet if there is one.
        if had_any_bullet {
            if let Some(last) = out.last_mut() {
                last.push(' ');
                last.push_str(trimmed);
            }
        } else {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// Strip a leading "1. " / "12. " marker from a line, returning the body if
/// matched.
fn strip_numbered_prefix(s: &str) -> Option<String> {
    let mut idx = 0;
    let bytes = s.as_bytes();
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx == 0 || idx >= bytes.len() {
        return None;
    }
    if bytes[idx] == b'.' && idx + 1 < bytes.len() && bytes[idx + 1] == b' ' {
        return Some(s[idx + 2..].to_string());
    }
    None
}

/// Heuristic: does a next-steps item describe pre-existing / unrelated work?
///
/// Why: When the QA phase reports failures we didn't introduce (e.g. an
/// existing i18n translation gap, a flaky test, work tracked under a different
/// ticket), the supervisor should not loop forever trying to fix them. We need
/// a cheap signal to flag those items as out of scope.
/// What: Lowercases the item and checks for any of a small set of phrases that
/// strongly indicate pre-existing / out-of-scope failures.
/// Test: Indirect via `test_parse_workflow_report_segregates_next_steps` and
/// `test_classify_qa_preexisting_failures`.
fn is_out_of_scope_step(item: &str) -> bool {
    let l = item.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "pre-existing",
        "preexisting",
        "pre existing",
        "unrelated",
        "out of scope",
        "out-of-scope",
        "i18n",
        "translation",
        "existing test",
        "existing failure",
        "existed before",
        "before this",
        "prior to this",
        "not introduced by",
        "not caused by this",
        "flaky",
    ];
    NEEDLES.iter().any(|n| l.contains(n))
}

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

/// Determine the workflow outcome label from a markdown report.
///
/// Why: Outcome detection is the load-bearing decision; isolating it makes the
/// fallback chain explicit and unit-testable.
/// What: 1) explicit `Final Verdict` section content; 2) `**Status**:` line;
/// 3) any `**success**` / `**partial**` / `**fail**` bold marker in the body;
/// 4) bare keywords. Defaults to `Fail` so the supervisor errs on the side of
/// retry / escalate when the report is unparseable.
/// Test: Covered by both report-parsing tests.
fn detect_outcome(content: &str) -> WorkflowOutcome {
    if let Some(verdict) = extract_section(content, "Final Verdict")
        && let Some(o) = match_outcome(&verdict)
    {
        return o;
    }
    for line in content.lines() {
        let lower = line.to_ascii_lowercase();
        if (lower.contains("**status**") || lower.starts_with("status:"))
            && let Some(o) = match_outcome(line)
        {
            return o;
        }
    }
    if let Some(o) = match_outcome(content) {
        return o;
    }
    WorkflowOutcome::Fail
}

/// Match a substring against the three outcome keywords with priority
/// success > partial > fail.
///
/// Why: Reports often mention multiple outcomes ("partial: code phase failed"),
/// so simple `.contains` ordering matters.
/// What: Lowercases once, then checks for whole-word-ish hits. Returns None if
/// no keyword is found.
/// Test: Indirect via `detect_outcome`.
fn match_outcome(s: &str) -> Option<WorkflowOutcome> {
    let l = s.to_ascii_lowercase();
    if l.contains("success") {
        Some(WorkflowOutcome::Success)
    } else if l.contains("partial") {
        Some(WorkflowOutcome::Partial)
    } else if l.contains("fail") {
        Some(WorkflowOutcome::Fail)
    } else {
        None
    }
}

/// Extract the body of a `## <heading>` section from a markdown document.
///
/// Why: Both `## Summary` and `## Next Steps` are sliced the same way; one
/// helper avoids duplicating the line-walk.
/// What: Case-insensitive heading match (allowing `#`, `##`, `###`). Returns
/// everything until the next heading at the same-or-shallower depth, trimmed.
/// Test: Indirect via the report-parsing tests.
fn extract_section(content: &str, heading: &str) -> Option<String> {
    let needle = heading.to_ascii_lowercase();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].trim_start();
        if l.starts_with('#') {
            let title = l.trim_start_matches('#').trim().to_ascii_lowercase();
            if title == needle {
                let mut out = Vec::new();
                let mut j = i + 1;
                while j < lines.len() {
                    let nl = lines[j].trim_start();
                    if nl.starts_with('#') {
                        break;
                    }
                    out.push(lines[j]);
                    j += 1;
                }
                return Some(out.join("\n").trim().to_string());
            }
        }
        i += 1;
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
