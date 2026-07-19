//! The workflow engine's fresh-run entry point (`run_with_perf_and_dirs`).
//!
//! Why: This is the deterministic Research -> Plan -> Code -> QA -> Observe
//! driver's fresh-start path: resolve the workflow definition, fire the
//! ticket-manager start hook, build the initial (empty) `WorkflowContext` +
//! `PerfCollector`, detect persona, then delegate to the shared
//! `phase_loop::run_phase_loop` — the SAME driver `tagent resume` uses via
//! `resume::resume_with_perf_and_dirs` — with `start_index: 0`. The loop
//! body itself (dispatch, QA gating, file extraction, checkpoint writes)
//! moved to `phase_loop` in #3062 specifically so resume never diverges from
//! fresh-run dispatch behavior.
//! What: `WorkflowEngine::run_with_perf_and_dirs` — the single end-to-end
//! entry point all the public `run*` wrappers funnel into.
//! Test: Covered end-to-end by the engine `tests` submodule and
//! `main::run_workflow`.

use std::path::PathBuf;
use std::time::Instant;

use tracing::info;

use crate::perf::PerfCollector;
use crate::workflow::config::WorkflowDef;
use crate::workflow::context::WorkflowContext;
use crate::workflow::error::WorkflowError;

use super::WorkflowEngine;
use super::phase_loop::PhaseLoopSeed;
use super::setup::detect_persona;

impl WorkflowEngine {
    /// Same as `run_with_perf` but accepts a separate `code_dir` for generated
    /// source files (#222). When `code_dir` is `None`, falls back to using
    /// `out_dir` for code as well — preserving pre-#222 behavior exactly.
    pub async fn run_with_perf_and_dirs(
        &self,
        name: &str,
        task: String,
        out_dir: Option<PathBuf>,
        code_dir: Option<PathBuf>,
    ) -> Result<(WorkflowContext, crate::perf::PerfRecord), WorkflowError> {
        // #54: Accept either a bare workflow name (joined to `config_dir`) or a
        // literal path (anything ending in `.json` or containing a path
        // separator). Without this, `--workflow config/workflows/foo.json`
        // double-joins to `config/workflows/config/workflows/foo.json`.
        let path = if name.ends_with(".json") || name.contains('/') {
            PathBuf::from(name)
        } else {
            self.config_dir.join(format!("{name}.json"))
        };
        if !path.exists() {
            return Err(WorkflowError::WorkflowNotFound {
                path: path.display().to_string(),
            });
        }
        let def =
            WorkflowDef::load(&path).map_err(|e| WorkflowError::ConfigInvalid(format!("{e:#}")))?;

        if def.phases.is_empty() {
            return Err(WorkflowError::ConfigInvalid(
                "workflow has no phases".to_string(),
            ));
        }

        info!(workflow = %def.name, phases = def.phases.len(), "starting workflow");

        // #84: If a ticket manager is attached, create the tracking issue
        // before any phase runs. Failures are logged and non-fatal — the
        // workflow must not die because GitHub is unreachable.
        let workflow_started = Instant::now();
        let task_preview_full = task.clone();
        if let Some(tm_cell) = &self.ticket_manager {
            let mut tm = tm_cell.lock().await;
            if tm.enabled() {
                if let Err(e) = tm
                    .on_workflow_start(&def.name, self.build, &task_preview_full)
                    .await
                {
                    tracing::warn!(error = %e, "ticket manager: on_workflow_start failed");
                }
                // #84: Best-effort related-issue search using the first line
                // of the task as keywords. Drop silently on failure.
                let keywords = task_preview_full
                    .lines()
                    .next()
                    .unwrap_or(&task_preview_full)
                    .chars()
                    .take(80)
                    .collect::<String>();
                if let Err(e) = tm.auto_relate(&keywords).await {
                    tracing::warn!(error = %e, "ticket manager: auto_relate failed");
                }
            }
        }

        // #47: Start perf collection for the whole run. Each phase's wall
        // clock + aggregated TokenUsage + resolved agent model get pushed
        // into the collector and flushed to disk at the end.
        let mut perf = PerfCollector::new(self.build, &def.name, &task);

        // #126/#153/#222: Pre-create + canonicalize out_dir and code_dir (the
        // latter falling back to out_dir). See `setup::resolve_dirs`.
        let (out_dir, code_dir) = self.resolve_dirs(out_dir, code_dir).await?;

        // #196/#205: Detect the active persona from the RAW task text before
        // it's cleaned and handed to the context. See `setup::detect_persona`.
        let (persona, cleaned_task) = detect_persona(&task);

        let ctx = WorkflowContext::builder(cleaned_task)
            .with_out_dir(out_dir.clone())
            .build();

        // #173: Run pre-plan skill discovery once for the whole workflow so
        // every skill the engine considered is recorded in the perf record
        // (`skills_considered`) regardless of which phase eventually consumed
        // them.
        let discovered_skills = self.discover_skills_for_task(&ctx.task, 8);
        for skill in &discovered_skills {
            perf.record_skill_considered(&skill.name);
        }

        // #347 follow-up: Pre-index existing source under `code_dir` before any
        // AST-native phase runs so the AST tool surface starts warm. See
        // `setup::maybe_pre_index_ast`.
        self.maybe_pre_index_ast(&def, &code_dir);

        // #3062: `TAGENT_RUN_ID`/`OPEN_MPM_RUN_ID` is the existing run-id
        // convention (already threaded through `emit_progress_event` and
        // `HistoryIndexer`) — reused verbatim as the checkpoint journal's
        // `run_id`, no new ID scheme.
        let run_id = crate::env_compat::env_var("TAGENT_RUN_ID", "OPEN_MPM_RUN_ID")
            .unwrap_or_else(|_| "unknown".to_string());
        let started_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let seed = PhaseLoopSeed {
            start_index: 0,
            run_id,
            raw_task: task,
            started_at,
            workflow_started,
            qa_retry_count: 0,
            qa_failure_feedback: None,
            total_cost_usd: 0.0,
            files_generated: 0,
            qa_summary: "n/a".to_string(),
            code_phase_used_claude_code: false,
        };

        self.run_phase_loop(
            &def,
            ctx,
            perf,
            out_dir,
            code_dir,
            persona,
            &discovered_skills,
            seed,
        )
        .await
    }
}
