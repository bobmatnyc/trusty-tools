//! Phase-checkpoint durability journal for the workflow engine (#3062,
//! SPEC-AGENTFW-02 / `docs/specs/trusty-agents-eve-style-agents-spec.md` §3).
//!
//! Why: Before this module, the engine held zero on-disk state during an
//! in-flight run — `perf.flush` only wrote once, at the very end. A process
//! crash mid-run lost the entire `WorkflowContext`, and there was no way to
//! continue a multi-phase run without starting over (and re-billing every
//! already-completed phase's LLM calls). §3.2/§3.3 of the spec define a
//! phase-level `RunState` machine and a `CheckpointRecord` journal written at
//! every phase boundary via the existing `state_writer::atomic_write`
//! primitive (reused verbatim, per the issue) so a resumed run can pick up
//! from the last completed phase.
//! What: `RunState` — the phase-boundary state machine; `CheckpointRecord` —
//! the on-disk journal shape (one file per run id at
//! `.trusty-agents/state/runs/<run_id>/checkpoint.json`); `write`, load,
//! delete, and list helpers, all going through `atomic_write` for writes and
//! plain reads for loads (matching the existing `.trusty-agents/state/`
//! convention documented in `build_info.rs`/`mistake_log.rs`).
//! Test: `checkpoint_tests` (round-trip serde, schema-version rejection,
//! corrupt-file handling, list/delete behavior).

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use fs4::FileExt;
use serde::{Deserialize, Serialize};

use crate::context::GoalBlock;
use crate::state_writer::atomic_write;
use crate::workflow::error::WorkflowError;

/// Current on-disk schema version for `CheckpointRecord`.
///
/// Why: SPEC-AGENTFW-02 §3.3's `CheckpointRecord` example does not include an
/// explicit version field, but issue #3062 explicitly requires "if the
/// journal is from an incompatible/older schema, fail with a clear message
/// (version the journal format)". This is an additive deviation from the
/// spec's literal struct, documented in the PR description: a `schema_version`
/// field lets a future incompatible change to this struct fail closed
/// (`WorkflowError::CheckpointSchemaMismatch`) instead of silently
/// misinterpreting an old journal via serde defaults.
/// What: Bump this constant whenever `CheckpointRecord`'s shape changes in a
/// way that isn't safely `serde(default)`-compatible.
/// Test: `schema_version_mismatch_is_rejected`.
///
/// v2 (code-critic finding, PR #3244): added `phase_summaries` — v1 journals
/// (which lack it) now fail closed via the schema-version check below rather
/// than silently deserializing with an empty summary map, which would have
/// replayed FULL phase content into every downstream `context_template` on
/// resume (see `CheckpointRecord::phase_summaries` doc).
pub const CHECKPOINT_SCHEMA_VERSION: u32 = 2;

fn current_schema_version() -> u32 {
    CHECKPOINT_SCHEMA_VERSION
}

/// Phase-boundary run state machine (SPEC-AGENTFW-02 §3.2, reproduced
/// verbatim).
///
/// Why: A single tagged enum makes every valid mid-run state explicit and
/// exhaustively matchable, rather than inferring progress from partial
/// `phase_outputs` alone.
/// What: `Pending` before phase 0 starts; `PhaseRunning{i}` while phase `i`'s
/// agent call is in flight; `PhaseComplete{i}` once phase `i`'s output has
/// been recorded into the context; `Failed{i}` when phase `i`'s dispatch
/// returned an error; `Done` once the whole run finished successfully.
/// `Failed` and `Done` are terminal for the journal — `Done` causes the
/// journal directory to be deleted (see `delete_run_dir`).
///
/// `Retrying{i,attempt}` is a DISTINCT-BUT-EQUIVALENT sibling of
/// `PhaseRunning{i}` (code-critic finding, PR #3244 — kept as its own
/// variant rather than folded into `PhaseRunning`, for observability: an
/// operator inspecting the journal can tell "phase i is running for the
/// first time" from "phase i is running as the existing Fix 2 one-shot
/// QA-triggered code-phase retry" without cross-referencing
/// `qa_retry_count`). It always carries the SAME `phase_index` the current
/// loop iteration is on when `handle_qa_envelope` flips `qa_retry_count`
/// 0->1 — today that is always the `qa` phase's own index, since this
/// engine's loop has no phase-index jump-back (see `phase_loop.rs`).
/// `tagent resume` treats `Retrying` IDENTICALLY to `PhaseRunning` /
/// `Failed`: resume AT `phase_index`, re-dispatching that phase from
/// scratch — there is no separate "replay the retry feedback" resume path.
/// Test: `run_state_round_trips_through_json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Pending,
    PhaseRunning { phase_index: usize },
    PhaseComplete { phase_index: usize },
    Retrying { phase_index: usize, attempt: u32 },
    Failed { phase_index: usize },
    Done,
}

