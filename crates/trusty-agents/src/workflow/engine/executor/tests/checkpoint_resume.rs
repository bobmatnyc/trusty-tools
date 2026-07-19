//! Integration tests for phase-checkpoint durability + `tagent resume`
//! (#3062, SPEC-AGENTFW-02 §3).
//!
//! Why: Unit tests in `checkpoint_tests.rs` cover the journal's serde/I-O
//! contract in isolation. These tests drive the REAL `WorkflowEngine` phase
//! loop end-to-end through a simulated crash (a mock agent that fails on its
//! first call, standing in for "the process was killed mid-phase" without
//! literally `SIGKILL`ing the test process) and assert that resuming does
//! not re-dispatch any already-completed phase, that resume replays the
//! extracted SUMMARY (not full content) into downstream prompts, that
//! concurrent resumes of the same run fail closed, and that the run
//! completes.
//! What: `FlakyOnceMock` counts calls per agent name, fails the first call
//! to one designated agent, records every rendered prompt it receives per
//! agent, and can be configured to return a custom (content, summary) pair
//! for a given agent. `resume_skips_completed_phases_and_reuses_their_output`
//! is the primary conformance test (SPEC-AGENTFW-02 §3.6: "re-runs phase 1
//! ... and does not re-invoke the phase-0 agent"). The remaining tests cover
//! the §3.5 failure-matrix fail-closed paths plus the two code-critic
//! findings from PR #3244 (summary/content conflation, concurrent-resume
//! mutual exclusion).
//! Test: This file IS the test suite.

use std::collections::HashMap;

use super::*;

use crate::workflow::ResumeOutcome;
use crate::workflow::engine::checkpoint::{self, CheckpointRecord, RunState};
use crate::workflow::error::WorkflowError;

/// Mock runner that counts calls per agent name, fails ONLY the first call
/// to `fail_once_agent` (every other call — including retries of that same
/// agent — succeeds), records every task/prompt text it receives per agent
/// (in call order, successful calls only), and returns a per-agent custom
/// `(content, summary)` pair when configured via `with_custom_output`
/// (otherwise a generic stub). Stands in for "the process crashed mid-phase
/// and a resume re-dispatches that phase" without an actual process kill.
struct FlakyOnceMock {
    call_counts: Mutex<HashMap<String, u32>>,
    fail_once_agent: String,
    captured_tasks: Mutex<HashMap<String, Vec<String>>>,
    custom_outputs: Mutex<HashMap<String, (String, Option<String>)>>,
}

impl FlakyOnceMock {
    fn new(fail_once_agent: &str) -> Self {
        Self {
            call_counts: Mutex::new(HashMap::new()),
            fail_once_agent: fail_once_agent.to_string(),
            captured_tasks: Mutex::new(HashMap::new()),
            custom_outputs: Mutex::new(HashMap::new()),
        }
    }

    /// Configure `agent_name` to return `content`/`summary` instead of the
    /// generic stub, on every (non-failing) call.
    fn with_custom_output(self, agent_name: &str, content: &str, summary: Option<&str>) -> Self {
        self.custom_outputs.lock().unwrap().insert(
            agent_name.to_string(),
            (content.to_string(), summary.map(str::to_string)),
        );
        self
    }

    fn call_count(&self, agent_name: &str) -> u32 {
        self.call_counts
            .lock()
            .unwrap()
            .get(agent_name)
            .copied()
            .unwrap_or(0)
    }

    /// The task/prompt text captured for `agent_name`'s `call_index`'th
    /// (0-based) successful call.
    fn captured_task(&self, agent_name: &str, call_index: usize) -> Option<String> {
        self.captured_tasks
            .lock()
            .unwrap()
            .get(agent_name)
            .and_then(|v| v.get(call_index))
            .cloned()
    }
}

#[async_trait]
impl AgentRunner for FlakyOnceMock {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        let call_number = {
            let mut counts = self.call_counts.lock().unwrap();
            let entry = counts.entry(agent_name.to_string()).or_insert(0);
            *entry += 1;
            *entry
        };
        if agent_name == self.fail_once_agent && call_number == 1 {
            anyhow::bail!("simulated crash: {agent_name} failed on its first invocation");
        }
        self.captured_tasks
            .lock()
            .unwrap()
            .entry(agent_name.to_string())
            .or_default()
            .push(task.to_string());

