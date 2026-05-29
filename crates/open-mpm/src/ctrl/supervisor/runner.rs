//! Supervised `om session run` loop + subprocess spawn.
//!
//! Why: `om session new` is fire-and-forget; the supervisor closes the gap by
//! driving the prescriptive workflow to completion with retry/escalation. This
//! file owns the stateful loop and the side-effecting subprocess spawn, kept
//! apart from the pure parsing/policy helpers so those stay unit-testable.
//! What: `CtrlSupervisor` (config + `run()` loop), `spawn_workflow`, and the
//! `derive_session_name` slug helper.
//! Test: End-to-end via `om session run`; the deterministic helpers it calls
//! live in `report.rs` / `policy.rs` and are unit-tested there.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::process::Command;
use uuid::Uuid;

use crate::ctrl_session::{Session, SessionStatus, SessionStore};

use super::policy::{build_targeted_retry_task, classify_partial_reason, should_retry};
use super::report::parse_workflow_report;
use super::types::{RetryDecision, SupervisorOutcome, WorkflowOutcome};

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