/// On-disk checkpoint journal for one workflow run (SPEC-AGENTFW-02 §3.3).
///
/// Why: Everything a resumed run needs to reconstruct `WorkflowContext` and
/// continue from the first incomplete phase, without re-running (and
/// re-billing) any already-completed phase.
/// What: `phase_outputs` holds the FULL content of every completed phase,
/// keyed by phase name (mirrors `WorkflowContext::phase_outputs`).
/// `phase_summaries` holds the EXTRACTED summary for the same phases
/// (mirrors `WorkflowContext::phase_summaries` — code-critic finding, PR
/// #3244 v2). This split matters: `WorkflowContext::render_template`
/// (`context.rs`) injects `phase_summaries`, not `phase_outputs`, into every
/// downstream `{{phase_name}}` template substitution — that's the entire
/// point of extracting a ~500-word digest instead of forwarding 30k raw
/// chars. A resumed run that only had `phase_outputs` to replay from would
/// have to fall back to `content.clone()` as the "summary", silently
/// re-inflating every downstream prompt for exactly the long-running,
/// content-heavy workflows checkpointing exists to protect. `phase_names` is
/// a diagnostic snapshot of `def.phases[..].name` taken at `Pending`;
/// `tagent resume` always reloads the workflow definition fresh and
/// validates it against this snapshot rather than trusting it directly
/// (§3.4).
/// Test: `checkpoint_record_round_trips_through_json`,
/// `checkpoint_resume_tests::resume_replays_summary_not_full_content_into_downstream_prompts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointRecord {
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,
    pub run_id: String,
    pub workflow: String,
    pub state: RunState,
    pub phase_names: Vec<String>,
    pub out_dir: PathBuf,
    pub code_dir: PathBuf,
    /// Original (uncleaned) task text — persona tags intact, matching what
    /// `run_with_perf_and_dirs` receives before `detect_persona` strips them.
    pub task: String,
    pub phase_outputs: BTreeMap<String, String>,
    /// Extracted per-phase summary — see the struct-level doc for why this
    /// must be persisted (and replayed) separately from `phase_outputs`.
    /// `#[serde(default)]` so a v1 journal still deserializes (as empty);
    /// the `schema_version` check in `load_checkpoint` is what actually
    /// rejects it, with a clear "incompatible schema" message rather than a
    /// raw serde error.
    #[serde(default)]
    pub phase_summaries: BTreeMap<String, String>,
    pub goal_block: Option<GoalBlock>,
    pub qa_retry_count: u32,
    pub qa_failure_feedback: Option<String>,
    pub started_at: String,
    pub updated_at: String,
}

/// `.trusty-agents/state/runs/<run_id>/` under `project_dir` — the directory
/// holding one run's `checkpoint.json` (and reserved for future per-run
/// artifacts).
///
/// Why: Matches the existing `.trusty-agents/state/` runtime-state root
/// convention (`build.json`, `sessions.json`, `mistakes/<sid>.jsonl`, …)
/// documented in `build_info.rs` / `mistake_log.rs`, scoped one level deeper
/// per run id so multiple in-flight runs never collide.
/// What: Pure path join, no I/O.
/// Test: `run_dir_and_checkpoint_path_join_correctly`.
pub fn run_dir(project_dir: &Path, run_id: &str) -> PathBuf {
    project_dir
        .join(".trusty-agents")
        .join("state")
        .join("runs")
        .join(run_id)
}

/// `<run_dir>/checkpoint.json` — the single journal file for a run.
pub fn checkpoint_path(project_dir: &Path, run_id: &str) -> PathBuf {
    run_dir(project_dir, run_id).join("checkpoint.json")
}

impl CheckpointRecord {
    /// Persist this record to `.trusty-agents/state/runs/<run_id>/checkpoint.json`
    /// under `project_dir`, atomically.
    ///
    /// Why: The write path REUSES `state_writer::atomic_write` verbatim (per
    /// the issue) — lock + tmp + rename — so a crash mid-write never leaves a
    /// half-written journal for a subsequent `tagent resume` to trip over.
    /// What: Serializes via `serde_json::to_vec_pretty` and calls
    /// `atomic_write`. Failures are returned to the caller, which (per the
    /// engine's existing "log and continue" convention for non-critical
    /// persistence side effects — perf flush, ticket hooks) logs a warning
    /// and does NOT fail the workflow run itself; see `phase_loop.rs`.
    /// Test: `write_then_load_round_trips`.
    pub fn write(&self, project_dir: &Path) -> Result<(), WorkflowError> {
        let path = checkpoint_path(project_dir, &self.run_id);
        let bytes = serde_json::to_vec_pretty(self)?;
        atomic_write(&path, &bytes).map_err(|e| {
            WorkflowError::ConfigInvalid(format!(
                "failed to write checkpoint {}: {e:#}",
                path.display()
            ))
        })
    }
}

