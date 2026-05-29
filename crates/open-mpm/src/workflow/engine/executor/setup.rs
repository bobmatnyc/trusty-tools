//! Run-setup helpers for the workflow engine: output/code directory resolution
//! and AST pre-indexing.
//!
//! Why: The phase loop needs canonical, pre-created `out_dir` / `code_dir`
//! paths and (when any phase opts into AST-native) a warm pre-indexed symbol
//! registry. Pulling that setup out of `run` (#359) keeps the driver under the
//! 500-line cap.
//! What: `resolve_dirs` pre-creates + canonicalizes both directories (with
//! `code_dir` falling back to `out_dir`); `maybe_pre_index_ast` walks `code_dir`
//! once and installs the process-global registry when an AST-native phase exists.
//! Test: Covered end-to-end by the engine `tests` submodule
//! (`legacy_monolithic_path_passes_absolute_working_dir` exercises canonicalization).

use std::path::PathBuf;

use tracing::info;

use crate::agents::persona::{detect_persona_matched, strip_persona_tag};
use crate::workflow::config::{PhaseDef, WorkflowDef};
use crate::workflow::context::WorkflowContext;
use crate::workflow::error::WorkflowError;

use super::super::state::phases_to_skip;
use super::WorkflowEngine;

/// Detect the active persona and emit the start-of-run persona signals (#196/#205).
///
/// Why: Persona detection must run on the RAW task text before it's cleaned and
/// handed to the context, and operators need a visible warning when a
/// non-engineer persona will skip phases. Keeping it as a free function (no
/// `&self` needed) lets `run` call it before building the context.
/// What: Returns `(persona, cleaned_task)` — the detected persona id and the
/// task with any `[persona]` marker stripped. Emits a `PersonaDetected` event
/// and, when the persona skips phases, a stderr + tracing warning.
/// Test: `plan_agent_context_includes_skill_summaries` pins the engineer-pin
/// behavior; the skip-warning path is side-effect-only.
pub(super) fn detect_persona(task: &str) -> (&'static str, String) {
    let (persona, matched_kw) = detect_persona_matched(task);
    let cleaned_task = strip_persona_tag(task);
    {
        let session_id = std::env::var("OPEN_MPM_RUN_ID").unwrap_or_default();
        crate::events::emit(crate::events::Event::PersonaDetected {
            session_id,
            persona: persona.to_string(),
        });
    }
    info!(persona = %persona, matched = %matched_kw, "workflow persona detected");
    // #205: emit a visible warning when a non-engineer persona will skip
    // phases, so operators can see *why* parts of the pipeline are absent.
    let skipped = phases_to_skip(persona);
    if !skipped.is_empty() {
        let phases_list = skipped.join(", ");
        tracing::warn!(
            persona = %persona,
            matched = %matched_kw,
            skipped = %phases_list,
            "[open-mpm] persona detected: {persona} (matched: \"{matched_kw}\") — skipping phases: {phases_list}"
        );
        eprintln!(
            "[open-mpm] persona detected: {persona} (matched: \"{matched_kw}\") — skipping phases: {phases_list}"
        );
    }
    (persona, cleaned_task)
}

impl WorkflowEngine {
    /// Pre-create and canonicalize `out_dir` and `code_dir` (#153/#222).
    ///
    /// Why: Subprocess runners set these as CWD, so they must exist and be
    /// absolute before any agent runs; `code_dir` falls back to `out_dir` so
    /// legacy single-dir callers are unchanged.
    /// What: Creates each directory, canonicalizes to an absolute path (falling
    /// back to the original on failure), and returns the resolved pair. Logs a
    /// line when the two diverge.
    /// Test: `legacy_monolithic_path_passes_absolute_working_dir`.
    pub(super) async fn resolve_dirs(
        &self,
        out_dir: Option<PathBuf>,
        code_dir: Option<PathBuf>,
    ) -> Result<(Option<PathBuf>, Option<PathBuf>), WorkflowError> {
        // #126 bug 3: Pre-create out_dir before any agent runs so subprocess
        // runners can safely set it as CWD without hitting ENOENT on first use.
        if let Some(dir) = &out_dir {
            tokio::fs::create_dir_all(dir).await.map_err(|e| {
                WorkflowError::ConfigInvalid(format!(
                    "failed to create out_dir {}: {e}",
                    dir.display()
                ))
            })?;
        }
        // #153: Replace `out_dir` with its canonical absolute form (after the
        // directory exists). On failure, fall back to the original path.
        let out_dir: Option<PathBuf> = out_dir.map(|dir| {
            std::fs::canonicalize(&dir)
                .inspect_err(|e| {
                    tracing::warn!(
                        out_dir = %dir.display(),
                        error = %e,
                        "failed to canonicalize out_dir; using original path"
                    );
                })
                .unwrap_or(dir)
        });

        // #222: Resolve `code_dir` — the destination for generated source files.
        // When unset, falls back to `out_dir` so legacy callers see no behavior
        // change.
        let code_dir: Option<PathBuf> = match code_dir {
            Some(dir) => {
                tokio::fs::create_dir_all(&dir).await.map_err(|e| {
                    WorkflowError::ConfigInvalid(format!(
                        "failed to create code_dir {}: {e}",
                        dir.display()
                    ))
                })?;
                Some(
                    std::fs::canonicalize(&dir)
                        .inspect_err(|e| {
                            tracing::warn!(
                                code_dir = %dir.display(),
                                error = %e,
                                "failed to canonicalize code_dir; using original path"
                            );
                        })
                        .unwrap_or(dir),
                )
            }
            None => out_dir.clone(),
        };
        if let (Some(o), Some(c)) = (out_dir.as_ref(), code_dir.as_ref())
            && o != c
        {
            info!(
                out_dir = %o.display(),
                code_dir = %c.display(),
                "#222: separated artifacts dir (out_dir) from code dir (code_dir)"
            );
        }

        Ok((out_dir, code_dir))
    }

