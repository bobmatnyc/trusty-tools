//! Integration tests for phase-checkpoint durability + `tagent resume`
//! (#3062, SPEC-AGENTFW-02 §3).
//!
//! Why: Unit tests in `checkpoint_tests.rs` cover the journal's serde/I-O
//! contract in isolation. These tests drive the REAL `WorkflowEngine` phase
//! loop end-to-end through a simulated crash (a mock agent that fails on its
//! first call, standing in for "the process was killed mid-phase" without
//! literally `SIGKILL`ing the test process) and assert that resuming does
//! not re-dispatch any already-completed phase, and that the run completes.
//! What: `FlakyOnceMock` counts calls per agent name and fails the first
//! call to one designated agent. `resume_skips_completed_phases_and_reuses_their_output`
//! is the primary conformance test (SPEC-AGENTFW-02 §3.6: "re-runs phase 1
//! ... and does not re-invoke the phase-0 agent"). The remaining tests cover
//! the §3.5 failure-matrix fail-closed paths.
//! Test: This file IS the test suite.

use std::collections::HashMap;

use super::*;

use crate::workflow::ResumeOutcome;
use crate::workflow::engine::checkpoint::{self, CheckpointRecord, RunState};
use crate::workflow::error::WorkflowError;

/// Mock runner that counts calls per agent name and fails ONLY the first
/// call to `fail_once_agent` — every other call (including retries of that
/// same agent) succeeds. Stands in for "the process crashed mid-phase and a
/// resume re-dispatches that phase" without an actual process kill.
struct FlakyOnceMock {
    call_counts: Arc<Mutex<HashMap<String, u32>>>,
    fail_once_agent: String,
}

#[async_trait]
impl AgentRunner for FlakyOnceMock {
    async fn run(&self, agent_name: &str, _task: &str) -> Result<AgentOutput> {
        let call_number = {
            let mut counts = self.call_counts.lock().unwrap();
            let entry = counts.entry(agent_name.to_string()).or_insert(0);
            *entry += 1;
            *entry
        };
        if agent_name == self.fail_once_agent && call_number == 1 {
            anyhow::bail!("simulated crash: {agent_name} failed on its first invocation");
        }
        Ok(AgentOutput {
            content: format!("{agent_name} output (call #{call_number})"),
            summary: Some(format!("{agent_name} summary")),
            usage: TokenUsage::default(),
        })
    }
}

/// RAII guard that sets a process env var for the duration of a test and
/// restores its previous value (or removes it) on drop — including on
/// panic/early-return. Every test in this file is `#[serial]` (env vars are
/// process-global) matching the existing convention in `env_compat.rs` /
/// `llm/credentials.rs`.
struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: serialized against every other `#[serial]` env-mutating
        // test in this crate (see `env_compat.rs` for the established
        // pattern this mirrors).
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// Write the shared 3-phase test workflow (`phase-a` -> `phase-b` ->
/// `phase-c`) used by every test in this file.
fn write_three_phase_workflow(workflows_dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(workflows_dir).unwrap();
    let path = workflows_dir.join(format!("{name}.json"));
    let json = format!(
        r#"{{
            "name": "{name}",
            "description": "checkpoint/resume integration test",
            "phases": [
                {{"name": "phase-a", "agent": "agent-a", "context_template": "{{{{task}}}}"}},
                {{"name": "phase-b", "agent": "agent-b", "context_template": "{{{{task}}}} / {{{{phase-a}}}}"}},
                {{"name": "phase-c", "agent": "agent-c", "context_template": "{{{{task}}}} / {{{{phase-b}}}}"}}
            ]
        }}"#
    );
    std::fs::write(&path, json).unwrap();
    path
}