/// Load and validate the checkpoint for `run_id` under `project_dir`.
///
/// Why: `tagent resume` must fail closed — never silently start a fresh run
/// under an existing `run_id` — on either a missing file or a corrupt/
/// schema-mismatched one (SPEC-AGENTFW-02 §3.5 failure matrix).
/// What: Missing file -> `WorkflowError::CheckpointNotFound` listing the run
/// ids that DO have a checkpoint (`list_resumable_runs`). Unparseable JSON ->
/// `WorkflowError::CheckpointCorrupt`. Parseable but a different
/// `schema_version` -> `WorkflowError::CheckpointSchemaMismatch`.
/// Test: `load_checkpoint_missing_lists_available_runs`,
/// `load_checkpoint_rejects_corrupt_json`, `schema_version_mismatch_is_rejected`.
pub fn load_checkpoint(
    project_dir: &Path,
    run_id: &str,
) -> Result<CheckpointRecord, WorkflowError> {
    let path = checkpoint_path(project_dir, run_id);
    let bytes = std::fs::read(&path).map_err(|_| WorkflowError::CheckpointNotFound {
        run_id: run_id.to_string(),
        available: list_resumable_runs(project_dir),
    })?;
    let record: CheckpointRecord =
        serde_json::from_slice(&bytes).map_err(|e| WorkflowError::CheckpointCorrupt {
            path: path.display().to_string(),
            source: anyhow::Error::new(e),
        })?;
    if record.schema_version != CHECKPOINT_SCHEMA_VERSION {
        return Err(WorkflowError::CheckpointSchemaMismatch {
            path: path.display().to_string(),
            found: record.schema_version,
            expected: CHECKPOINT_SCHEMA_VERSION,
        });
    }
    Ok(record)
}

/// Flip an existing checkpoint's `state` to `RunState::Done` and write it
/// back, best-effort. A no-op (not an error) when no checkpoint exists yet.
///
/// Why (code-critic finding, PR #3244 HIGH): `finalize_run` previously went
/// straight to `delete_run_dir` on success WITHOUT ever writing
/// `RunState::Done` to disk. If `remove_dir_all` then failed (e.g. a
/// transient permission/lock issue), the journal was left at
/// `PhaseComplete{last}` — indistinguishable from "the last phase finished
/// but the run is still in flight". A subsequent `tagent resume` would
/// happily re-enter `run_phase_loop` at `resume_index = last + 1` (an empty
/// slice, since every phase is already complete) and re-run `finalize_run`
/// a SECOND time: duplicate perf flush, duplicate auto-push, duplicate
/// `on_workflow_success` ticket comment/close. Calling this function BEFORE
/// `delete_run_dir` means a failed deletion still leaves an accurate `Done`
/// record on disk, so a subsequent resume's `RunState::Done` check
/// short-circuits to `ResumeOutcome::AlreadyDone` and fires nothing.
/// What: Loads the checkpoint (if `load_checkpoint` errors — missing,
/// corrupt, or schema-mismatched — this is a silent no-op; there is nothing
/// safe to "flip", and the workflow already succeeded so we must not fail
/// the run over journal bookkeeping), sets `state = Done` and refreshes
/// `updated_at`, then re-writes via the same `atomic_write`-backed `write`.
/// Write failures are logged at WARN and non-fatal, matching every other
/// finalize-tail side effect.
/// Test: `mark_done_before_delete_transitions_existing_checkpoint`,
/// `mark_done_before_delete_is_noop_when_absent`.
pub fn mark_done_before_delete(project_dir: &Path, run_id: &str) {
    let Ok(mut record) = load_checkpoint(project_dir, run_id) else {
        return;
    };
    record.state = RunState::Done;
    record.updated_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    if let Err(e) = record.write(project_dir) {
        tracing::warn!(
            run_id = %run_id,
            error = %e,
            "failed to write Done checkpoint before deletion (non-fatal)"
        );
    }
}