    /// Pre-index existing source under `code_dir` when any phase opts into
    /// AST-native, so the AST tool surface starts warm (#347 follow-up).
    ///
    /// Why: Research/plan agents otherwise pay a per-file disk parse on first
    /// lookup; walking the tree once up front populates the process-global
    /// registry.
    /// What: When any phase sets `ast_native = Some(true)` (or the global
    /// override is on) AND `code_dir` exists, walks it and installs the
    /// resulting registry. Failures are logged and non-fatal.
    /// Test: Indirectly via `pre_indexed_registry_round_trip` in `src/ast/mod.rs`
    /// and the workflow integration suite.
    pub(super) fn maybe_pre_index_ast(&self, def: &WorkflowDef, code_dir: &Option<PathBuf>) {
        let any_ast_phase = def.phases.iter().any(|p| p.ast_native == Some(true))
            || crate::ast::is_ast_native_overridden();
        if any_ast_phase && let Some(dir) = code_dir.as_ref().filter(|d| d.exists()) {
            let started = std::time::Instant::now();
            match crate::ast::pre_index_directory(dir, dir) {
                Ok(registry) => {
                    let symbol_count = registry.len();
                    crate::ast::set_pre_indexed_registry(registry);
                    info!(
                        duration_ms = started.elapsed().as_millis() as u64,
                        symbols = symbol_count,
                        path = %dir.display(),
                        "AST pre-index complete"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %dir.display(),
                        "AST pre-index failed, continuing with empty registry"
                    );
                }
            }
        }
    }

    /// Decide whether a phase should be skipped, recording a sentinel output
    /// when so (#82/#196/#209).
    ///
    /// Why: Two independent skip conditions — `skip: true` in the workflow JSON
    /// and persona-driven opt-out — both need to write a sentinel into the
    /// context so downstream `{{phase_name}}` templates render a meaningful
    /// message instead of `(missing: …)`.
    /// What: Returns `true` (and records the sentinel + emits a `PhaseSkipped`
    /// event for the persona case) when the phase must be skipped; `false` when
    /// it should run.
    /// Test: Persona skipping is covered by `plan_agent_context_includes_skill_summaries`
    /// (which pins engineer so research runs); the `skip: true` path is
    /// config-driven.
    pub(super) fn phase_should_skip(
        &self,
        phase: &PhaseDef,
        persona: &str,
        ctx: &mut WorkflowContext,
    ) -> bool {
        // #82: Phases with `skip: true` are opt-out (the `docs` phase ships
        // disabled by default).
        if phase.skip.unwrap_or(false) {
            info!(phase = %phase.name, agent = %phase.agent, "skipping phase (skip=true)");
            // #209: Write a sentinel so downstream templates render a clear
            // "skipped" message instead of `(missing: phase_name)`.
            ctx.record_phase(
                &phase.name,
                "(skipped: disabled in workflow config)".to_string(),
                None,
            );
            return true;
        }
        // #196: Persona-driven phase skipping.
        if phases_to_skip(persona).contains(&phase.name.as_str()) {
            info!(
                phase = %phase.name,
                persona = %persona,
                "skipping phase (persona opt-out)"
            );
            let session_id = std::env::var("OPEN_MPM_RUN_ID").unwrap_or_default();
            crate::events::emit(crate::events::Event::PhaseSkipped {
                session_id,
                phase: phase.name.clone(),
                persona: persona.to_string(),
            });
            // #209: Write a sentinel string for downstream templates.
            ctx.record_phase(
                &phase.name,
                format!("(skipped: {persona} persona does not run this phase)"),
                None,
            );
            return true;
        }
        false
    }
}
