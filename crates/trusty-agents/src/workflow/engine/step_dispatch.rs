//! Phase dispatch helpers: project-dir discovery, the code-phase wave loop, and
//! package-marker pre-creation.
//!
//! Why: The code phase's per-file wave loop is the most intricate part of the
//! engine — it builds focused per-file prompts, enforces single-file write
//! authority, threads model overrides, and reconciles written files. Isolating
//! it (plus the small `discover_project_dir` / `should_precreate` /
//! `precreate_package_markers` helpers it relies on) keeps the executor's phase
//! loop readable.
//! What: `discover_project_dir` locates the generated project root inside a
//! directory; `run_wave_loop` runs one code-agent per file in wave order;
//! `should_precreate` / `precreate_package_markers` pre-seed empty package
//! markers so the post-dispatch presence check never false-negatives.
//! Test: `discover_project_dir_*`, `wave_loop_*`, `should_precreate_*`,
//! `precreate_package_markers_*` in `executor`'s test module.

use std::path::PathBuf;
use std::sync::Arc;

use tracing::info;

use crate::agents::AgentConfig;
use crate::tools::traits::{AgentOutput, AgentRunner, RunContext};
use crate::workflow::config::{Assignments, FileAssignment, PhaseDef, safe_join};

use super::retry::run_wave_file_with_retry;