/// The primary conformance test (SPEC-AGENTFW-02 §3.6): a 3-phase workflow's
/// middle phase (`phase-b`) fails on its first dispatch — simulating a crash
/// after `phase-a` completed. The run returns `Err` and leaves a `Failed{1}`
/// checkpoint. `tagent resume` (via `resume_with_perf_and_dirs`) then
/// completes the run: `phase-a`'s agent is called exactly once (never
/// re-dispatched), `phase-b`'s agent is called twice (the failed attempt +
/// the successful retry), `phase-c` runs once, and the final context
/// contains all three phase outputs. The checkpoint directory is deleted
/// once the resumed run reaches `Done`.
#[tokio::test]
#[serial_test::serial]
async fn resume_skips_completed_phases_and_reuses_their_output() {
    let tmp = tempfile::tempdir().unwrap();
    let _project_guard = EnvVarGuard::set("TAGENT_PROJECT_DIR", &tmp.path().to_string_lossy());
    let run_id = format!("test-run-{}", uuid::Uuid::new_v4());
    let _run_id_guard = EnvVarGuard::set("TAGENT_RUN_ID", &run_id);

    let workflows_dir = tmp.path().join("workflows");
    write_three_phase_workflow(&workflows_dir, "checkpoint-resume-test");
    let out_dir = tmp.path().join("out");

    let call_counts: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));
    let mock = Arc::new(FlakyOnceMock {
        call_counts: call_counts.clone(),
        fail_once_agent: "agent-b".to_string(),
    });
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());

    // --- "crash" run: phase-a succeeds, phase-b fails, phase-c never runs.
    let first_result = engine
        .run(
            "checkpoint-resume-test",
            "do the thing".into(),
            Some(out_dir.clone()),
        )
        .await;
    assert!(
        first_result.is_err(),
        "expected the simulated phase-b crash to propagate as Err"
    );
    {
        let counts = call_counts.lock().unwrap();
        assert_eq!(counts.get("agent-a").copied(), Some(1));
        assert_eq!(counts.get("agent-b").copied(), Some(1));
        assert_eq!(counts.get("agent-c"), None, "phase-c must not run yet");
    }

    // Checkpoint must reflect phase-a complete, phase-b failed (index 1).
    let project_dir = crate::usage::project_dir();
    let record = checkpoint::load_checkpoint(&project_dir, &run_id)
        .expect("a checkpoint must exist after the simulated crash");
    assert_eq!(record.state, RunState::Failed { phase_index: 1 });
    assert!(record.phase_outputs.contains_key("phase-a"));
    assert!(!record.phase_outputs.contains_key("phase-b"));

    // --- resume: phase-a must NOT be re-dispatched; phase-b retries and
    // succeeds; phase-c runs for the first time; the run completes.
    let outcome = engine
        .resume_with_perf_and_dirs(&run_id)
        .await
        .expect("resume should succeed");
    let (ctx, resumed_at_phase) = match outcome {
        ResumeOutcome::Resumed {
            ctx,
            resumed_at_phase,
            ..
        } => (ctx, resumed_at_phase),
        ResumeOutcome::AlreadyDone { .. } => panic!("expected Resumed, got AlreadyDone"),
    };
    assert_eq!(resumed_at_phase, "phase-b");

    {
        let counts = call_counts.lock().unwrap();
        assert_eq!(
            counts.get("agent-a").copied(),
            Some(1),
            "phase-a must NOT be re-executed on resume"
        );
        assert_eq!(
            counts.get("agent-b").copied(),
            Some(2),
            "phase-b's failed attempt + successful retry"
        );
        assert_eq!(counts.get("agent-c").copied(), Some(1));
    }

    assert!(ctx.phase_outputs.contains_key("phase-a"));
    assert!(ctx.phase_outputs.contains_key("phase-b"));
    assert!(ctx.phase_outputs.contains_key("phase-c"));

    // A successfully-completed (Done) run deletes its checkpoint directory.
    assert!(
        checkpoint::load_checkpoint(&project_dir, &run_id).is_err(),
        "checkpoint must be deleted after the resumed run completes"
    );
}

