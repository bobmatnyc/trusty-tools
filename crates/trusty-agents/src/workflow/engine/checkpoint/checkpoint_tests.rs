//! Unit tests for the checkpoint journal (#3062).
//!
//! Why: The journal is the entire durability contract — serde round-trips,
//! schema-version rejection, and corrupt/missing-file handling must be locked
//! down so a future refactor can't silently break crash recovery.
//! What: Exercises `RunState`/`CheckpointRecord` JSON round-trips, `write` +
//! `load_checkpoint` end-to-end via `atomic_write`, the three fail-closed
//! paths (missing, corrupt, schema mismatch), and `list_resumable_runs` /
//! `delete_run_dir`.
//! Test: This file IS the test suite for `checkpoint.rs`.

use std::collections::BTreeMap;

use tempfile::TempDir;

use super::*;

fn sample_record(run_id: &str) -> CheckpointRecord {
    let mut phase_outputs = BTreeMap::new();
    phase_outputs.insert("research".to_string(), "research output".to_string());
    CheckpointRecord {
        schema_version: CHECKPOINT_SCHEMA_VERSION,
        run_id: run_id.to_string(),
        workflow: "prescriptive".to_string(),
        state: RunState::PhaseComplete { phase_index: 0 },
        phase_names: vec!["research".to_string(), "plan".to_string()],
        out_dir: "/tmp/out".into(),
        code_dir: "/tmp/code".into(),
        task: "[hacker] do the thing".to_string(),
        phase_outputs,
        goal_block: None,
        qa_retry_count: 0,
        qa_failure_feedback: None,
        started_at: "2026-07-19T00:00:00Z".to_string(),
        updated_at: "2026-07-19T00:01:00Z".to_string(),
    }
}

/// `RunState` round-trips through JSON for every variant, including the
/// struct-carrying ones — this is the wire format `tagent resume` depends on.
#[test]
fn run_state_round_trips_through_json() {
    let states = [
        RunState::Pending,
        RunState::PhaseRunning { phase_index: 2 },
        RunState::PhaseComplete { phase_index: 2 },
        RunState::Retrying {
            phase_index: 2,
            attempt: 1,
        },
        RunState::Failed { phase_index: 2 },
        RunState::Done,
    ];
    for state in states {
        let json = serde_json::to_string(&state).unwrap();
        let back: RunState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back, "round-trip mismatch for {json}");
    }
}

/// A full `CheckpointRecord` (including a populated `phase_outputs` map)
/// round-trips byte-for-byte-equivalent through JSON.
#[test]
fn checkpoint_record_round_trips_through_json() {
    let record = sample_record("run-abc");
    let json = serde_json::to_string_pretty(&record).unwrap();
    let back: CheckpointRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(back.run_id, "run-abc");
    assert_eq!(back.workflow, "prescriptive");
    assert_eq!(back.state, RunState::PhaseComplete { phase_index: 0 });
    assert_eq!(
        back.phase_outputs.get("research"),
        Some(&"research output".to_string())
    );
    assert_eq!(back.schema_version, CHECKPOINT_SCHEMA_VERSION);
}

/// `run_dir` / `checkpoint_path` join `project_dir` / `.trusty-agents/state/
/// runs/<run_id>/checkpoint.json` exactly — this is the on-disk location
/// SPEC-AGENTFW-02 §3.3 mandates.
#[test]
fn run_dir_and_checkpoint_path_join_correctly() {
    let project = std::path::Path::new("/proj");
    let dir = run_dir(project, "run-1");
    assert_eq!(
        dir,
        std::path::PathBuf::from("/proj/.trusty-agents/state/runs/run-1")
    );
    let path = checkpoint_path(project, "run-1");
    assert_eq!(
        path,
        std::path::PathBuf::from("/proj/.trusty-agents/state/runs/run-1/checkpoint.json")
    );
}

/// `write` followed by `load_checkpoint` returns an equivalent record — the
/// core write-then-resume contract, going through the real `atomic_write`
/// primitive (lock + tmp + rename), not a mock.
#[test]
fn write_then_load_round_trips() {
    let tmp = TempDir::new().unwrap();
    let record = sample_record("run-xyz");
    record.write(tmp.path()).unwrap();

    let loaded = load_checkpoint(tmp.path(), "run-xyz").unwrap();
    assert_eq!(loaded.run_id, "run-xyz");
    assert_eq!(loaded.state, record.state);
    assert_eq!(loaded.phase_outputs, record.phase_outputs);
}

