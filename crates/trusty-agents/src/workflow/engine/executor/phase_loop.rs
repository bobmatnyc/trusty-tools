//! Shared phase-loop driver used by both a fresh run and `tagent resume`
//! (#3062, SPEC-AGENTFW-02 §3).
//!
//! Why: A resumed run must execute the exact same phase-dispatch logic as a
//! fresh run — prompt assembly, wave/parallel/persistent dispatch, QA
//! gating, file extraction — starting partway through `def.phases` instead
//! of at index 0, with `WorkflowContext` pre-seeded from a checkpoint instead
//! of empty. Extracting the loop body out of `run.rs` (which built it inline
//! for the fresh-run case only) lets `run` and `resume` share one
//! implementation instead of drifting apart. This also owns the phase-
//! boundary checkpoint writes: `PhaseRunning{i}` on entering a non-skipped
//! phase, `PhaseComplete{i}` once that phase's output has been recorded AND
//! any produced files extracted (deliberately later than SPEC-AGENTFW-02
//! §3.3's literal "immediately after `ctx.record_phase`" — see the doc on
//! `run_phase_loop` for why), `Retrying{i,1}` on the one-shot QA retry
//! signal, and `Failed{i}` on a phase dispatch error.
//! What: `PhaseLoopSeed` bundles the run-scoped state that differs between a
//! fresh run (all zeroed, `start_index: 0`) and a resume (loaded from the
//! checkpoint, `start_index` past the last completed phase).
//! `WorkflowEngine::run_phase_loop` is the (former) body of
//! `run_with_perf_and_dirs`'s `'phase_loop`, now parameterized over the
//! start index and seed state, ending in the same `finalize_run` call.
//! Test: Exercised end-to-end via the engine `tests` submodule (fresh-run
//! path, unchanged behavior) and `checkpoint_resume_tests` (resume path,
//! phases before `start_index` are not re-dispatched).

use std::path::PathBuf;
use std::time::Instant;

use tracing::info;

use crate::agents::AgentConfig;
use crate::context::TurnRecord;
use crate::ipc::extract_summary;
use crate::perf::PerfCollector;
use crate::workflow::config::{Assignments, WorkflowDef};
use crate::workflow::context::WorkflowContext;
use crate::workflow::engine::checkpoint::{self, CheckpointRecord, RunState};
use crate::workflow::error::WorkflowError;

use super::super::helpers::relocate_plan_outputs_from_project_root;
use super::super::state::emit_progress_event;
use super::super::step_dispatch::discover_project_dir;
use super::DiscoveredSkill;
use super::WorkflowEngine;
use super::finalize::FinalizeState;

/// Run-scoped state that differs between a fresh run (start_index 0,
/// zeroed/defaulted) and a resume (loaded from a `CheckpointRecord`).
///
/// Why: Bundling these into one struct keeps `run_phase_loop`'s signature
/// readable and makes the fresh-vs-resume seeding differences explicit at
/// each call site instead of scattered across a dozen positional args.
/// What: Plain owned fields, consumed once at the top of `run_phase_loop`.
/// Test: Covered indirectly — both call sites (`run.rs`, `resume.rs`)
/// construct one and are exercised by the engine test suites.
pub(super) struct PhaseLoopSeed {
    /// Index into `def.phases` to start iterating from (0 for a fresh run).
    pub start_index: usize,
    /// Run id used for both telemetry (`TAGENT_RUN_ID`) and the checkpoint
    /// journal's `run_id` field.
    pub run_id: String,
    /// Original (uncleaned) task text — persisted into the checkpoint
    /// verbatim so a later resume sees the same persona tag.
    pub raw_task: String,
    /// ISO8601 run start time — a fresh run stamps `now()`; a resume reuses
    /// the checkpoint's original `started_at` so the journal reflects true
    /// run age, not just the resume's restart time.
    pub started_at: String,
    pub workflow_started: Instant,
    pub qa_retry_count: u32,
    pub qa_failure_feedback: Option<String>,
    pub total_cost_usd: f64,
    pub files_generated: usize,
    pub qa_summary: String,
    pub code_phase_used_claude_code: bool,
}

