//! Post-dispatch per-phase side effects for the workflow engine.
//!
//! Why: After an agent runs, the engine still has to interpret a structured QA
//! envelope (test counts + one-shot retry gating), relocate misrouted
//! claude-code outputs, and extract `## File:` sections to disk before the next
//! phase runs. Pulling those three blocks out of the phase loop (#359) keeps
//! `run` under the 500-line cap while preserving the exact gating each relies on.
//! What: `handle_qa_envelope` mutates the run-scoped QA gating state;
//! `reconcile_code_phase` relocates misrouted files and reports whether the code
//! phase used claude-code; `extract_phase_files` writes produced files and
//! returns the count.
//! Test: Covered end-to-end via the engine `tests` submodule
//! (`files_are_extracted_before_next_phase_runs`,
//! `post_code_reconciles_files_from_project_root`, the QA-path tests).

use std::path::PathBuf;
use std::time::Instant;

use tracing::info;

use crate::agents::AgentConfig;
use crate::ipc::extract_files_from_content;
use crate::workflow::config::PhaseDef;
use crate::workflow::context::WorkflowContext;
use crate::workflow::error::WorkflowError;

use super::super::helpers::{agent_uses_claude_code, reconcile_code_outputs_against};
use super::super::qa::{QaStatus, parse_qa_envelope};
use super::super::state::emit_progress_event;
use super::WorkflowEngine;

impl WorkflowEngine {
    /// Record a phase dispatch failure: perf entry, failed-phase ticket
    /// comment, and progress event (#56/#84/#149).
    ///
    /// Why: When a phase errors the loop breaks, but we must still capture its
    /// duration/model in perf, post a ❌ ticket comment, and stream the failure
    /// before propagating — otherwise failed runs lose all telemetry.
    /// What: Resolves the phase model, records a zero-usage perf entry for the
    /// elapsed time, fires the (non-fatal) failure ticket hook, emits the
    /// failure progress event, and returns the error for the caller to store.
    /// Test: `wave_loop_does_not_retry_fatal_error` drives this path.
    pub(super) async fn record_phase_failure(
        &self,
        phase: &PhaseDef,
        e: WorkflowError,
        phase_started: Instant,
        perf: &mut crate::perf::PerfCollector,
    ) -> WorkflowError {
        // #56: Record the duration of the failed phase and capture the error so
        // we can flush perf before propagating.
        let duration_ms = phase_started.elapsed().as_millis() as u64;
        let phase_model = phase
            .model
            .clone()
            .or_else(|| {
                AgentConfig::by_name(&phase.agent)
                    .ok()
                    .map(|c| c.agent.model)
            })
            .unwrap_or_else(|| "unknown".to_string());
        perf.record_phase(
            &phase.name,
            duration_ms,
            &phase_model,
            &crate::perf::TokenUsage::default(),
        );

        // #84: Post the failed-phase comment too (marked with ❌).
        if let Some(tm_cell) = &self.ticket_manager {
            let tm = tm_cell.lock().await;
            if let Err(err) = tm
                .on_phase_complete(
                    &phase.name,
                    &phase_model,
                    duration_ms,
                    0,
                    0,
                    0.0,
                    "❌ failed",
                )
                .await
            {
                tracing::warn!(error = %err, phase = %phase.name,
                    "ticket manager: on_phase_complete (failure) failed");
            }
        }

        // #149: Stream the failure to stderr + machine progress event.
        let elapsed = std::time::Duration::from_millis(duration_ms);
        let err_msg = format!("{e}");
        if let Some(rep) = &self.progress {
            rep.phase_failed(&phase.name, elapsed, &err_msg);
        }
        emit_progress_event(
            &phase.name,
            "failed",
            elapsed.as_secs_f32(),
            0.0,
            Some(&err_msg),
        );

        e
    }

