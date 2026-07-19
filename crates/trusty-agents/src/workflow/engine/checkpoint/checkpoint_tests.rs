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
    let mut phase_summaries = BTreeMap::new();
    phase_summaries.insert("research".to_string(), "research summary".to_string());
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
        phase_summaries,
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
    // Code-critic finding (PR #3244): phase_summaries must round-trip
    // separately from phase_outputs — this is what resume replays into
    // downstream `{{phase_name}}` template substitutions.
    assert_eq!(
        back.phase_summaries.get("research"),
        Some(&"research summary".to_string())
    );
    assert_eq!(back.schema_version, CHECKPOINT_SCHEMA_VERSION);
}

/// Code-critic finding (PR #3244): a v1 journal (schema_version 1, written
/// before `phase_summaries` existed) must fail closed with a CLEAR
/// "incompatible schema" message via `CheckpointSchemaMismatch` — not
/// silently deserialize with an empty `phase_summaries` map and let a
/// resumed run replay full raw content into downstream prompts.
#[test]
fn v1_journal_without_phase_summaries_fails_closed_via_schema_mismatch() {
    let tmp = TempDir::new().unwrap();
    // Hand-write a v1-shaped journal: schema_version 1, no phase_summaries
    // key at all (simulating what PR #3244's original implementation wrote
    // before this fix).
    let v1_json = r#"{
        "schema_version": 1,
        "run_id": "run-v1",
        "workflow": "prescriptive",
        "state": {"phase_complete": {"phase_index": 0}},
        "phase_names": ["research", "plan"],
        "out_dir": "/tmp/out",
        "code_dir": "/tmp/code",
        "task": "do the thing",
        "phase_outputs": {"research": "research output"},
        "goal_block": null,
        "qa_retry_count": 0,
        "qa_failure_feedback": null,
        "started_at": "2026-07-19T00:00:00Z",
        "updated_at": "2026-07-19T00:01:00Z"
    }"#;
    let dir = run_dir(tmp.path(), "run-v1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("checkpoint.json"), v1_json).unwrap();

    let err = load_checkpoint(tmp.path(), "run-v1").unwrap_err();
    match err {
        WorkflowError::CheckpointSchemaMismatch {
            found, expected, ..
        } => {
            assert_eq!(found, 1);
            assert_eq!(expected, CHECKPOINT_SCHEMA_VERSION);
        }
        other => panic!("expected CheckpointSchemaMismatch, got {other:?}"),
    }
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

/// Code-critic finding (PR #3244 MEDIUM): `finalize_run`'s pre-delete step —
/// an existing `PhaseComplete` checkpoint is flipped to `Done` in place,
/// with `updated_at` refreshed, and everything else preserved.
#[test]
fn mark_done_before_delete_transitions_existing_checkpoint() {
    let tmp = TempDir::new().unwrap();
    let record = sample_record("run-finishing");
    record.write(tmp.path()).unwrap();

    mark_done_before_delete(tmp.path(), "run-finishing");

    let loaded = load_checkpoint(tmp.path(), "run-finishing").unwrap();
    assert_eq!(loaded.state, RunState::Done);
    // Everything else survives the transition untouched.
    assert_eq!(loaded.run_id, "run-finishing");
    assert_eq!(loaded.phase_outputs, record.phase_outputs);
    assert_eq!(loaded.phase_summaries, record.phase_summaries);
}

/// `mark_done_before_delete` on a run id with no checkpoint at all is a
/// silent no-op — the workflow already succeeded, and there is nothing safe
/// to "flip"; this must never fail the run over journal bookkeeping.
#[test]
fn mark_done_before_delete_is_noop_when_absent() {
    let tmp = TempDir::new().unwrap();
    mark_done_before_delete(tmp.path(), "never-existed");
    // Reaching here without panicking is the assertion; nothing was created.
    assert!(!checkpoint_path(tmp.path(), "never-existed").exists());
}

/// Code-critic finding (PR #3244 MEDIUM): `acquire_resume_lock` fails closed
/// immediately (non-blocking) when another holder already has the lock for
/// the same run id — two concurrent `tagent resume` calls on the same run
/// must never both proceed.
#[test]
fn resume_lock_blocks_concurrent_acquisition() {
    let tmp = TempDir::new().unwrap();
    let _first =
        acquire_resume_lock(tmp.path(), "run-locked").expect("first acquisition must succeed");

    let second = acquire_resume_lock(tmp.path(), "run-locked");
    assert!(
        matches!(second, Err(WorkflowError::ResumeAlreadyInProgress { .. })),
        "expected ResumeAlreadyInProgress while the first guard is held, got {second:?}"
    );
}

/// Dropping the lock guard releases the OS-level advisory lock, so a
/// subsequent acquisition (e.g. a retried resume after the first one
/// finished) succeeds.
#[test]
fn resume_lock_released_on_drop() {
    let tmp = TempDir::new().unwrap();
    {
        let _guard = acquire_resume_lock(tmp.path(), "run-reacquire").expect("first acquisition");
        // Guard drops at the end of this block.
    }
    let reacquired = acquire_resume_lock(tmp.path(), "run-reacquire");
    assert!(
        reacquired.is_ok(),
        "expected re-acquisition to succeed after the first guard dropped: {reacquired:?}"
    );
}

/// Two DIFFERENT run ids never contend for the same lock — the lock is
/// scoped to `<run_dir>/resume.lock`, per run id.
#[test]
fn resume_lock_is_scoped_per_run_id() {
    let tmp = TempDir::new().unwrap();
    let _a = acquire_resume_lock(tmp.path(), "run-a").expect("run-a lock");
    let b = acquire_resume_lock(tmp.path(), "run-b");
    assert!(b.is_ok(), "distinct run ids must not contend: {b:?}");
}