/// §3.5 failure matrix: if the workflow JSON's phase list changed since the
/// checkpoint was written, resume MUST fail closed with
/// `ResumeDefinitionChanged` rather than silently running a different
/// pipeline than the one that was checkpointed.
#[tokio::test]
#[serial_test::serial]
async fn resume_fails_closed_on_definition_change() {
    let tmp = tempfile::tempdir().unwrap();
    let _project_guard = EnvVarGuard::set("TAGENT_PROJECT_DIR", &tmp.path().to_string_lossy());
    let run_id = format!("test-run-{}", uuid::Uuid::new_v4());

    let workflows_dir = tmp.path().join("workflows");
    write_three_phase_workflow(&workflows_dir, "definition-changed-test");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    // Hand-write a checkpoint claiming phase-a is complete.
    let mut phase_outputs = std::collections::BTreeMap::new();
    phase_outputs.insert("phase-a".to_string(), "a output".to_string());
    let record = CheckpointRecord {
        schema_version: checkpoint::CHECKPOINT_SCHEMA_VERSION,
        run_id: run_id.clone(),
        workflow: "definition-changed-test".to_string(),
        state: RunState::PhaseComplete { phase_index: 0 },
        phase_names: vec![
            "phase-a".to_string(),
            "phase-b".to_string(),
            "phase-c".to_string(),
        ],
        out_dir: out_dir.clone(),
        code_dir: out_dir.clone(),
        task: "do the thing".to_string(),
        phase_outputs,
        goal_block: None,
        qa_retry_count: 0,
        qa_failure_feedback: None,
        started_at: "2026-07-19T00:00:00Z".to_string(),
        updated_at: "2026-07-19T00:00:01Z".to_string(),
    };
    record.write(tmp.path()).unwrap();

    // Now mutate the on-disk workflow JSON to remove a phase — the
    // definition on disk no longer matches what was checkpointed.
    let path = workflows_dir.join("definition-changed-test.json");
    std::fs::write(
        &path,
        r#"{
            "name": "definition-changed-test",
            "phases": [
                {"name": "phase-a", "agent": "agent-a", "context_template": "{{task}}"}
            ]
        }"#,
    )
    .unwrap();

    let call_counts: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));
    let mock = Arc::new(FlakyOnceMock {
        call_counts,
        fail_once_agent: String::new(),
    });
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());

    let err = engine
        .resume_with_perf_and_dirs(&run_id)
        .await
        .expect_err("resume must fail closed when the phase list changed");
    assert!(
        matches!(err, WorkflowError::ResumeDefinitionChanged { .. }),
        "expected ResumeDefinitionChanged, got {err:?}"
    );
}

/// §3.4 requirement: "If the run finished, say so." A checkpoint whose state
/// is (unexpectedly) `Done` must report `AlreadyDone` instead of attempting
/// to re-run anything.
#[tokio::test]
#[serial_test::serial]
async fn resume_reports_already_done_for_done_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let _project_guard = EnvVarGuard::set("TAGENT_PROJECT_DIR", &tmp.path().to_string_lossy());
    let run_id = format!("test-run-{}", uuid::Uuid::new_v4());

    let workflows_dir = tmp.path().join("workflows");
    write_three_phase_workflow(&workflows_dir, "already-done-test");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let record = CheckpointRecord {
        schema_version: checkpoint::CHECKPOINT_SCHEMA_VERSION,
        run_id: run_id.clone(),
        workflow: "already-done-test".to_string(),
        state: RunState::Done,
        phase_names: vec![
            "phase-a".to_string(),
            "phase-b".to_string(),
            "phase-c".to_string(),
        ],
        out_dir: out_dir.clone(),
        code_dir: out_dir.clone(),
        task: "do the thing".to_string(),
        phase_outputs: std::collections::BTreeMap::new(),
        goal_block: None,
        qa_retry_count: 0,
        qa_failure_feedback: None,
        started_at: "2026-07-19T00:00:00Z".to_string(),
        updated_at: "2026-07-19T00:00:01Z".to_string(),
    };
    record.write(tmp.path()).unwrap();

    let call_counts: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));
    let mock = Arc::new(FlakyOnceMock {
        call_counts,
        fail_once_agent: String::new(),
    });
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());

    let outcome = engine
        .resume_with_perf_and_dirs(&run_id)
        .await
        .expect("resume of a Done checkpoint must not error");
    assert!(matches!(outcome, ResumeOutcome::AlreadyDone { .. }));
}

/// §3.5 failure matrix: resuming a run id with no checkpoint on disk fails
/// closed with `CheckpointNotFound`, not a panic.
#[tokio::test]
#[serial_test::serial]
async fn resume_missing_checkpoint_returns_clear_error() {
    let tmp = tempfile::tempdir().unwrap();
    let _project_guard = EnvVarGuard::set("TAGENT_PROJECT_DIR", &tmp.path().to_string_lossy());

    let workflows_dir = tmp.path().join("workflows");
    write_three_phase_workflow(&workflows_dir, "missing-checkpoint-test");

    let call_counts: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));
    let mock = Arc::new(FlakyOnceMock {
        call_counts,
        fail_once_agent: String::new(),
    });
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());

    let err = engine
        .resume_with_perf_and_dirs("never-existed")
        .await
        .expect_err("resuming an unknown run id must fail closed");
    assert!(
        matches!(err, WorkflowError::CheckpointNotFound { .. }),
        "expected CheckpointNotFound, got {err:?}"
    );
}