    /// Interpret a QA phase's output: record test counts and apply the
    /// one-shot retry gating (#84, claude-mpm parity Fix 2).
    ///
    /// Why: Structured QA envelopes drive perf test-count metrics and the
    /// single engineer retry on failure; free-text QA output flows through the
    /// legacy first-line summary unchanged.
    /// What: Sets `qa_summary` to the first non-empty content line, records
    /// pass/fail counts on `perf`, and on `Fail` marks `failed_phase` (once) and
    /// queues `qa_failure_feedback` for the next code phase, capping retries at
    /// exactly one. No-op for non-`qa` phases or free-text output.
    /// Test: `qa_receives_correct_path_for_claude_code_runner` and the engine
    /// integration suite.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_qa_envelope(
        &self,
        phase: &PhaseDef,
        content: &str,
        perf: &mut crate::perf::PerfCollector,
        qa_summary: &mut String,
        failed_phase: &mut Option<String>,
        qa_retry_count: &mut u32,
        qa_failure_feedback: &mut Option<String>,
    ) {
        if phase.name != "qa" {
            return;
        }
        *qa_summary = content
            .lines()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.chars().take(80).collect::<String>())
            .unwrap_or_else(|| "completed".to_string());

        let Some(env) = parse_qa_envelope(content) else {
            return;
        };
        if let (Some(p), Some(f)) = (env.passed, env.failed) {
            perf.set_test_counts(p, f);
        } else if let Some(p) = env.passed {
            perf.set_test_counts(p, 0);
        } else if let Some(f) = env.failed {
            perf.set_test_counts(0, f);
        }

        match env.status {
            QaStatus::Pass => {
                // Clear any stale feedback from a prior failure that the
                // engineer subsequently fixed.
                *qa_failure_feedback = None;
            }
            QaStatus::Fail => {
                // Mark the run as having failed at QA, but do not break the
                // loop — observe / docs may still run.
                if failed_phase.is_none() {
                    *failed_phase = Some("qa".to_string());
                }
                if *qa_retry_count < 1 {
                    *qa_retry_count += 1;
                    let detail = env.details.clone().unwrap_or_else(|| {
                        format!("QA reported {} failed test(s).", env.failed.unwrap_or(0))
                    });
                    *qa_failure_feedback = Some(format!(
                        "[QA FEEDBACK] Tests failed in previous run. Details:\n\
                         {detail}\n\
                         Please fix these failures before proceeding.\n\n"
                    ));
                    tracing::warn!(
                        retry = *qa_retry_count,
                        "QA reported status=fail; queued feedback for next code phase"
                    );
                } else {
                    tracing::warn!("QA reported status=fail again after retry cap; advancing");
                    *qa_failure_feedback = None;
                }
            }
        }
    }

    /// After the code phase under a claude-code runner, relocate any files
    /// written to the git root into `code_dir` (#123/#222).
    ///
    /// Why: The claude CLI anchors relative writes at the git root rather than
    /// `RunContext::working_dir`, so declared files can land outside `code_dir`.
    /// What: Returns `true` when this was a code phase backed by claude-code (so
    /// the caller can later hint the QA prompt), and best-effort reconciles
    /// misrouted files using `out_dir` as the assignments source of truth.
    /// Test: `post_code_reconciles_files_from_project_root`,
    /// `reconcile_code_outputs_against_divergent_dirs`.
    pub(super) async fn reconcile_code_phase(
        &self,
        phase: &PhaseDef,
        out_dir: &Option<PathBuf>,
        code_dir: &Option<PathBuf>,
    ) -> bool {
        if !(phase.name == "code" && agent_uses_claude_code(&phase.agent)) {
            return false;
        }
        let target = code_dir.as_deref().or(out_dir.as_deref());
        let assignments_dir = out_dir.as_deref().or(target);
        if let (Some(target), Some(asg_dir)) = (target, assignments_dir)
            && let Err(e) = reconcile_code_outputs_against(asg_dir, target).await
        {
            tracing::warn!(
                error = %e,
                "post-code reconciliation check failed; continuing"
            );
        }
        true
    }

    /// Extract `## File:` sections from a `produces_files` phase to disk before
    /// the next phase runs (#64/#222).
    ///
    /// Why: QA must run against files the code phase just produced; extraction
    /// has to happen between phases, not after the whole workflow.
    /// What: For the `code` phase files go to `code_dir`, else to `out_dir`.
    /// Writes each extracted file (creating parents) and returns the count.
    /// No-op (returns 0) when the phase doesn't produce files or has no target.
    /// Test: `files_are_extracted_before_next_phase_runs`,
    /// `phase_without_produces_files_does_not_extract`.
    pub(super) async fn extract_phase_files(
        &self,
        phase: &PhaseDef,
        ctx: &WorkflowContext,
        out_dir: &Option<PathBuf>,
        code_dir: &Option<PathBuf>,
    ) -> Result<usize, WorkflowError> {
        let extract_target = if phase.name == "code" {
            code_dir.as_deref().or(out_dir.as_deref())
        } else {
            out_dir.as_deref()
        };
        if !phase.produces_files.unwrap_or(false) {
            return Ok(0);
        }
        let (Some(dir), Some(phase_content)) = (extract_target, ctx.phase_outputs.get(&phase.name))
        else {
            return Ok(0);
        };

        let files = extract_files_from_content(phase_content);
        let file_count = files.len();
        for (rel, body) in &files {
            let dest = dir.join(rel);
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(WorkflowError::Io)?;
            }
            tokio::fs::write(&dest, body.as_bytes())
                .await
                .map_err(WorkflowError::Io)?;
            info!(
                file = %dest.display(),
                phase = %phase.name,
                "extracted file from phase output"
            );
        }
        info!(
            count = file_count,
            phase = %phase.name,
            out_dir = %dir.display(),
            "extracted phase files to out_dir before next phase"
        );
        Ok(file_count)
    }
}
