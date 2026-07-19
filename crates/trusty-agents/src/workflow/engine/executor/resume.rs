//! `tagent resume <run-id>` engine entry point (#3062, SPEC-AGENTFW-02 §3.4).
//!
//! Why: A crashed/killed run leaves a `checkpoint.json` behind (written by
//! `phase_loop::run_phase_loop`). This module reconstructs a
//! `WorkflowContext` from that journal — reusing every already-completed
//! phase's output verbatim, never re-dispatching it — and continues the
//! SAME shared phase loop a fresh run uses, starting at the first incomplete
//! phase.
//! What: `WorkflowEngine::resume_with_perf_and_dirs` loads + validates the
//! checkpoint (fails closed per §3.5's failure matrix: missing/corrupt/
//! schema-mismatched journal, or a workflow definition that no longer
//! matches the checkpointed phase list), reconstructs context, and delegates
//! to `run_phase_loop`. `ResumeOutcome` distinguishes "already finished,
//! nothing to resume" from "picked back up and ran".
//! Test: `checkpoint_resume` in the engine `tests` submodule.

use std::path::PathBuf;

use crate::perf::PerfCollector;
use crate::workflow::config::WorkflowDef;
use crate::workflow::context::WorkflowContext;
use crate::workflow::engine::checkpoint::{self, RunState};
use crate::workflow::error::WorkflowError;

use super::WorkflowEngine;
use super::phase_loop::PhaseLoopSeed;
use super::setup::detect_persona;

/// Outcome of `tagent resume <run-id>` (SPEC-AGENTFW-02 §3.4 requirement:
/// "If the run finished, say so.").
///
/// Why: A checkpoint directory should never contain a `Done` record in
/// practice (`finalize_run` deletes it on success) but a resume MUST still
/// handle finding one gracefully — e.g. a prior checkpoint-delete failure —
/// rather than re-running a completed workflow.
/// What: `AlreadyDone` carries the run id back for the CLI's message;
/// `Resumed` carries the same `(WorkflowContext, PerfRecord)` shape `run()`
/// returns, plus the phase name resume actually restarted at (for the CLI's
/// "resumed at phase '…'" line).
/// Test: `resume_reports_already_done_for_done_checkpoint`.
#[derive(Debug)]
pub enum ResumeOutcome {
    AlreadyDone {
        run_id: String,
    },
    /// Boxed: `WorkflowContext` + `PerfRecord` together dwarf the
    /// `AlreadyDone` variant, which would otherwise force every
    /// `ResumeOutcome` (including the common already-done case) to reserve
    /// stack space for the larger variant.
    Resumed {
        ctx: Box<WorkflowContext>,
        perf: Box<crate::perf::PerfRecord>,
        resumed_at_phase: String,
    },
}

impl WorkflowEngine {
    /// Resume a checkpointed run: reconstruct `WorkflowContext` from the
    /// journal and continue `run_phase_loop` from the first incomplete
    /// phase.
    ///
    /// Why: Single entry point so `tagent resume <run-id>` only needs one
    /// call, mirroring how `run_with_perf_and_dirs` is the single entry
    /// point for a fresh run.
    /// What: Loads the checkpoint (fail-closed on missing/corrupt/schema
    /// mismatch — see `checkpoint::load_checkpoint`), reloads the workflow
    /// definition fresh and validates its phase list still matches the
    /// checkpoint's snapshot (fail-closed on drift —
    /// `WorkflowError::ResumeDefinitionChanged`), replays every
    /// already-`PhaseComplete` phase's output into a fresh `WorkflowContext`
    /// without re-dispatching it, re-validates `out_dir`/`code_dir` exactly
    /// as a fresh run does, and hands off to the shared `run_phase_loop`
    /// starting at the first incomplete phase index.
    /// Test: `checkpoint_resume_tests::resume_skips_completed_phases_and_reuses_their_output`,
    /// `checkpoint_resume_tests::resume_fails_closed_on_definition_change`.
    pub async fn resume_with_perf_and_dirs(
        &self,
        run_id: &str,
    ) -> Result<ResumeOutcome, WorkflowError> {
        let project_dir = crate::usage::project_dir();

        // Code-critic finding (PR #3244): hold an exclusive advisory lock for
        // the ENTIRE resume, not just the checkpoint writes inside it — two
        // concurrent `tagent resume` calls for the same run_id must not both
        // dispatch phases. Fails closed immediately (non-blocking) rather
        // than queuing. Held until this function returns (drop releases it).
        let _resume_lock = checkpoint::acquire_resume_lock(&project_dir, run_id)?;

        let record = checkpoint::load_checkpoint(&project_dir, run_id)?;

        if matches!(record.state, RunState::Done) {
            return Ok(ResumeOutcome::AlreadyDone {
                run_id: run_id.to_string(),
            });
        }

        // §3.4: reload the workflow definition FRESH — never trust the
        // checkpoint's `phase_names` snapshot directly, only validate against it.
        let path = if record.workflow.ends_with(".json") || record.workflow.contains('/') {
            PathBuf::from(&record.workflow)
        } else {
            self.config_dir.join(format!("{}.json", record.workflow))
        };
        if !path.exists() {
            return Err(WorkflowError::WorkflowNotFound {
                path: path.display().to_string(),
            });
        }
        let def =
            WorkflowDef::load(&path).map_err(|e| WorkflowError::ConfigInvalid(format!("{e:#}")))?;

        let current_phase_names: Vec<String> = def.phases.iter().map(|p| p.name.clone()).collect();
        if current_phase_names != record.phase_names {
            return Err(WorkflowError::ResumeDefinitionChanged {
                run_id: run_id.to_string(),
                workflow: record.workflow.clone(),
                checkpoint_phases: record.phase_names.clone(),
                current_phases: current_phase_names,
            });
        }

        // §3.4: resume immediately AFTER phase_index for PhaseComplete, or AT
        // phase_index for Failed/Retrying/PhaseRunning (re-run from scratch —
        // no sub-phase resume).
        let resume_index = match record.state {
            RunState::Pending => 0,
            RunState::PhaseRunning { phase_index }
            | RunState::Failed { phase_index }
            | RunState::Retrying { phase_index, .. } => phase_index,
            RunState::PhaseComplete { phase_index } => phase_index + 1,
            RunState::Done => unreachable!("handled above"),
        };
        let resumed_at_phase = def
            .phases
            .get(resume_index)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "(finalize)".to_string());