/// Fixed (per-run) fields needed to build a `CheckpointRecord` at each
/// phase boundary — split out of `PhaseLoopSeed` because these don't change
/// across the loop's iterations, unlike `qa_retry_count`/`qa_failure_feedback`.
struct CheckpointMeta {
    run_id: String,
    workflow: String,
    out_dir: PathBuf,
    code_dir: PathBuf,
    task: String,
    phase_names: Vec<String>,
    started_at: String,
}

/// Build and persist one checkpoint write. Failures are logged and
/// non-fatal (matching the engine's existing "log and continue" convention
/// for non-critical persistence side effects — perf flush, ticket hooks):
/// a checkpoint write failure must not abort an otherwise-succeeding
/// workflow run, it just degrades that run's crash-resumability.
fn write_checkpoint(
    project_dir: &std::path::Path,
    meta: &CheckpointMeta,
    state: RunState,
    ctx: &WorkflowContext,
    qa_retry_count: u32,
    qa_failure_feedback: &Option<String>,
) {
    let record = CheckpointRecord {
        schema_version: checkpoint::CHECKPOINT_SCHEMA_VERSION,
        run_id: meta.run_id.clone(),
        workflow: meta.workflow.clone(),
        state,
        phase_names: meta.phase_names.clone(),
        out_dir: meta.out_dir.clone(),
        code_dir: meta.code_dir.clone(),
        task: meta.task.clone(),
        phase_outputs: ctx.phase_outputs.clone().into_iter().collect(),
        // Code-critic finding (PR #3244): persist the SUMMARY map alongside
        // the full-content map. `render_template` injects `phase_summaries`
        // into downstream `{{phase_name}}` substitutions, not
        // `phase_outputs` — replaying only `phase_outputs` on resume would
        // silently blow up every downstream prompt with raw, un-digested
        // phase content.
        phase_summaries: ctx.phase_summaries.clone().into_iter().collect(),
        goal_block: ctx.goal_block.clone(),
        qa_retry_count,
        qa_failure_feedback: qa_failure_feedback.clone(),
        started_at: meta.started_at.clone(),
        updated_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
    };
    if let Err(e) = record.write(project_dir) {
        tracing::warn!(
            run_id = %meta.run_id,
            state = ?record.state,
            error = %e,
            "failed to write phase checkpoint (non-fatal; resume durability degraded for this phase)"
        );
    }
}