/// Best-effort removal of `.trusty-agents/state/runs/<run_id>/` on successful
/// (`RunState::Done`) completion (SPEC-AGENTFW-02 §3.3 "Deletion").
///
/// Why: A completed run's journal has no further durability value — keeping
/// it around forever would leak disk over many runs. A `Failed` checkpoint is
/// intentionally NOT deleted by this function (it's the resumable artifact);
/// callers only invoke this once the run reaches `Done`. Callers MUST call
/// `mark_done_before_delete` first (see its doc) so a failed deletion still
/// leaves an accurate terminal record.
/// What: `remove_dir_all`, logged at WARN on failure and otherwise silently
/// non-fatal — matching the existing pattern used throughout `finalize.rs`
/// for ticket-manager/auto-push hooks. Never panics, never propagates an
/// error.
/// Test: `delete_run_dir_removes_directory`,
/// `delete_run_dir_is_noop_when_absent`.
pub fn delete_run_dir(project_dir: &Path, run_id: &str) {
    let dir = run_dir(project_dir, run_id);
    if dir.exists()
        && let Err(e) = std::fs::remove_dir_all(&dir)
    {
        tracing::warn!(
            path = %dir.display(),
            error = %e,
            "failed to remove completed run's checkpoint dir (non-fatal)"
        );
    }
}

/// List run ids under `.trusty-agents/state/runs/` that currently have a
/// `checkpoint.json` — i.e. runs `tagent resume <id>` can act on.
///
/// Why: Both the "missing checkpoint" error message and `tagent resume
/// --list` need the same enumeration; centralizing it keeps them consistent.
/// What: Reads the `runs/` directory (empty `Vec` if it doesn't exist yet —
/// never an error), keeps only entries containing a `checkpoint.json`, and
/// returns the directory names (run ids) sorted for stable output.
/// Test: `list_resumable_runs_finds_only_dirs_with_checkpoint`.
pub fn list_resumable_runs(project_dir: &Path) -> Vec<String> {
    let runs_root = project_dir
        .join(".trusty-agents")
        .join("state")
        .join("runs");
    let Ok(entries) = std::fs::read_dir(&runs_root) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("checkpoint.json").exists())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    ids.sort();
    ids
}

/// Holds the exclusive advisory lock on `<run_dir>/resume.lock` for the
/// duration of one `tagent resume` call (code-critic finding, PR #3244).
///
/// Why: Two `tagent resume <run-id>` invocations racing on the same run
/// would both read the checkpoint, both start dispatching from the same
/// phase index, and both write phase-boundary checkpoints — corrupting the
/// journal's phase ordering and double-running agent calls. Unlike
/// `atomic_write`'s per-call lock (held only for the duration of one file
/// write), a resume needs the lock held for the ENTIRE resumed run.
/// What: Wraps the locked `File` handle; `Drop` releases the lock
/// (best-effort — `unlock` failures are logged, never panic) so the lock is
/// always released when the guard goes out of scope, including on an early
/// `?` return from `resume_with_perf_and_dirs`.
/// Test: `resume_lock_blocks_concurrent_acquisition`,
/// `resume_lock_released_on_drop`.
#[derive(Debug)]
pub struct ResumeLockGuard {
    file: File,
    run_id: String,
}

impl Drop for ResumeLockGuard {
    fn drop(&mut self) {
        if let Err(e) = FileExt::unlock(&self.file) {
            tracing::warn!(
                run_id = %self.run_id,
                error = %e,
                "failed to release resume lock (non-fatal; OS releases it on process exit)"
            );
        }
    }
}

/// Acquire the exclusive `resume.lock` for `run_id`, failing closed
/// immediately (non-blocking) if another process already holds it.
///
/// Why: A `tagent resume` that can't get the lock should tell the operator
/// clearly ("already being resumed") rather than hang indefinitely or,
/// worse, silently proceed and corrupt the journal.
/// What: Opens (creating if absent) `<run_dir>/resume.lock` and calls
/// `FileExt::try_lock` (the same `fs4` primitive `state_writer::atomic_write`
/// uses, non-blocking here since we want fail-closed, not fail-slow).
/// Returns `WorkflowError::ResumeAlreadyInProgress` when the lock is held by
/// someone else. The lock is released when the returned guard is dropped.
/// Test: `resume_lock_blocks_concurrent_acquisition`.
pub fn acquire_resume_lock(
    project_dir: &Path,
    run_id: &str,
) -> Result<ResumeLockGuard, WorkflowError> {
    let dir = run_dir(project_dir, run_id);
    std::fs::create_dir_all(&dir)?;
    let lock_path = dir.join("resume.lock");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    FileExt::try_lock(&file).map_err(|_| WorkflowError::ResumeAlreadyInProgress {
        run_id: run_id.to_string(),
        lock_path: lock_path.display().to_string(),
    })?;
    Ok(ResumeLockGuard {
        file,
        run_id: run_id.to_string(),
    })
}

#[cfg(test)]
mod checkpoint_tests;
