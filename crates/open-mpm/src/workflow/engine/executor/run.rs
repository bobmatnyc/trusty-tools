//! The workflow engine's phase-loop driver (`run_with_perf_and_dirs`).
//!
//! Why: This is the deterministic Research -> Plan -> Code -> QA -> Observe
//! driver. It resolves output/code dirs, detects the persona, iterates phases
//! (skipping, prompt assembly, dispatch, perf, QA gating, file extraction), and
//! hands off to `finalize_run`. Split from the builder surface (#359) to keep
//! each file under the 500-line cap.
//! What: `WorkflowEngine::run_with_perf_and_dirs` — the single end-to-end entry
//! point all the public `run*` wrappers funnel into.
//! Test: Covered end-to-end by the engine `tests` submodule and
//! `main::run_workflow`.

use std::path::PathBuf;
use std::time::Instant;

use tracing::info;

use crate::agents::AgentConfig;
use crate::context::TurnRecord;
use crate::ipc::extract_summary;
use crate::perf::PerfCollector;
use crate::workflow::config::{Assignments, WorkflowDef};
use crate::workflow::context::WorkflowContext;
use crate::workflow::error::WorkflowError;

use super::super::helpers::relocate_plan_outputs_from_project_root;
use super::super::state::emit_progress_event;
use super::super::step_dispatch::discover_project_dir;
use super::DiscoveredSkill;
use super::WorkflowEngine;
use super::finalize::FinalizeState;
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
        // #84: Accumulate cost + files count so the success hook has a
        // single-line summary without re-reading the flushed perf JSON.
        let mut total_cost_usd: f64 = 0.0;
        let mut files_generated: usize = 0;
        let mut qa_summary: String = "n/a".to_string();
        // #123: Track whether the code phase ran under a `claude-code` runner
        // so we can prepend a path-search hint to the QA agent's prompt.
        let mut code_phase_used_claude_code: bool = false;

        // #126/#153/#222: Pre-create + canonicalize out_dir and code_dir (the
        // latter falling back to out_dir). See `setup::resolve_dirs`.
        let (out_dir, code_dir) = self.resolve_dirs(out_dir, code_dir).await?;

        // #196/#205: Detect the active persona from the RAW task text before
        // it's cleaned and handed to the context. See `setup::detect_persona`.
        let (persona, cleaned_task) = detect_persona(&task);

        let mut ctx = WorkflowContext::builder(cleaned_task)
            .with_out_dir(out_dir.clone())
            .build();

        // #56: Track the first failing phase name and the error so we can
        // flush a partial perf record before propagating.
        let mut failed_phase: Option<String> = None;
        let mut first_error: Option<WorkflowError> = None;

        // Fix 2 (claude-mpm parity): QA gating state. `qa_failure_feedback`
        // carries the `[QA FEEDBACK] …` block to prepend to the next
        // code-phase render; `qa_retry_count` bounds retries at exactly one.
        let mut qa_failure_feedback: Option<String> = None;
        let mut qa_retry_count: u32 = 0;

        // #173: Run pre-plan skill discovery once for the whole workflow so
        // every skill the engine considered is recorded in the perf record
        // (`skills_considered`) regardless of which phase eventually consumed
        // them.
        let discovered_skills: Vec<DiscoveredSkill> = self.discover_skills_for_task(&ctx.task, 8);
        for skill in &discovered_skills {
            perf.record_skill_considered(&skill.name);
        }

        // #347 follow-up: Pre-index existing source under `code_dir` before any
        // AST-native phase runs so the AST tool surface starts warm. See
        // `setup::maybe_pre_index_ast`.
        self.maybe_pre_index_ast(&def, &code_dir);

        'phase_loop: for phase in &def.phases {
            // #82/#196/#209: Skip `skip: true` and persona-opt-out phases,
            // recording a sentinel output. See `setup::phase_should_skip`.
            if self.phase_should_skip(phase, persona, &mut ctx) {
                continue;
            }
            info!(phase = %phase.name, agent = %phase.agent, "running phase");

            // Per-phase AST-native override (#347/#348 follow-up). The override
            // lives in a process-wide AtomicBool, so we save the prior value,
            // apply the phase's preference, and let `_ast_guard` restore it
            // when the phase completes (Drop runs on every continue/break/error).
            struct AstNativeGuard {
                prev: bool,
                applied: bool,
            }
            impl Drop for AstNativeGuard {
                fn drop(&mut self) {
                    if self.applied {
                        crate::ast::set_ast_native_override(self.prev);
                    }
                }
            }
            let _ast_guard = {
                let prev = crate::ast::is_ast_native_overridden();
                let applied = if let Some(phase_ast) = phase.ast_native {
                    crate::ast::set_ast_native_override(phase_ast);
                    true
                } else {
                    false
                };
                AstNativeGuard { prev, applied }
            };

            // #149: Stream phase-start to stderr + machine progress event.
            if let Some(rep) = &self.progress {
                rep.phase_start(&phase.name);
            }
            emit_progress_event(&phase.name, "running", 0.0, 0.0, None);

            // #140/#222: Refresh the discovered project_dir inside `code_dir`
            // (where source files actually land) before rendering templates
            // that reference {{project_dir}}.
            if let Some(dir) = code_dir.as_deref() {
                ctx.project_dir = discover_project_dir(dir);
                if let Some(p) = &ctx.project_dir
                    && p.as_path() != dir
                {
                    info!(
                        project_dir = %p.display(),
                        code_dir = %dir.display(),
                        "discovered project subdirectory for {{{{project_dir}}}}"
                    );
                }
            }

            // Assemble the full per-phase prompt (template render + every
            // context-injection layer). See `prompt::assemble_phase_prompt`.
            let rendered = self
                .assemble_phase_prompt(
                    phase,
                    &ctx,
                    &out_dir,
                    &code_dir,
                    &discovered_skills,
                    code_phase_used_claude_code,
                    &mut qa_failure_feedback,
                    &mut perf,
                )
                .await;

            let phase_started = Instant::now();

            // #107: `phase.model` is threaded through `RunContext` below.
            if let Some(m) = &phase.model {
                tracing::debug!(phase = %phase.name, model = %m, "phase model");
            }

            // #51: Check the agent's TOML for `persistent_session`.
            let persistent = AgentConfig::by_name(&phase.agent)
                .map(|c| c.agent.persistent_session)
                .unwrap_or(false);

            // #88: Wave-loop execution path. When this is the "code" phase AND
            // the plan-agent has written `assignments.json` into `out_dir`,
            // run one code-agent per file in topological wave order.
            let wave_assignments = if phase.name == "code" {
                match out_dir.as_deref() {
                    Some(dir) => {
                        let loaded = Assignments::load(dir);
                        if loaded.is_some() {
                            tracing::info!(
                                phase = %phase.name,
                                out_dir = %dir.display(),
                                "code phase: taking WAVE LOOP path (assignments.json present)"
                            );
                        } else {
                            tracing::info!(
                                phase = %phase.name,
                                out_dir = %dir.display(),
                                "code phase: taking LEGACY monolithic path (assignments.json absent or unparseable)"
                            );
                        }
                        loaded
                    }
                    None => {
                        tracing::debug!(
                            phase = %phase.name,
                            "code phase: no out_dir configured; wave loop cannot run"
                        );
                        None
                    }
                }
            } else {
                None
            };

            // Per-phase AST-native override (#349 follow-up to #348). Save the
            // current global override, apply the phase's override (if any) for
            // the duration of the dispatch, then restore in BOTH Ok and Err
            // paths so subsequent phases see the correct inherited value.
            let prior_ast_native = crate::ast::is_ast_native_overridden();
            if let Some(phase_ast) = phase.ast_native {
                crate::ast::set_ast_native_override(phase_ast);
            }

            let output_result = self
                .dispatch_phase(
                    phase,
                    &rendered,
                    &out_dir,
                    &code_dir,
                    wave_assignments,
                    persistent,
                )
                .await;

            // Restore the global AST-native override now that this phase's
            // dispatch is complete (regardless of Ok/Err).
            crate::ast::set_ast_native_override(prior_ast_native);

            let output = match output_result {
                Ok(o) => o,
                Err(e) => {
                    // Record perf + ticket + progress for the failed phase, then
                    // capture the error and break. See `post_phase::record_phase_failure`.
                    failed_phase = Some(phase.name.clone());
                    first_error = Some(
                        self.record_phase_failure(phase, e, phase_started, &mut perf)
                            .await,
                    );
                    break 'phase_loop;
                }
            };

            // #27: Prefer the agent-supplied summary; else extract one ourselves.
            let summary = output
                .summary
                .clone()
                .or_else(|| Some(extract_summary(&output.content)));
            let content_len = output.content.len();
            let summary_len = summary.as_ref().map(|s| s.len()).unwrap_or(0);

            // #47: Record perf BEFORE we move `output.content` into the ctx.
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
            perf.record_phase(&phase.name, duration_ms, &phase_model, &output.usage);

            // #84: Compute this phase's cost for the ticket comment and
            // accumulate into the workflow total.
            let phase_cost = crate::perf::cost_usd(
                &phase_model,
                output.usage.prompt_tokens,
                output.usage.completion_tokens,
                output.usage.cache_read_tokens,
                output.usage.cache_creation_tokens,
            );
            total_cost_usd += phase_cost;

            // #149: Stream phase-done to stderr + machine progress event. For
            // QA, surface the first content line as a note (e.g. "35/35 passed").
            let phase_note: Option<String> = if phase.name == "qa" {
                output
                    .content
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .map(|l| l.chars().take(60).collect::<String>())
            } else {
                None
            };
            let elapsed = std::time::Duration::from_millis(duration_ms);
            if let Some(rep) = &self.progress {
                rep.phase_done(
                    &phase.name,
                    elapsed,
                    phase_cost as f32,
                    phase_note.as_deref(),
                );
            }
            emit_progress_event(
                &phase.name,
                "done",
                elapsed.as_secs_f32(),
                phase_cost as f32,
                phase_note.as_deref(),
            );

            // #84: Post per-phase ticket comment. Non-fatal on failure.
            if let Some(tm_cell) = &self.ticket_manager {
                let tm = tm_cell.lock().await;
                if let Err(e) = tm
                    .on_phase_complete(
                        &phase.name,
                        &phase_model,
                        duration_ms,
                        output.usage.prompt_tokens,
                        output.usage.completion_tokens,
                        phase_cost,
                        "✅ success",
                    )
                    .await
                {
                    tracing::warn!(error = %e, phase = %phase.name,
                        "ticket manager: on_phase_complete failed");
                }
            }

            // #84 + Fix 2 (claude-mpm parity): QA summary + structured envelope
            // handling (test counts + one-shot retry gating). See
            // `post_phase::handle_qa_envelope`.
            self.handle_qa_envelope(
                phase,
                &output.content,
                &mut perf,
                &mut qa_summary,
                &mut failed_phase,
                &mut qa_retry_count,
                &mut qa_failure_feedback,
            );

            // #70: Fire-and-forget record this turn for background indexing.
            if let Some(idx) = &self.indexer {
                let run_id =
                    std::env::var("OPEN_MPM_RUN_ID").unwrap_or_else(|_| "unknown".to_string());
                idx.record(TurnRecord {
                    session_id: run_id,
                    agent: phase.agent.clone(),
                    turn_number: ctx.phase_outputs.len() as u32,
                    timestamp: chrono::Utc::now(),
                    prompt_text: rendered.clone(),
                    response_text: output.content.clone(),
                    prompt_tokens: output.usage.prompt_tokens,
                    completion_tokens: output.usage.completion_tokens,
                });
            }

            ctx.record_phase(&phase.name, output.content, summary);

            // #68: After the plan phase, try to extract the goal block so
            // downstream phases can inject it into their prompts.
            if ctx.goal_block.is_none()
                && let Some(content) = ctx.phase_outputs.get(&phase.name)
                && let Some(g) = crate::context::goals::parse_goal_block_from_text(content)
            {
                tracing::info!(
                    phase = %phase.name,
                    primary = %g.primary,
                    secondary_count = g.secondary.len(),
                    "parsed goal block from phase output"
                );
                ctx.goal_block = Some(g);
            }
            info!(
                phase = %phase.name,
                content_chars = content_len,
                summary_chars = summary_len,
                duration_ms,
                prompt_tokens = output.usage.prompt_tokens,
                completion_tokens = output.usage.completion_tokens,
                cache_read = output.usage.cache_read_tokens,
                cache_creation = output.usage.cache_creation_tokens,
                "phase complete"
            );

            // #160: After the plan phase, check for a misrouted
            // assignments.json at the project root and relocate it.
            if phase.name == "plan"
                && let Some(dir) = out_dir.as_deref()
                && let Err(e) = relocate_plan_outputs_from_project_root(dir).await
            {
                tracing::warn!(
                    error = %e,
                    "post-plan relocation check failed; continuing"
                );
            }

            // #123/#222: After the code phase for a claude-code runner,
            // reconcile file locations into `code_dir`. Returns whether this was
            // a claude-code code phase so the QA prompt can be hinted later.
            if self.reconcile_code_phase(phase, &out_dir, &code_dir).await {
                code_phase_used_claude_code = true;
            }

            // #64/#222: Extract `## File: <path>` sections from this phase's
            // output BEFORE the next phase runs (code -> code_dir, else out_dir).
            files_generated += self
                .extract_phase_files(phase, &ctx, &out_dir, &code_dir)
                .await?;
        }

        // Hand off all accumulated run state to the finalization tail (report
        // write, perf flush, auto-push, ticket hooks, progress, error return).
        self.finalize_run(
            &def,
            FinalizeState {
                ctx,
                perf,
                out_dir,
                first_error,
                failed_phase,
                workflow_started,
                total_cost_usd,
                files_generated,
                qa_summary,
            },
        )
        .await
    }
}