impl WorkflowEngine {
    /// Iterate `def.phases` from `seed.start_index` onward, dispatching each
    /// non-skipped phase and persisting a checkpoint at every phase
    /// boundary, then hand off to `finalize_run`.
    ///
    /// Why: This is the single shared driver for both a fresh run
    /// (`start_index: 0`, empty `ctx`) and `tagent resume` (`start_index`
    /// past the last completed phase, `ctx` pre-seeded from the checkpoint's
    /// `phase_outputs`). Sharing one implementation means resume can never
    /// silently diverge from fresh-run dispatch behavior.
    /// What: Mirrors the pre-#3062 `run_with_perf_and_dirs` loop body
    /// exactly for dispatch/QA/extraction logic, adding checkpoint writes at
    /// three points: `PhaseRunning{i}` right after the skip-check (matching
    /// SPEC-AGENTFW-02 §3.2's "entering the first non-skipped phase"),
    /// `Retrying{i,1}` immediately after `handle_qa_envelope` flips
    /// `qa_retry_count` 0->1, and `PhaseComplete{i}` / `Failed{i}` at the
    /// true end of the iteration. `PhaseComplete{i}` is written AFTER
    /// `extract_phase_files` completes — deliberately later than the spec's
    /// literal "immediately after `ctx.record_phase`" — because a phase's
    /// generated files are extracted to disk only after `record_phase`;
    /// writing the checkpoint before extraction would let a crash between
    /// the two leave a `PhaseComplete{i}` journal entry whose files were
    /// never written, so a resume would skip re-dispatching phase `i` (its
    /// checkpoint says done) while `code_dir`/`out_dir` is silently missing
    /// its output. Ending the checkpoint boundary after extraction closes
    /// that gap.
    /// Test: `checkpoint_resume_tests::resume_skips_completed_phases_and_reuses_their_output`.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_phase_loop(
        &self,
        def: &WorkflowDef,
        mut ctx: WorkflowContext,
        mut perf: PerfCollector,
        out_dir: Option<PathBuf>,
        code_dir: Option<PathBuf>,
        persona: &str,
        discovered_skills: &[DiscoveredSkill],
        seed: PhaseLoopSeed,
    ) -> Result<(WorkflowContext, crate::perf::PerfRecord), WorkflowError> {
        let PhaseLoopSeed {
            start_index,
            run_id,
            raw_task,
            started_at,
            workflow_started,
            mut qa_retry_count,
            mut qa_failure_feedback,
            mut total_cost_usd,
            mut files_generated,
            mut qa_summary,
            mut code_phase_used_claude_code,
        } = seed;

        let mut failed_phase: Option<String> = None;
        let mut first_error: Option<WorkflowError> = None;

        let cp_project_dir = crate::usage::project_dir();
        let cp_meta = CheckpointMeta {
            run_id: run_id.clone(),
            workflow: def.name.clone(),
            out_dir: out_dir.clone().unwrap_or_default(),
            code_dir: code_dir
                .clone()
                .or_else(|| out_dir.clone())
                .unwrap_or_default(),
            task: raw_task,
            phase_names: def.phases.iter().map(|p| p.name.clone()).collect(),
            started_at,
        };

        'phase_loop: for (phase_index, phase) in def.phases.iter().enumerate().skip(start_index) {
            // #82/#196/#209: Skip `skip: true` and persona-opt-out phases,
            // recording a sentinel output. See `setup::phase_should_skip`.
            if self.phase_should_skip(phase, persona, &mut ctx) {
                continue;
            }
            info!(phase = %phase.name, agent = %phase.agent, "running phase");
            // #3062: Pending -> PhaseRunning{i} / PhaseComplete{i-1} -> PhaseRunning{i}.
            write_checkpoint(
                &cp_project_dir,
                &cp_meta,
                RunState::PhaseRunning { phase_index },
                &ctx,
                qa_retry_count,
                &qa_failure_feedback,
            );

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
                    discovered_skills,
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
                    // #3062: PhaseRunning{i} -> Failed{i}.
                    write_checkpoint(
                        &cp_project_dir,
                        &cp_meta,
                        RunState::Failed { phase_index },
                        &ctx,
                        qa_retry_count,
                        &qa_failure_feedback,
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
            let qa_retry_before = qa_retry_count;
            self.handle_qa_envelope(
                phase,
                &output.content,
                &mut perf,
                &mut qa_summary,
                &mut failed_phase,
                &mut qa_retry_count,
                &mut qa_failure_feedback,
            );
            if qa_retry_before == 0 && qa_retry_count == 1 {
                // #3062: PhaseRunning{i} -> Retrying{i,1} (SPEC-AGENTFW-02 §3.2's
                // "existing Fix 2 one-shot code-phase retry", signaled from
                // whichever phase index `handle_qa_envelope` is currently
                // processing — today always the `qa` phase itself, since this
                // engine's loop has no phase-index jump-back).
                write_checkpoint(
                    &cp_project_dir,
                    &cp_meta,
                    RunState::Retrying {
                        phase_index,
                        attempt: qa_retry_count,
                    },
                    &ctx,
                    qa_retry_count,
                    &qa_failure_feedback,
                );
            }

            // #70: Fire-and-forget record this turn for background indexing.
            if let Some(idx) = &self.indexer {
                idx.record(TurnRecord {
                    session_id: run_id.clone(),
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

            // #3062: PhaseRunning{i} -> PhaseComplete{i}, written after file
            // extraction — see the doc comment above for why this is later
            // than the spec's literal "immediately after ctx.record_phase".
            write_checkpoint(
                &cp_project_dir,
                &cp_meta,
                RunState::PhaseComplete { phase_index },
                &ctx,
                qa_retry_count,
                &qa_failure_feedback,
            );
        }

        // Hand off all accumulated run state to the finalization tail (report
        // write, perf flush, auto-push, ticket hooks, progress, error return,
        // and — #3062 — checkpoint deletion on full success).
        self.finalize_run(
            def,
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
                run_id,
            },
        )
        .await
    }
}