/// Discover the generated project root inside `out_dir`.
///
/// Why (#140): The code phase (especially claude-code engineers) often writes
/// the generated project into a subdirectory of `out_dir` (e.g.
/// `out/l4-bakeoff-.../doc_pipeline/` rather than directly into
/// `out/l4-bakeoff-.../`). When QA runs pytest against `out_dir`, pytest finds
/// no `tests/` and exits with code 5 ("no tests collected").
/// What: Returns `out_dir` itself if it directly contains `pyproject.toml`,
/// otherwise the first immediate child directory (excluding `.venv`,
/// `__pycache__`, `node_modules`, dotfiles) that contains `pyproject.toml`.
/// Falls back to `out_dir` if no project root is found so callers always get a
/// usable path.
/// Test: `discover_project_dir_finds_subdirectory`,
/// `discover_project_dir_falls_back_to_out_dir_when_no_project`.
pub(crate) fn discover_project_dir(out_dir: &std::path::Path) -> Option<PathBuf> {
    // Direct hit: pyproject.toml sits right in out_dir.
    if out_dir.join("pyproject.toml").is_file() {
        return Some(out_dir.to_path_buf());
    }
    // Scan one level of immediate children for a project marker.
    let entries = match std::fs::read_dir(out_dir) {
        Ok(e) => e,
        Err(_) => return Some(out_dir.to_path_buf()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        // Skip virtualenvs, caches, and hidden directories — they often
        // contain their own pyproject.toml (e.g. inside site-packages)
        // and would produce false positives.
        if name.starts_with('.') || name == "__pycache__" || name == "node_modules" {
            continue;
        }
        if path.join("pyproject.toml").is_file() {
            return Some(path);
        }
    }
    // No subdirectory project found — fall back to out_dir so {{project_dir}}
    // behaves identically to {{out_dir}} for the common case.
    Some(out_dir.to_path_buf())
}

pub(crate) async fn run_wave_loop(
    assignments: Assignments,
    phase: &PhaseDef,
    runner: Arc<dyn AgentRunner>,
    code_dir: &std::path::Path,
    artifacts_dir: &std::path::Path,
    progress: Option<Arc<crate::progress::ProgressReporter>>,
) -> anyhow::Result<AgentOutput> {
    // #222: `code_dir` is where source files are written; `artifacts_dir` is
    // where the plan-agent staged `assignments.json` and `stubs/`. In the
    // legacy default these point to the same directory.
    let out_dir = code_dir;
    let error_convention = assignments
        .error_convention
        .as_deref()
        .unwrap_or("exceptions")
        .to_string();

    let mut combined = String::new();
    let mut agg_usage = crate::perf::TokenUsage::default();
    let mut written_paths: Vec<String> = Vec::new();

    // #231: Source model-elevation config from the agent's TOML once, before
    // the wave loop dispatches files. Missing or unparseable TOML simply
    // disables elevation (None, None) — the same fail-soft posture used
    // elsewhere in this file.
    let (elevation_threshold, elevation_model) =
        match AgentConfig::by_name_async(&phase.agent).await {
            Ok(cfg) => (cfg.llm.elevation_threshold, cfg.llm.elevation_model.clone()),
            Err(_) => (None, None),
        };

    // #150: Pre-create empty package-marker files (e.g. `__init__.py`) before
    // the wave loop dispatches any agents. Why: engineer agents treat
    // trivially-empty placeholder files as not worth writing, but the
    // post-dispatch file-presence check then hard-errors. Pre-creating them
    // as empty files satisfies the check regardless of agent behavior.
    precreate_package_markers(&assignments, out_dir).await?;

    let wave_total = assignments.waves.len();
    for wave in &assignments.waves {
        info!(wave = wave.wave, files = wave.files.len(), "starting wave");
        // #149: Stream per-wave progress so the code phase doesn't appear
        // stuck during long sequential waves.
        let wave_started = std::time::Instant::now();
        if let Some(rep) = &progress {
            rep.wave_start(wave.wave as usize, wave_total, wave.files.len());
        }
        for file in &wave.files {
            // #114: Defense-in-depth — `Assignments::load` already validates
            // file paths, but a caller constructing an `Assignments` value
            // programmatically would bypass that. Re-check here so every
            // `out_dir.join(&file.path)` is guaranteed safe.
            if let Err(e) = Assignments::validate_file_path(&file.path) {
                return Err(anyhow::anyhow!(
                    "wave-loop: rejecting unsafe file path: {e}"
                ));
            }
            // #114: Belt-and-suspenders — even if `validate_file_path` accepts
            // the lexical form, canonicalize and confirm the resolved path
            // sits inside `out_dir`. Guards against any future bypass and
            // catches symlink-rewritten ancestors. WARN+skip rather than
            // hard-erroring so one bad path can't fail the whole wave.
            if safe_join(out_dir, &file.path).is_none() {
                tracing::warn!(
                    path = %file.path,
                    out_dir = %out_dir.display(),
                    "wave-loop: rejecting path that escapes out_dir; skipping file"
                );
                continue;
            }
            let max_lines = file.max_lines.unwrap_or(300);
            let deps = if file.depends_on.is_empty() {
                "none".to_string()
            } else {
                file.depends_on
                    .iter()
                    .map(|d| format!("`{d}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            // #222: When code_dir != artifacts_dir, stubs live in
            // artifacts_dir/stubs/ but the agent's CWD is code_dir. Pass an
            // ABSOLUTE stub path so the engineer can find it regardless of
            // its working directory.
            let stub_instruction = match &file.stub {
                Some(stub) => {
                    let abs_stub = artifacts_dir.join("stubs").join(stub);
                    format!(
                        "1. Read your stub: `{abs_stub}` (ABSOLUTE PATH) — implement it exactly, do not change signatures",
                        abs_stub = abs_stub.display()
                    )
                }
                None => format!(
                    "1. No stub for this file — implement based on purpose: {}",
                    file.purpose
                ),
            };
            // #166: Compute absolute path for the write step. The claude CLI
            // subprocess anchors relative `write_file` calls to the git
            // repository root, so a relative path like
            // `multi_repo_analyzer/pyproject.toml` would land at the repo
            // root instead of out_dir. We keep the relative `file.path` for
            // reading stubs and dependencies (those are relative to the
            // working_dir/out_dir), but instruct the agent to use the
            // absolute path for the write_file call.
            let abs_path = out_dir.join(&file.path);
            let abs_path_str = abs_path.to_string_lossy();
            let task = format!(
                "You are responsible for ONE file: `{path}`\n\
                 Purpose: {purpose}\n\
                 Max lines: {max_lines}\n\
                 Error convention: {error_convention}\n\n\
                 Steps:\n\
                 {stub_instruction}\n\
                 2. Read each dependency for context: {deps}\n\
                 3. Add a one-line intent comment above each function: # INTENT: <what it does, not how>\n\
                 4. Write full implementation to `{abs_path}` via write_file (ABSOLUTE PATH, not relative).\n\
                    IMPORTANT: Use the ABSOLUTE path `{abs_path}` when calling write_file.\n\
                    Do NOT use the relative path `{path}` for writing — it will land in the wrong directory.\n\
                 5. Run your tests if possible: the test file for this module (if it exists in stubs/)\n\
                 6. Call finish_task when done\n",
                path = file.path,
                abs_path = abs_path_str,
                purpose = file.purpose,
                max_lines = max_lines,
                error_convention = error_convention,
                stub_instruction = stub_instruction,
                deps = deps,
            );

            // CRIT-1 / MAJ-1 (#90, #93): Per-file overrides travel through
            // the runner trait as a `RunContext`; the runner applies them to
            // the child process only (never mutating the parent env). The
            // working_dir=out_dir ensures claude-code writes land directly in
            // out_dir instead of the parent's cwd.
            let ctx = RunContext {
                assigned_file: Some(std::path::PathBuf::from(&file.path)),
                max_turns_override: Some(40),
                working_dir: Some(out_dir.to_path_buf()),
                // #107: Thread the phase-level model override through each
                // per-file wave-loop invocation so the code phase's
                // `model` reaches the claude-code runner.
                model: phase.model.clone(),
            };

            let result = run_wave_file_with_retry(
                runner.as_ref(),
                &phase.agent,
                &task,
                &ctx,
                elevation_threshold,
                elevation_model.as_deref(),
            )
            .await;

            let output = match result {
                Ok(o) => o,
                Err(e) => {
                    // MAJ-2 (#94): Emit the partial combined output before
                    // propagating so operators can see which files succeeded.
                    tracing::warn!(
                        path = %file.path,
                        wave = wave.wave,
                        error = %e,
                        completed_files = written_paths.len(),
                        partial_chars = combined.len(),
                        "wave-loop: per-file agent failed; partial output preserved in logs"
                    );
                    if !combined.is_empty() {
                        tracing::info!(
                            partial_content = %combined,
                            "wave-loop: partial combined output at point of failure"
                        );
                    }
                    return Err(e.context(format!(
                        "wave-loop: per-file agent failed for {} (completed: {} files)",
                        file.path,
                        written_paths.len()
                    )));
                }
            };

            info!(
                path = %file.path,
                wave = wave.wave,
                prompt_tokens = output.usage.prompt_tokens,
                completion_tokens = output.usage.completion_tokens,
                "wave-loop file complete"
            );

            agg_usage.add(&output.usage);
            combined.push_str(&format!(
                "## File: {path} (wave {wave})\n\n",
                path = file.path,
                wave = wave.wave
            ));
            combined.push_str(&output.content);
            combined.push_str("\n\n");

            // #114: The agent must have written the file to out_dir. With
            // working_dir=out_dir set on the runner, cwd-fallback is no
            // longer necessary — either the file is there or the agent
            // misbehaved. Previously this was a warning; escalating to a
            // hard error prevents later waves from running against missing
            // dependencies (adopted from self-review patch 03).
            let dest = out_dir.join(&file.path);
            if dest.exists() {
                written_paths.push(file.path.clone());
            } else {
                return Err(anyhow::anyhow!(
                    "wave-loop: agent for '{}' completed successfully but did not write \
                     the assigned file to disk at '{}'. {} prior file(s) were written \
                     successfully. This is treated as a hard error because later waves \
                     may depend on this file.",
                    file.path,
                    dest.display(),
                    written_paths.len(),
                ));
            }
        }
        // #149: Stream wave-done after all files in this wave have been written.
        if let Some(rep) = &progress {
            rep.wave_done(wave.wave as usize, wave_total, wave_started.elapsed());
        }
    }

    // #228: Clean up plan-agent scaffolding stubs after code phase completes.
    // The stubs/ directory was used as implementation hints during the wave
    // loop. Leaving it on disk causes recursive test runners (e.g. `go test ./...`,
    // `pytest --rootdir`) to pick up unfilled stub functions and fail.
    let stubs_dir = artifacts_dir.join("stubs");
    if stubs_dir.exists() {
        match tokio::fs::remove_dir_all(&stubs_dir).await {
            Ok(_) => tracing::info!(
                path = %stubs_dir.display(),
                "wave-loop: removed stubs scaffolding dir after code phase"
            ),
            Err(e) => tracing::warn!(
                path = %stubs_dir.display(),
                error = %e,
                "wave-loop: could not remove stubs dir (non-fatal)"
            ),
        }
    }

    let summary = format!(
        "wave-loop complete: {} files across {} waves ({} written)",
        assignments
            .waves
            .iter()
            .map(|w| w.files.len())
            .sum::<usize>(),
        assignments.waves.len(),
        written_paths.len()
    );

    Ok(AgentOutput {
        content: combined,
        summary: Some(summary),
        usage: agg_usage,
    })
}

pub(crate) fn should_precreate(file: &FileAssignment) -> bool {
    let name = std::path::Path::new(&file.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if name == "__init__.py" {
        return true;
    }
    // Plan-agent placeholder pattern: `stub: null` with empty purpose.
    if file.stub.is_none() && file.purpose.trim().is_empty() {
        return true;
    }
    false
}

/// #150: Pre-create empty package-marker files referenced by the assignments
/// plan so the wave loop's post-dispatch presence check passes regardless of
/// whether the engineer agent chose to write them.
///
/// Why: See `should_precreate`. The hard-error on missing files was observed
/// in L2 bake-off build #166 when an agent skipped an empty `__init__.py`.
/// What: Iterates all wave files; for each one `should_precreate` accepts,
/// creates parent directories and writes an empty file if it does not yet
/// exist. Does not overwrite existing content.
/// Test: `precreate_package_markers_creates_init_py`.
pub(crate) async fn precreate_package_markers(
    assignments: &Assignments,
    out_dir: &std::path::Path,
) -> anyhow::Result<()> {
    for wave in &assignments.waves {
        for file in &wave.files {
            if !should_precreate(file) {
                continue;
            }
            // Defense-in-depth: reject unsafe paths here too, though
            // `Assignments::load` + `run_wave_loop` already validated.
            if Assignments::validate_file_path(&file.path).is_err() {
                continue;
            }
            let full_path = out_dir.join(&file.path);
            if full_path.exists() {
                continue;
            }
            if let Some(parent) = full_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&full_path, b"").await?;
            tracing::debug!(
                path = %file.path,
                "wave-loop: pre-created empty package marker"
            );
        }
    }
    Ok(())
}