        if let Some((content, summary)) = self.custom_outputs.lock().unwrap().get(agent_name) {
            return Ok(AgentOutput {
                content: content.clone(),
                summary: summary.clone(),
                usage: TokenUsage::default(),
            });
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

/// A minimal hand-built `CheckpointRecord`, matching what `phase_loop.rs`
/// would actually write for a partially-complete run — used by tests that
/// need to inject a specific journal state without driving the engine
/// through a real crash.
#[allow(clippy::too_many_arguments)]
fn hand_written_record(
    run_id: &str,
    workflow: &str,
    state: RunState,
    phase_names: &[&str],
    out_dir: &std::path::Path,
    phase_outputs: &[(&str, &str)],
) -> CheckpointRecord {
    CheckpointRecord {
        schema_version: checkpoint::CHECKPOINT_SCHEMA_VERSION,
        run_id: run_id.to_string(),
        workflow: workflow.to_string(),
        state,
        phase_names: phase_names.iter().map(|s| s.to_string()).collect(),
        out_dir: out_dir.to_path_buf(),
        code_dir: out_dir.to_path_buf(),
        task: "do the thing".to_string(),
        phase_outputs: phase_outputs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        phase_summaries: std::collections::BTreeMap::new(),
        goal_block: None,
        qa_retry_count: 0,
        qa_failure_feedback: None,
        started_at: "2026-07-19T00:00:00Z".to_string(),
        updated_at: "2026-07-19T00:00:01Z".to_string(),
    }
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

    let mock = Arc::new(FlakyOnceMock::new("agent-b"));
    let engine = WorkflowEngine::new(mock.clone(), workflows_dir.clone());

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
    assert_eq!(mock.call_count("agent-a"), 1);
    assert_eq!(mock.call_count("agent-b"), 1);
    assert_eq!(mock.call_count("agent-c"), 0, "phase-c must not run yet");

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

    assert_eq!(
        mock.call_count("agent-a"),
        1,
        "phase-a must NOT be re-executed on resume"
    );
    assert_eq!(
        mock.call_count("agent-b"),
        2,
        "phase-b's failed attempt + successful retry"
    );
    assert_eq!(mock.call_count("agent-c"), 1);

    assert!(ctx.phase_outputs.contains_key("phase-a"));
    assert!(ctx.phase_outputs.contains_key("phase-b"));
    assert!(ctx.phase_outputs.contains_key("phase-c"));

    // A successfully-completed (Done) run deletes its checkpoint directory.
    assert!(
        checkpoint::load_checkpoint(&project_dir, &run_id).is_err(),
        "checkpoint must be deleted after the resumed run completes"
    );
}

/// CRITICAL code-critic finding (PR #3244): a resumed downstream phase's
/// rendered prompt must contain the persisted SUMMARY of an already-complete
/// phase, never its full raw content. `phase-a` is configured to return a
/// large body (a few KB, far bigger than a real summary) plus a short,
/// distinctive summary; after the simulated crash + resume, we inspect the
/// EXACT rendered task `phase-b` received on its (post-resume) successful
/// call and assert it contains the summary marker but not the full-content
/// marker.
#[tokio::test]
#[serial_test::serial]
async fn resume_replays_summary_not_full_content_into_downstream_prompts() {
    let tmp = tempfile::tempdir().unwrap();
    let _project_guard = EnvVarGuard::set("TAGENT_PROJECT_DIR", &tmp.path().to_string_lossy());
    let run_id = format!("test-run-{}", uuid::Uuid::new_v4());
    let _run_id_guard = EnvVarGuard::set("TAGENT_RUN_ID", &run_id);

    let workflows_dir = tmp.path().join("workflows");
    write_three_phase_workflow(&workflows_dir, "summary-replay-test");
    let out_dir = tmp.path().join("out");

    // A large body (2KB+) that must NEVER reach phase-b's rendered prompt
    // after resume — only the short summary should.
    let full_content = format!("FULL_CONTENT_MARKER_{}", "x".repeat(2000));
    let short_summary = "CONCISE_SUMMARY_MARKER";

    let mock = Arc::new(FlakyOnceMock::new("agent-b").with_custom_output(
        "agent-a",
        &full_content,
        Some(short_summary),
    ));
    let engine = WorkflowEngine::new(mock.clone(), workflows_dir.clone());

    let first_result = engine
        .run(
            "summary-replay-test",
            "do the thing".into(),
            Some(out_dir.clone()),
        )
        .await;
    assert!(first_result.is_err(), "expected simulated phase-b crash");

    // Sanity: the checkpoint itself carries the SHORT summary separately
    // from the full content for phase-a.
    let project_dir = crate::usage::project_dir();
    let record = checkpoint::load_checkpoint(&project_dir, &run_id).unwrap();
    assert_eq!(
        record.phase_summaries.get("phase-a").map(String::as_str),
        Some(short_summary),
    );
    assert_eq!(
        record.phase_outputs.get("phase-a").map(String::as_str),
        Some(full_content.as_str()),
    );

    engine
        .resume_with_perf_and_dirs(&run_id)
        .await
        .expect("resume should succeed");

    // The ACTUAL rendered task phase-b received after resume (its one and
    // only successful call) must contain the summary and must NOT contain
    // the full raw content.
    let phase_b_task = mock
        .captured_task("agent-b", 0)
        .expect("agent-b must have been dispatched exactly once after resume");
    assert!(
        phase_b_task.contains(short_summary),
        "resumed phase-b prompt must contain phase-a's SUMMARY: {phase_b_task}"
    );
    assert!(
        !phase_b_task.contains(&full_content),
        "resumed phase-b prompt must NOT contain phase-a's full raw content"
    );
}

/// HIGH code-critic finding (PR #3244): a second `tagent resume` for the
/// SAME run id while the first is still in flight must fail closed with
/// `ResumeAlreadyInProgress`, not race it. We simulate "already in flight"
/// by acquiring the resume lock directly (as the first resume would) and
/// keeping the guard alive while attempting a second `resume_with_perf_and_dirs`
/// call for the same run id.
#[tokio::test]
#[serial_test::serial]
async fn resume_fails_closed_when_already_in_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let _project_guard = EnvVarGuard::set("TAGENT_PROJECT_DIR", &tmp.path().to_string_lossy());
    let run_id = format!("test-run-{}", uuid::Uuid::new_v4());

    let workflows_dir = tmp.path().join("workflows");
    write_three_phase_workflow(&workflows_dir, "concurrent-resume-test");
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let record = hand_written_record(
        &run_id,
        "concurrent-resume-test",
        RunState::PhaseComplete { phase_index: 0 },
        &["phase-a", "phase-b", "phase-c"],
        &out_dir,
        &[("phase-a", "a output")],
    );
    let project_dir = crate::usage::project_dir();
    record.write(&project_dir).unwrap();

    // Hold the lock as if a first `tagent resume` were already in flight.
    let _held = checkpoint::acquire_resume_lock(&project_dir, &run_id)
        .expect("simulated in-flight resume must acquire the lock");

    let mock = Arc::new(FlakyOnceMock::new(""));
    let engine = WorkflowEngine::new(mock.clone(), workflows_dir.clone());

    let err = engine
        .resume_with_perf_and_dirs(&run_id)
        .await
        .expect_err("a second concurrent resume must fail closed");
    assert!(
        matches!(err, WorkflowError::ResumeAlreadyInProgress { .. }),
        "expected ResumeAlreadyInProgress, got {err:?}"
    );
    // Nothing was dispatched — the lock check happens before any phase runs.
    assert_eq!(mock.call_count("agent-a"), 0);
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
    let record = hand_written_record(
        &run_id,
        "definition-changed-test",
        RunState::PhaseComplete { phase_index: 0 },
        &["phase-a", "phase-b", "phase-c"],
        &out_dir,
        &[("phase-a", "a output")],
    );
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

    let mock = Arc::new(FlakyOnceMock::new(""));
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
/// to re-run anything — and, per the HIGH code-critic finding on PR #3244,
/// this is also the "no duplicate hooks fire" proof: `resume_with_perf_and_dirs`
/// returns as soon as it observes `Done`, before touching the ticket
/// manager, perf collector, or dispatching a single phase.
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

    let record = hand_written_record(
        &run_id,
        "already-done-test",
        RunState::Done,
        &["phase-a", "phase-b", "phase-c"],
        &out_dir,
        &[],
    );
    record.write(tmp.path()).unwrap();

    let mock = Arc::new(FlakyOnceMock::new(""));
    let engine = WorkflowEngine::new(mock.clone(), workflows_dir.clone());

    let outcome = engine
        .resume_with_perf_and_dirs(&run_id)
        .await
        .expect("resume of a Done checkpoint must not error");
    assert!(matches!(outcome, ResumeOutcome::AlreadyDone { .. }));
    // No phase was dispatched — proves no hooks/side effects fired.
    assert_eq!(mock.call_count("agent-a"), 0);
    assert_eq!(mock.call_count("agent-b"), 0);
    assert_eq!(mock.call_count("agent-c"), 0);
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

    let mock = Arc::new(FlakyOnceMock::new(""));
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
