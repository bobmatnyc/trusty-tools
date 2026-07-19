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
use std::path::{Path, PathBuf};

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
pub const CHECKPOINT_SCHEMA_VERSION: u32 = 1;

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
/// been recorded into the context; `Retrying{i,attempt}` for the existing
/// one-shot QA-triggered code-phase retry; `Failed{i}` when phase `i`'s
/// dispatch returned an error; `Done` once the whole run finished
/// successfully. `Failed` and `Done` are terminal for the journal — `Done`
/// causes the journal directory to be deleted (see `delete_run_dir`).
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
/// What: `phase_outputs` holds the full content of every completed phase,
/// keyed by phase name (mirrors `WorkflowContext::phase_outputs`, not the
/// summary map — resuming re-derives summaries as `content.clone()`, a
/// documented simplification versus the original run's extracted summary).
/// `phase_names` is a diagnostic snapshot of `def.phases[..].name` taken at
/// `Pending`; `tagent resume` always reloads the workflow definition fresh
/// and validates it against this snapshot rather than trusting it directly
/// (§3.4).
/// Test: `checkpoint_record_round_trips_through_json`.
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

/// Best-effort removal of `.trusty-agents/state/runs/<run_id>/` on successful
/// (`RunState::Done`) completion (SPEC-AGENTFW-02 §3.3 "Deletion").
///
/// Why: A completed run's journal has no further durability value — keeping
/// it around forever would leak disk over many runs. A `Failed` checkpoint is
/// intentionally NOT deleted by this function (it's the resumable artifact);
/// callers only invoke this once the run reaches `Done`.
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

#[cfg(test)]
mod checkpoint_tests;