        crate::events::emit(crate::events::Event::RunResumed {
            session_id: run_id.to_string(),
            resumed_at_phase: resumed_at_phase.clone(),
        });
        tracing::info!(
            run_id = %run_id,
            workflow = %record.workflow,
            resumed_at_phase = %resumed_at_phase,
            resume_index,
            "resuming checkpointed workflow run"
        );

        // Persona detection runs on the ORIGINAL raw task text, exactly as a
        // fresh run's `detect_persona(&task)` call does.
        let (persona, cleaned_task) = detect_persona(&record.task);

        let mut ctx = WorkflowContext::builder(cleaned_task)
            .with_out_dir(Some(record.out_dir.clone()))
            .build();
        ctx.goal_block = record.goal_block.clone();
        // Replay every already-complete phase's output verbatim — NOT
        // re-dispatched. Code-critic finding (PR #3244): pass the PERSISTED
        // SUMMARY (not `None`) so `ctx.phase_summaries` — the map
        // `render_template` actually injects into downstream
        // `{{phase_name}}` substitutions — carries the original ~500-word
        // digest, not the full raw content. Falling back to `None` (which
        // defaults the summary to the full content) would silently re-inflate
        // every downstream prompt for exactly the long-running,
        // content-heavy workflows checkpointing exists to protect.
        for phase_def in &def.phases[..resume_index.min(def.phases.len())] {
            if let Some(content) = record.phase_outputs.get(&phase_def.name) {
                let summary = record.phase_summaries.get(&phase_def.name).cloned();
                ctx.record_phase(&phase_def.name, content.clone(), summary);
            }
        }

        // §3.5 failure matrix: re-validate/re-canonicalize out_dir/code_dir
        // exactly as a fresh run does. A missing out_dir is recreated; a
        // missing code_dir with partial generated files is a known MVP
        // limitation, not solved here.
        let (out_dir, code_dir) = self
            .resolve_dirs(Some(record.out_dir.clone()), Some(record.code_dir.clone()))
            .await?;

        let discovered_skills = self.discover_skills_for_task(&ctx.task, 8);
        let mut perf = PerfCollector::new(self.build, &def.name, &record.task);
        for skill in &discovered_skills {
            perf.record_skill_considered(&skill.name);
        }
        self.maybe_pre_index_ast(&def, &code_dir);

        // Deviations from a fresh run, documented in the PR body: the
        // ticket-manager `on_workflow_start` hook does not re-fire (the
        // tracking issue already exists), `workflow_started` restarts the
        // wall clock for the resumed portion only, and per-phase cost/file
        // counters start at zero (perf for the pre-crash portion was never
        // flushed anyway, since `perf.flush` only runs once at the end).
        let seed = PhaseLoopSeed {
            start_index: resume_index,
            run_id: run_id.to_string(),
            raw_task: record.task.clone(),
            started_at: record.started_at.clone(),
            workflow_started: std::time::Instant::now(),
            qa_retry_count: record.qa_retry_count,
            qa_failure_feedback: record.qa_failure_feedback.clone(),
            total_cost_usd: 0.0,
            files_generated: 0,
            qa_summary: "n/a".to_string(),
            code_phase_used_claude_code: false,
        };

        let (ctx, perf_record) = self
            .run_phase_loop(
                &def,
                ctx,
                perf,
                out_dir,
                code_dir,
                persona,
                &discovered_skills,
                seed,
            )
            .await?;

        Ok(ResumeOutcome::Resumed {
            ctx: Box::new(ctx),
            perf: Box::new(perf_record),
            resumed_at_phase,
        })
    }
}