/// Loading a run id with no checkpoint on disk fails closed with
/// `CheckpointNotFound`, listing the run ids that DO resolve.
#[test]
fn load_checkpoint_missing_lists_available_runs() {
    let tmp = TempDir::new().unwrap();
    sample_record("run-present").write(tmp.path()).unwrap();

    let err = load_checkpoint(tmp.path(), "run-absent").unwrap_err();
    match err {
        WorkflowError::CheckpointNotFound { run_id, available } => {
            assert_eq!(run_id, "run-absent");
            assert_eq!(available, vec!["run-present".to_string()]);
        }
        other => panic!("expected CheckpointNotFound, got {other:?}"),
    }
}

/// A hand-truncated / non-JSON checkpoint file fails closed with
/// `CheckpointCorrupt`, never a panic and never a silent fresh-run fallback.
#[test]
fn load_checkpoint_rejects_corrupt_json() {
    let tmp = TempDir::new().unwrap();
    let dir = run_dir(tmp.path(), "run-bad");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("checkpoint.json"), b"{ not valid json").unwrap();

    let err = load_checkpoint(tmp.path(), "run-bad").unwrap_err();
    assert!(
        matches!(err, WorkflowError::CheckpointCorrupt { .. }),
        "expected CheckpointCorrupt, got {err:?}"
    );
}

/// A checkpoint whose `schema_version` doesn't match the running binary's
/// `CHECKPOINT_SCHEMA_VERSION` fails closed with `CheckpointSchemaMismatch`
/// rather than silently reinterpreting an incompatible journal.
#[test]
fn schema_version_mismatch_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let mut record = sample_record("run-old");
    record.schema_version = CHECKPOINT_SCHEMA_VERSION + 99;
    record.write(tmp.path()).unwrap();

    let err = load_checkpoint(tmp.path(), "run-old").unwrap_err();
    match err {
        WorkflowError::CheckpointSchemaMismatch {
            found, expected, ..
        } => {
            assert_eq!(found, CHECKPOINT_SCHEMA_VERSION + 99);
            assert_eq!(expected, CHECKPOINT_SCHEMA_VERSION);
        }
        other => panic!("expected CheckpointSchemaMismatch, got {other:?}"),
    }
}

/// `delete_run_dir` removes the whole `runs/<run_id>/` directory (matching
/// §3.3 "Deletion": a `Done` run leaves nothing behind).
#[test]
fn delete_run_dir_removes_directory() {
    let tmp = TempDir::new().unwrap();
    sample_record("run-done").write(tmp.path()).unwrap();
    assert!(checkpoint_path(tmp.path(), "run-done").exists());

    delete_run_dir(tmp.path(), "run-done");
    assert!(!run_dir(tmp.path(), "run-done").exists());
}

/// Deleting a run dir that was never created (or already removed) is a
/// silent no-op — never panics, never errors the caller.
#[test]
fn delete_run_dir_is_noop_when_absent() {
    let tmp = TempDir::new().unwrap();
    delete_run_dir(tmp.path(), "never-existed");
    // Reaching here without panicking is the assertion.
}

/// `list_resumable_runs` only returns run ids that actually have a
/// `checkpoint.json` — a stray empty directory under `runs/` doesn't count.
#[test]
fn list_resumable_runs_finds_only_dirs_with_checkpoint() {
    let tmp = TempDir::new().unwrap();
    sample_record("run-a").write(tmp.path()).unwrap();
    sample_record("run-b").write(tmp.path()).unwrap();
    // A directory with no checkpoint.json inside — must be excluded.
    std::fs::create_dir_all(run_dir(tmp.path(), "run-empty")).unwrap();

    let ids = list_resumable_runs(tmp.path());
    assert_eq!(ids, vec!["run-a".to_string(), "run-b".to_string()]);
}

/// An empty (or nonexistent) `runs/` directory yields an empty list, not an
/// error — a fresh project with no checkpoints ever written is the common
/// case, not an edge case.
#[test]
fn list_resumable_runs_empty_when_no_runs_dir() {
    let tmp = TempDir::new().unwrap();
    assert!(list_resumable_runs(tmp.path()).is_empty());
}
