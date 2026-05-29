//! Per-phase agent dispatch for the workflow engine.
//!
//! Why: A single phase can run four different ways — the per-file wave loop,
//! concurrent parallel sub-agents (merged via the conflict resolver), a
//! persistent-session agent that threads prior history, or a plain
//! single-shot `run_with_context` call. Isolating that ~180-line branch (#359)
//! keeps the phase loop in `run` under the 500-line cap while preserving the
//! exact `working_dir` / model wiring each branch relies on.
//! What: `WorkflowEngine::dispatch_phase` selects the branch and returns the
//! resulting `AgentOutput` (or a `WorkflowError::PhaseFailed`).
//! Test: Covered end-to-end via the engine `tests` submodule
//! (`wave_loop_*`, `legacy_monolithic_path_passes_absolute_working_dir`, etc.).

use std::path::PathBuf;

use crate::tools::traits::{AgentOutput, RunContext};
use crate::workflow::config::{Assignments, PhaseDef};
use crate::workflow::error::WorkflowError;
use crate::workflow::parallel::run_parallel_phase;
use crate::workflow::resolver::ConflictResolver;

use super::super::step_dispatch::run_wave_loop;
use super::WorkflowEngine;

impl WorkflowEngine {
    /// Dispatch a single phase to the agent runner via the appropriate path.
    ///
    /// Why: Centralizes the wave-loop / parallel / persistent / single-shot
    /// branch selection so the phase loop only needs one call.
    /// What: Runs the wave loop when `wave_assignments` is `Some`; else runs
    /// parallel sub-agents when the phase declares them; else threads session
    /// history when the agent is persistent; else makes a plain
    /// `run_with_context` call. All paths thread `working_dir = code_dir`
    /// (falling back to `out_dir`) and the per-phase model override.
    /// Test: See the module-level doc.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn dispatch_phase(
        &self,
        phase: &PhaseDef,
        rendered: &str,
        out_dir: &Option<PathBuf>,
        code_dir: &Option<PathBuf>,
        wave_assignments: Option<Assignments>,
        persistent: bool,
    ) -> Result<AgentOutput, WorkflowError> {
        if let Some(asg) = wave_assignments {
            let artifacts_dir = out_dir
                .as_deref()
                .expect("assignments.json only loaded when out_dir is Some");
            // #222: Wave loop writes code to `code_dir` (== out_dir in legacy
            // mode). It also reads stubs from `out_dir/stubs/`.
            let code_target = code_dir.as_deref().unwrap_or(artifacts_dir);
            return run_wave_loop(
                asg,
                phase,
                self.agent_runner.clone(),
                code_target,
                artifacts_dir,
                self.progress.clone(),
            )
            .await
            .map_err(|e| WorkflowError::PhaseFailed {
                phase: phase.name.clone(),
                source: e,
            });
        }

        if let Some(subs) = phase.parallel_subtasks.as_ref().filter(|s| !s.is_empty()) {
            let use_worktrees = phase.worktree_protection.unwrap_or(false);
            // Parallel sub-agents write into per-label subdirs under the
            // out_dir (or a temp dir if no out_dir is set).
            let phase_out_dir = out_dir
                .clone()
                .unwrap_or_else(|| std::env::temp_dir().join("open-mpm-parallel"));
            return match run_parallel_phase(
                rendered,
                subs,
                &phase.agent,
                self.agent_runner.clone(),
                &phase_out_dir,
                use_worktrees,
            )
            .await
            {
                Ok(results) if !results.is_empty() => {
                    // Merge file trees into the phase_out_dir via ConflictResolver.
                    let api_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
                    let resolver = ConflictResolver::new(api_key);
                    let merge_report = resolver
                        .merge(&results, &phase_out_dir)
                        .await
                        .unwrap_or_else(|e| format!("merge failed: {e:#}"));

                    // Aggregate combined content + usage across sub-agents.
                    let mut combined = String::new();
                    let mut agg_usage = crate::perf::TokenUsage::default();
                    for r in &results {
                        combined.push_str(&format!("## Sub-agent [{}]\n\n", r.label));
                        combined.push_str(&r.output.content);
                        combined.push_str("\n\n");
                        agg_usage.add(&r.output.usage);
                    }
                    combined.push_str("\n---\n\n");
                    combined.push_str(&merge_report);

                    Ok(AgentOutput {
                        content: combined,
                        summary: None,
                        usage: agg_usage,
                    })
                }
                Ok(_) => Err(WorkflowError::PhaseFailed {
                    phase: phase.name.clone(),
                    source: anyhow::anyhow!(
                        "parallel phase '{}' produced zero successful sub-agent results",
                        phase.name
                    ),
                }),
                Err(e) => Err(WorkflowError::PhaseFailed {
                    phase: phase.name.clone(),
                    source: e,
                }),
            };
        }

        // #122/#410: All phases run with `working_dir = code_dir` (the user's
        // project source tree), falling back to `out_dir` when unset. Artifacts
        // the harness writes use absolute out_dir paths and are unaffected by
        // the agent CWD.
        let working_dir = code_dir.clone().or_else(|| out_dir.clone());
        let ctx_run = RunContext {
            working_dir,
            model: phase.model.clone(),
            ..RunContext::default()
        };

        if persistent {
            let history = self.sessions.get_history_wire(&phase.agent).await;
            return match self
                .agent_runner
                .run_with_history(&phase.agent, rendered, &history, &ctx_run)
                .await
            {
                Ok(out) => {
                    // Record exchange so the next call to this agent sees it.
                    if let Err(e) = self
                        .sessions
                        .extend_history(&phase.agent, rendered, &out.content)
                        .await
                    {
                        tracing::warn!(agent = %phase.agent, error = %e, "failed to extend session history; continuing");
                    }
                    Ok(out)
                }
                Err(e) => Err(WorkflowError::PhaseFailed {
                    phase: phase.name.clone(),
                    source: e,
                }),
            };
        }

        // MAJ-1 (#93): Route non-wave calls through `run_with_context` so
        // subprocess-driven runners write somewhere predictable.
        self.agent_runner
            .run_with_context(&phase.agent, rendered, &ctx_run)
            .await
            .map_err(|e| WorkflowError::PhaseFailed {
                phase: phase.name.clone(),
                source: e,
            })
    }
}
