//! Post-phase-loop finalization for the workflow engine.
//!
//! Why: After the phase loop completes (or breaks on the first failure) the
//! engine still has to write `workflow-report.md`, stamp + flush the perf
//! record, run auto-push, fire the terminal ticket hook, emit the final
//! progress line, and propagate any captured error. Extracting that ~110-line
//! tail (#359) keeps the phase loop in `run` under the 500-line cap while
//! preserving exact ordering and the non-fatal-failure semantics each step
//! relies on.
//! What: `WorkflowEngine::finalize_run` consumes the accumulated run state and
//! returns the final `(WorkflowContext, PerfRecord)` (or the first phase error).
//! Test: Exercised end-to-end via the engine `tests` submodule and
//! `main::run_workflow`.

use std::path::PathBuf;
use std::time::Instant;

use tracing::info;

use crate::workflow::autopush;
use crate::workflow::config::WorkflowDef;
use crate::workflow::context::WorkflowContext;
use crate::workflow::error::WorkflowError;

use super::WorkflowEngine;

/// State accumulated by the phase loop and consumed once at finalization.
///
/// Why: The finalization tail reads a dozen run-scoped values; bundling them
/// into one struct keeps `finalize_run`'s signature readable and makes the
/// hand-off from the loop explicit.
/// What: Plain owned fields — no behavior. `first_error`/`failed_phase` drive
/// the success-vs-partial bookkeeping; the cost/summary/count fields feed the
/// terminal ticket hook.
/// Test: Indirectly via the engine `tests` submodule (every `engine.run(...)`
/// call routes through `finalize_run`).
pub(super) struct FinalizeState {
    pub ctx: WorkflowContext,
    pub perf: crate::perf::PerfCollector,
    pub out_dir: Option<PathBuf>,
    pub first_error: Option<WorkflowError>,
    pub failed_phase: Option<String>,
    pub workflow_started: Instant,
    pub total_cost_usd: f64,
    pub files_generated: usize,
    pub qa_summary: String,
}

impl WorkflowEngine {
    /// Finalize a workflow run: write the report, flush perf, auto-push, fire
    /// the terminal ticket hook, emit the final progress line, then return the
    /// context + perf record (or propagate the first phase error).
    ///
    /// Why: One place owns all the "after the loop" side effects so the phase
    /// loop stays focused on per-phase orchestration.
    /// What: Mirrors the pre-split tail exactly — report and auto-push run only
    /// on full success; perf is flushed regardless; ticket hooks branch on
    /// success vs failure; every side effect is non-fatal and must not mask the
    /// original phase error.
    /// Test: See the module-level doc.
    pub(super) async fn finalize_run(
        &self,
        def: &WorkflowDef,
        state: FinalizeState,
    ) -> Result<(WorkflowContext, crate::perf::PerfRecord), WorkflowError> {
        let FinalizeState {
            ctx,
            mut perf,
            out_dir,
            first_error,
            failed_phase,
            workflow_started,
            total_cost_usd,
            files_generated,
            qa_summary,
        } = state;

        // Only write the workflow report when all phases succeeded — a partial
        // run's `observe` section would reflect an incomplete workflow.
        if first_error.is_none()
            && let Some(dir) = &out_dir
            && let Some(report) = ctx.phase_outputs.get("observe").cloned()
        {
            let target = dir.join("workflow-report.md");
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(WorkflowError::Io)?;
            }
            if let Err(e) = tokio::fs::write(&target, report).await {
                tracing::warn!(path = %target.display(), error = %e, "workflow report write failed (non-fatal)");
            } else {
                info!(path = %target.display(), "wrote workflow report");
            }
        }

        // #56: Record run status before flushing so partial/failed runs land on
        // disk with the correct outcome.
        if first_error.is_some() {
            perf.set_status("partial");
            if let Some(name) = &failed_phase {
                perf.set_failed_phase(name);
            }
        } else {
            perf.set_status("success");
        }

        // #47 + #56: Flush perf record regardless of success/failure so we
        // always capture latency+token usage for every run. Non-fatal: a perf
        // write failure must not override the original phase error.
        if let Some(perf_dir) = &self.perf_dir
            && let Err(e) = perf.flush(perf_dir).await
        {
            tracing::warn!(error = %e, "failed to flush perf record");
        }
        // #151: snapshot the perf record for in-process consumers (the
        // JSON envelope builder). Cheap clone; `build_record` already
        // duplicates the phase vec.
        let perf_record = perf.build_record();

        // #76: Auto-push on successful workflow completion. Gated on
        // `auto_push.enabled` in the workflow config; a missing config is
        // equivalent to disabled. Failures are non-fatal — the workflow itself
        // already succeeded; we just log.
        if first_error.is_none()
            && let Some(auto_push_cfg) = &def.auto_push
            && auto_push_cfg.enabled
        {
            let push_dir = out_dir
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let task_preview = ctx.task.lines().next().unwrap_or(&ctx.task).to_string();
            if let Err(e) = autopush::run_auto_push(
                auto_push_cfg,
                &push_dir,
                &def.name,
                self.build,
                &task_preview,
            )
            .await
            {
                tracing::warn!(error = %e, "auto-push failed (non-fatal)");
            }
        }

        // #84: Fire the final ticket hook — success closes the issue, failure
        // leaves it open with a comment. Both paths log and continue on error.
        if let Some(tm_cell) = &self.ticket_manager {
            let tm = tm_cell.lock().await;
            let total_duration_ms = workflow_started.elapsed().as_millis() as u64;
            if let Some(err) = &first_error {
                let failed = failed_phase.clone().unwrap_or_else(|| "unknown".into());
                if let Err(e) = tm
                    .on_workflow_failure(&failed, &format!("{err}"), self.perf_dir.as_deref())
                    .await
                {
                    tracing::warn!(error = %e, "ticket manager: on_workflow_failure failed");
                }
            } else if let Err(e) = tm
                .on_workflow_success(
                    total_cost_usd,
                    total_duration_ms,
                    &qa_summary,
                    files_generated,
                )
                .await
            {
                tracing::warn!(error = %e, "ticket manager: on_workflow_success failed");
            }
        }

        // #149: Final workflow-complete summary line (only on full success).
        if first_error.is_none()
            && let Some(rep) = &self.progress
        {
            let total_elapsed = workflow_started.elapsed();
            rep.workflow_done(total_elapsed, total_cost_usd as f32);
        }

        if let Some(err) = first_error {
            return Err(err);
        }

        Ok((ctx, perf_record))
    }
}
