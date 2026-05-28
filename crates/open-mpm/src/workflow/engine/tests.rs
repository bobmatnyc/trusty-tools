//! Integration tests for `WorkflowEngine` and the engine's phase/wave logic.
//!
//! Why: These tests exercise the full engine via mock `AgentRunner`s — phase
//! ordering, file extraction, the wave loop, retry/elevation, reconciliation,
//! and skill discovery. They live in a dedicated file (included via `#[path]`)
//! to keep `executor.rs` focused on production code.
//! What: A `#[cfg(test)] mod tests` body re-included into `executor`'s scope so
//! `super::*` continues to resolve to the executor module.
//! Test: This file IS the test module.

use super::*;

use std::sync::Mutex;

use super::super::helpers::{reconcile_code_outputs_from, relocate_plan_outputs_from};
use super::super::retry::run_wave_file_with_retry;
use super::super::step_dispatch::{precreate_package_markers, should_precreate};

/// #140: When out_dir itself contains pyproject.toml (the simple case
/// where the engineer writes files directly into out_dir), discovery
/// should return out_dir unchanged. Existing QA behavior must be
/// preserved for this pattern.
#[test]
fn discover_project_dir_returns_out_dir_when_pyproject_at_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("pyproject.toml"), b"[project]\nname='x'\n").unwrap();
    let discovered = discover_project_dir(root).unwrap();
    assert_eq!(discovered, root);
}

/// #140: The primary bug scenario — engineer writes the project into
/// `out_dir/task_board/` with pyproject.toml one level down.
/// Discovery must return the subdirectory so pytest runs where
/// tests/ actually lives.
#[test]
fn discover_project_dir_finds_subdirectory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let project = root.join("task_board");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("pyproject.toml"), b"[project]\nname='x'\n").unwrap();
    std::fs::create_dir(project.join("tests")).unwrap();
    let discovered = discover_project_dir(root).unwrap();
    assert_eq!(discovered, project);
}

/// #140: Discovery must ignore `.venv` and hidden directories that
/// routinely contain their own pyproject.toml inside site-packages.
/// Without this guard, a stale virtualenv could hijack detection.
#[test]
fn discover_project_dir_ignores_venv_and_hidden_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Create a .venv with a dummy pyproject.toml — must be skipped.
    let venv = root.join(".venv");
    std::fs::create_dir(&venv).unwrap();
    std::fs::write(venv.join("pyproject.toml"), b"[project]\nname='venv'\n").unwrap();
    // Fallback should be out_dir itself since no real project exists.
    let discovered = discover_project_dir(root).unwrap();
    assert_eq!(discovered, root);
}

/// #140: When out_dir has no pyproject.toml anywhere, discovery falls
/// back to out_dir so {{project_dir}} templates remain functional.
#[test]
fn discover_project_dir_falls_back_to_out_dir_when_no_project() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir(root.join("src")).unwrap();
    let discovered = discover_project_dir(root).unwrap();
    assert_eq!(discovered, root);
}

use anyhow::Result;
use async_trait::async_trait;

use crate::perf::TokenUsage;
use crate::tools::traits::{AgentOutput, AgentRunner};

/// Mock runner that:
///   - for the "code" agent, returns a content blob with two `## File:`
///     sections so the engine has something to extract;
///   - for the "qa" agent, snapshots the contents of `out_dir` at the
///     moment it is invoked — this is the heart of the #64 assertion:
///     files must exist BEFORE QA runs, not after the workflow ends.
struct PhaseOrderMock {
    out_dir: PathBuf,
    qa_dir_snapshot: Arc<Mutex<Vec<PathBuf>>>,
}

#[async_trait]
impl AgentRunner for PhaseOrderMock {
    async fn run(&self, agent_name: &str, _task: &str) -> Result<AgentOutput> {
        if agent_name == "qa-mock" {
            // Snapshot the out_dir so the test can assert what QA saw.
            let mut found = Vec::new();
            if self.out_dir.exists() {
                let mut stack = vec![self.out_dir.clone()];
                while let Some(p) = stack.pop() {
                    let mut rd = tokio::fs::read_dir(&p).await?;
                    while let Some(entry) = rd.next_entry().await? {
                        let path = entry.path();
                        if path.is_dir() {
                            stack.push(path);
                        } else {
                            found.push(path);
                        }
                    }
                }
            }
            self.qa_dir_snapshot.lock().unwrap().extend(found);
            return Ok(AgentOutput {
                content: "QA result: ok".to_string(),
                summary: Some("QA ok".to_string()),
                usage: TokenUsage::default(),
            });
        }

        if agent_name == "code-mock" {
            let content = "Here is the code.\n\n\
                    ## File: src/hello.py\n\
                    ```python\n\
                    def greet():\n    return \"hi\"\n\
                    ```\n\n\
                    ## File: tests/test_hello.py\n\
                    ```python\n\
                    from src.hello import greet\n\n\
                    def test_greet():\n    assert greet() == \"hi\"\n\
                    ```\n"
                .to_string();
            return Ok(AgentOutput {
                content,
                summary: Some("code summary".into()),
                usage: TokenUsage::default(),
            });
        }

        // Any other agent: return a harmless stub.
        Ok(AgentOutput {
            content: format!("stub output from {agent_name}"),
            summary: None,
            usage: TokenUsage::default(),
        })
    }
}

/// (#64) Files emitted by a `produces_files: true` phase must be on disk
/// BEFORE the next phase runs — not after the workflow completes.
/// We build a minimal two-phase workflow (code -> qa), wire a mock runner
/// that snapshots the `out_dir` contents when QA is invoked, and assert
/// both extracted files are visible from QA's perspective.
#[tokio::test]
async fn files_are_extracted_before_next_phase_runs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out_dir = tmp.path().to_path_buf();

    // Write a minimal workflow JSON that exercises `produces_files`.
    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("order-test.json");
    let wf_json = r#"{
            "name": "order-test",
            "description": "code -> qa order test",
            "phases": [
                {
                    "name": "code",
                    "agent": "code-mock",
                    "produces_files": true,
                    "context_template": "{{task}}"
                },
                {
                    "name": "qa",
                    "agent": "qa-mock",
                    "context_template": "verify {{out_dir}}"
                }
            ]
        }"#;
    tokio::fs::write(&wf_path, wf_json).await.unwrap();

    let snapshot: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = Arc::new(PhaseOrderMock {
        out_dir: out_dir.clone(),
        qa_dir_snapshot: snapshot.clone(),
    });

    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    let ctx = engine
        .run("order-test", "do the thing".into(), Some(out_dir.clone()))
        .await
        .expect("workflow should complete");

    // Assert the snapshot QA captured contains BOTH files written by code.
    let snap = snapshot.lock().unwrap().clone();
    let hello = out_dir.join("src/hello.py");
    let test_hello = out_dir.join("tests/test_hello.py");
    assert!(
        snap.contains(&hello),
        "QA did not see src/hello.py; snapshot was {snap:?}"
    );
    assert!(
        snap.contains(&test_hello),
        "QA did not see tests/test_hello.py; snapshot was {snap:?}"
    );

    // And the engine recorded both phase outputs.
    assert!(ctx.phase_outputs.contains_key("code"));
    assert!(ctx.phase_outputs.contains_key("qa"));

    // Sanity: on-disk bodies match what the mock emitted.
    let hello_body = tokio::fs::read_to_string(&hello).await.unwrap();
    assert!(hello_body.contains("def greet():"));
}

/// A phase WITHOUT `produces_files` must not perform any extraction, even
/// if its output contains `## File:` sections. This guards the opt-in
/// semantics — only the code phase should touch disk.
#[tokio::test]
async fn phase_without_produces_files_does_not_extract() {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().to_path_buf();

    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("no-extract.json");
    let wf_json = r#"{
            "name": "no-extract",
            "description": "no produces_files anywhere",
            "phases": [
                {
                    "name": "code",
                    "agent": "code-mock",
                    "context_template": "{{task}}"
                }
            ]
        }"#;
    tokio::fs::write(&wf_path, wf_json).await.unwrap();

    let snapshot: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = Arc::new(PhaseOrderMock {
        out_dir: out_dir.clone(),
        qa_dir_snapshot: snapshot.clone(),
    });
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    engine
        .run("no-extract", "x".into(), Some(out_dir.clone()))
        .await
        .expect("workflow ok");

    // No files should have been written.
    assert!(!out_dir.join("src/hello.py").exists());
    assert!(!out_dir.join("tests/test_hello.py").exists());
}

/// #153: Mock runner that captures the `working_dir` observed in
/// `RunContext` on each `run_with_context` call. Used to assert the
/// legacy monolithic code path passes `out_dir` as an *absolute* path
/// to the runner (so subprocess-driven runners like claude-code write
/// into out_dir instead of accidentally writing to the parent process
/// CWD — i.e. the project root).
struct WorkingDirCapture {
    working_dirs: Arc<Mutex<Vec<Option<PathBuf>>>>,
}

#[async_trait]
impl AgentRunner for WorkingDirCapture {
    async fn run(&self, agent_name: &str, _task: &str) -> Result<AgentOutput> {
        // Fallback: record a `None` so tests can detect when the engine
        // dispatched through the plain `run` path (which loses working_dir).
        self.working_dirs.lock().unwrap().push(None);
        Ok(AgentOutput {
            content: format!("done {agent_name}"),
            summary: None,
            usage: TokenUsage::default(),
        })
    }

    async fn run_with_context(
        &self,
        agent_name: &str,
        _task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        self.working_dirs
            .lock()
            .unwrap()
            .push(ctx.working_dir.clone());
        Ok(AgentOutput {
            content: format!("done {agent_name}"),
            summary: None,
            usage: TokenUsage::default(),
        })
    }
}

/// #153: The legacy monolithic code path (no `assignments.json`) must
/// thread `out_dir` into `RunContext::working_dir` as an **absolute**
/// path. If it passes a relative path (or `None`), subprocess-driven
/// runners such as `ClaudeCodeAgentRunner` can write files into the
/// parent process CWD (project root) instead of `out_dir`, which breaks
/// `discover_project_dir` and causes QA to fail with "no tests found".
///
/// We drive the engine with a CLI-shaped **relative** `out_dir`
/// (`out/legacy-monolithic-test`), let `run_with_perf` create and
/// canonicalize it, and assert the runner observed an absolute path.
#[tokio::test]
async fn legacy_monolithic_path_passes_absolute_working_dir() {
    // Drive a relative out_dir path, mirroring how the CLI
    // (`--out-dir out/...`) actually wires this.
    let tmp = tempfile::tempdir().expect("tempdir");
    // Chdir into tmp so a *relative* out_dir doesn't pollute the repo.
    // We can't use std::env::set_current_dir in multi-threaded tests
    // safely, so instead we construct a relative path that we'll turn
    // into an absolute tempdir-rooted path only when asserting — the
    // engine itself must handle the absolute-ification internally.
    // To do that, give the engine a path rooted in `tmp` but constructed
    // without canonicalization, simulating a relative-ish input.
    let out_dir = tmp.path().join("out").join("legacy-monolithic-test");

    // Write a one-phase workflow with no produces_files and no
    // assignments.json — this is the pure legacy monolithic path.
    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("legacy-monolithic.json");
    let wf_json = r#"{
            "name": "legacy-monolithic",
            "description": "single-phase monolithic code path",
            "phases": [
                {
                    "name": "code",
                    "agent": "code-mock",
                    "context_template": "{{task}}"
                }
            ]
        }"#;
    tokio::fs::write(&wf_path, wf_json).await.unwrap();

    let captured: Arc<Mutex<Vec<Option<PathBuf>>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = Arc::new(WorkingDirCapture {
        working_dirs: captured.clone(),
    });

    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    engine
        .run(
            "legacy-monolithic",
            "do the thing".into(),
            Some(out_dir.clone()),
        )
        .await
        .expect("workflow should complete");

    // The mock must have been called through `run_with_context` (which
    // is what the legacy monolithic branch uses — not plain `run`).
    let snap = captured.lock().unwrap().clone();
    assert_eq!(
        snap.len(),
        1,
        "expected exactly one agent invocation, saw {snap:?}"
    );
    let observed = snap[0]
        .clone()
        .expect("legacy monolithic path must set RunContext::working_dir, got None");

    // Core assertion: working_dir must be absolute, not a relative path
    // that subprocesses could re-resolve against an inherited CWD.
    assert!(
        observed.is_absolute(),
        "legacy monolithic path must set an absolute working_dir; got {}",
        observed.display()
    );

    // And it must point at the canonical out_dir (same file after
    // canonicalization). We canonicalize our expected path the same
    // way the engine does, so symlink resolution (e.g. /var -> /private/var
    // on macOS) doesn't cause a spurious mismatch.
    let expected = std::fs::canonicalize(&out_dir).expect("canonicalize test out_dir");
    assert_eq!(
        observed, expected,
        "working_dir must match canonicalized out_dir"
    );
}

/// #88: Wave-loop mock that records each per-file task it receives so we
/// can assert the wave loop invokes the code-agent once per assignment
/// in the right order.
struct WaveLoopMock {
    calls: Arc<Mutex<Vec<(String, String)>>>, // (agent, task)
    /// Snapshots of the `max_turns_override` observed at each call. None
    /// means the context carried no override (non-wave path).
    max_turns_snapshots: Arc<Mutex<Vec<Option<u32>>>>,
    out_dir: PathBuf,
}

#[async_trait]
impl AgentRunner for WaveLoopMock {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        // CRIT-1 / MAJ-1 (#90, #93): Default `run` path is taken when
        // the engine calls us without a RunContext (legacy non-wave).
        // Record the call and log that no override was observed.
        self.calls
            .lock()
            .unwrap()
            .push((agent_name.to_string(), task.to_string()));
        self.max_turns_snapshots.lock().unwrap().push(None);

        Ok(AgentOutput {
            content: format!("done {agent_name}"),
            summary: Some("ok".into()),
            usage: TokenUsage::default(),
        })
    }

    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        self.calls
            .lock()
            .unwrap()
            .push((agent_name.to_string(), task.to_string()));

        // CRIT-1 / MAJ-1 (#90, #93): Snapshot the context-provided turn
        // cap so tests can prove the wave loop plumbed it through the
        // `RunContext` instead of mutating parent env vars.
        self.max_turns_snapshots
            .lock()
            .unwrap()
            .push(ctx.max_turns_override);

        // Simulate the agent writing its assigned file to disk using the
        // context-supplied path (previously came from an env var).
        if let Some(path) = &ctx.assigned_file {
            let dest = self.out_dir.join(path);
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
            tokio::fs::write(&dest, b"# generated\n").await.ok();
        }

        Ok(AgentOutput {
            content: format!("done {agent_name}"),
            summary: Some("ok".into()),
            usage: TokenUsage::default(),
        })
    }
}

// CRIT-1 (#90): The `env_lock()` helper previously serialized tests that
// mutated `OPEN_MPM_ASSIGNED_FILE` / `OPEN_MPM_MAX_TURNS` globally. With
// the wave loop now threading those overrides through a `RunContext` on
// each call, no test touches process-global env vars, so no lock is needed.

/// #88: With `assignments.json` present, the code phase invokes the runner
/// once per file in wave order, sets `OPEN_MPM_ASSIGNED_FILE` for each
/// call, and each file's prompt names the correct path.
#[tokio::test]
async fn wave_loop_runs_one_agent_per_file() {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().to_path_buf();

    // Seed assignments.json — two waves, three files total.
    let assignments_json = r#"{
            "error_convention": "exceptions",
            "waves": [
                {
                    "wave": 1,
                    "files": [
                        {"path":"src/util.py","stub":"util.py","purpose":"helpers"},
                        {"path":"src/types.py","stub":"types.py","purpose":"type defs"}
                    ]
                },
                {
                    "wave": 2,
                    "files": [
                        {"path":"src/main.py","stub":"main.py","purpose":"entrypoint",
                         "depends_on":["src/util.py","src/types.py"]}
                    ]
                }
            ]
        }"#;
    tokio::fs::write(out_dir.join("assignments.json"), assignments_json)
        .await
        .unwrap();

    // Minimal workflow with just the code phase.
    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("wave-test.json");
    let wf_json = r#"{
            "name": "wave-test",
            "description": "wave loop",
            "phases": [
                {"name":"code","agent":"code-agent","context_template":"{{task}}"}
            ]
        }"#;
    tokio::fs::write(&wf_path, wf_json).await.unwrap();

    let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let max_turns_snapshots: Arc<Mutex<Vec<Option<u32>>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = Arc::new(WaveLoopMock {
        calls: calls.clone(),
        max_turns_snapshots: max_turns_snapshots.clone(),
        out_dir: out_dir.clone(),
    });
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    engine
        .run("wave-test", "x".into(), Some(out_dir.clone()))
        .await
        .expect("wave-loop workflow ok");

    let recorded = calls.lock().unwrap().clone();
    // Three per-file invocations, all against code-agent.
    assert_eq!(recorded.len(), 3, "expected 3 calls, got {recorded:?}");
    assert!(recorded.iter().all(|(a, _)| a == "code-agent"));

    // Wave order: util.py, types.py, then main.py.
    assert!(recorded[0].1.contains("src/util.py"));
    assert!(recorded[1].1.contains("src/types.py"));
    assert!(recorded[2].1.contains("src/main.py"));

    // Main.py's prompt must list its dependencies.
    assert!(recorded[2].1.contains("src/util.py"));
    assert!(recorded[2].1.contains("src/types.py"));

    // Each per-file task mentions the stub read step.
    assert!(recorded[0].1.contains("stubs/util.py"));
    assert!(recorded[2].1.contains("stubs/main.py"));

    // Files landed on disk (written by the mock from ctx.assigned_file).
    assert!(out_dir.join("src/util.py").exists());
    assert!(out_dir.join("src/types.py").exists());
    assert!(out_dir.join("src/main.py").exists());

    // CRIT-1 (#90): Every per-file invocation must observe
    // max_turns_override=40 via the RunContext (not a process env var)
    // so the sub-agent's turn budget is adequate for complex files.
    let turns = max_turns_snapshots.lock().unwrap().clone();
    assert_eq!(turns.len(), 3);
    for (i, v) in turns.iter().enumerate() {
        assert_eq!(
            *v,
            Some(40),
            "call {i} expected max_turns_override=40, got {v:?}"
        );
    }

    // CRIT-1 (#90): Parent env must never be touched by the wave loop.
    assert!(std::env::var("OPEN_MPM_ASSIGNED_FILE").is_err());
    assert!(std::env::var("OPEN_MPM_MAX_TURNS").is_err());
}

/// #166: Regression test — the per-file wave-loop task must instruct the
/// agent to call write_file with an ABSOLUTE path (out_dir + file.path).
///
/// Why: The claude CLI subprocess anchors relative `write_file` calls to
/// the git repository root, so a relative path like
/// `multi_repo_analyzer/pyproject.toml` would land at the repo root
/// instead of under out_dir.
/// What: Run the wave loop with a known out_dir and assert each recorded
/// task prompt contains `out_dir/<file.path>` and instructs the agent to
/// use that absolute path for write_file.
/// Test: This function — set up a temp out_dir, seed assignments.json
/// with a relative file path, run the engine, and assert the mock's
/// recorded prompt contains the absolute path and the ABSOLUTE PATH
/// warning language.
#[tokio::test]
async fn wave_loop_task_uses_absolute_path_for_write() {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().to_path_buf();

    // Single file, single wave — minimal seed.
    let assignments_json = r#"{
            "error_convention": "exceptions",
            "waves": [
                {
                    "wave": 1,
                    "files": [
                        {"path":"multi_repo_analyzer/pyproject.toml","stub":"pyproject.toml","purpose":"build config"}
                    ]
                }
            ]
        }"#;
    tokio::fs::write(out_dir.join("assignments.json"), assignments_json)
        .await
        .unwrap();

    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("wave-abs-test.json");
    let wf_json = r#"{
            "name": "wave-abs-test",
            "description": "wave loop absolute path",
            "phases": [
                {"name":"code","agent":"code-agent","context_template":"{{task}}"}
            ]
        }"#;
    tokio::fs::write(&wf_path, wf_json).await.unwrap();

    let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let max_turns_snapshots: Arc<Mutex<Vec<Option<u32>>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = Arc::new(WaveLoopMock {
        calls: calls.clone(),
        max_turns_snapshots: max_turns_snapshots.clone(),
        out_dir: out_dir.clone(),
    });
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    engine
        .run("wave-abs-test", "x".into(), Some(out_dir.clone()))
        .await
        .expect("wave-loop workflow ok");

    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1, "expected 1 call, got {recorded:?}");
    let prompt = &recorded[0].1;

    // The prompt must contain the absolute path (out_dir + file.path).
    let expected_abs = out_dir.join("multi_repo_analyzer/pyproject.toml");
    let expected_abs_str = expected_abs.to_string_lossy().to_string();
    assert!(
        prompt.contains(&expected_abs_str),
        "task prompt missing absolute path `{expected_abs_str}`; prompt was:\n{prompt}"
    );

    // The prompt must use ABSOLUTE PATH language in the write step so
    // agents cannot miss the instruction.
    assert!(
        prompt.contains("ABSOLUTE PATH"),
        "task prompt missing ABSOLUTE PATH emphasis; prompt was:\n{prompt}"
    );
    assert!(
        prompt.contains("write_file"),
        "task prompt missing write_file reference; prompt was:\n{prompt}"
    );

    // The relative path is still present (for reading stubs/deps and
    // human-readable context), but the write step explicitly warns
    // against using it for writing.
    assert!(prompt.contains("multi_repo_analyzer/pyproject.toml"));
    assert!(
        prompt.contains("will land in the wrong directory"),
        "task prompt missing 'wrong directory' warning; prompt was:\n{prompt}"
    );
}

/// Mock runner for the "plan writes assignments.json then code uses
/// wave-loop" regression test. The plan agent writes assignments.json
/// during its run (simulating what the real plan-agent's write_file tool
/// does). The code agent records each call it receives.
struct PlanThenCodeMock {
    /// (agent, task) pairs recorded in order.
    calls: Arc<Mutex<Vec<(String, String)>>>,
    /// Directory where the "plan" step will write assignments.json and
    /// where the "code" step expects to find it.
    out_dir: PathBuf,
    /// Body to write as assignments.json when the plan agent runs.
    assignments_body: String,
}

#[async_trait]
impl AgentRunner for PlanThenCodeMock {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        // Non-wave calls (plan phase) land here; wave-loop calls go
        // through `run_with_context` below.
        self.calls
            .lock()
            .unwrap()
            .push((agent_name.to_string(), task.to_string()));

        if agent_name == "plan-mock" {
            // Simulate the plan-agent's write_file("assignments.json", ...)
            // that happens DURING plan phase execution. The engine's
            // wave-loop decision must observe this write when it later
            // processes the code phase.
            tokio::fs::write(
                self.out_dir.join("assignments.json"),
                &self.assignments_body,
            )
            .await
            .ok();
            return Ok(AgentOutput {
                content: "plan done".into(),
                summary: Some("planned".into()),
                usage: TokenUsage::default(),
            });
        }

        Ok(AgentOutput {
            content: format!("done {agent_name}"),
            summary: Some("ok".into()),
            usage: TokenUsage::default(),
        })
    }

    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        self.calls
            .lock()
            .unwrap()
            .push((agent_name.to_string(), task.to_string()));

        // plan-mock goes through non-wave path which may or may not set
        // working_dir; still simulate its assignments.json write.
        if agent_name == "plan-mock" {
            tokio::fs::write(
                self.out_dir.join("assignments.json"),
                &self.assignments_body,
            )
            .await
            .ok();
            return Ok(AgentOutput {
                content: "plan done".into(),
                summary: Some("planned".into()),
                usage: TokenUsage::default(),
            });
        }

        // code-agent: wave-loop writes each per-file output using the
        // context-supplied assigned_file.
        if let Some(path) = &ctx.assigned_file {
            let dest = self.out_dir.join(path);
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
            tokio::fs::write(&dest, b"# generated\n").await.ok();
        }

        Ok(AgentOutput {
            content: format!("done {agent_name}"),
            summary: Some("ok".into()),
            usage: TokenUsage::default(),
        })
    }
}

/// #88 regression (post-merge bug): The wave-loop trigger must check
/// `out_dir/assignments.json` AFTER the plan phase has written it, not
/// before. This test exercises the full two-phase (plan -> code) path
/// with no pre-seeded assignments.json — the plan mock writes it at
/// runtime. If the engine checked for assignments.json at startup (or
/// only before the first phase), the code phase would see nothing and
/// fall through to the legacy path. We assert the wave loop fires by
/// counting per-file code-agent invocations.
#[tokio::test]
async fn wave_loop_triggers_after_plan_phase_writes_assignments() {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().to_path_buf();

    // Deliberately NOT seeded — plan-mock writes this during its run.
    assert!(!out_dir.join("assignments.json").exists());

    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("plan-then-code.json");
    let wf_json = r#"{
            "name": "plan-then-code",
            "phases": [
                {"name":"plan","agent":"plan-mock","context_template":"{{task}}"},
                {"name":"code","agent":"code-agent","context_template":"{{plan}}"}
            ]
        }"#;
    tokio::fs::write(&wf_path, wf_json).await.unwrap();

    let assignments_body = r#"{
            "error_convention": "exceptions",
            "waves": [
                {
                    "wave": 1,
                    "files": [
                        {"path":"src/a.py","stub":"a.py","purpose":"first"},
                        {"path":"src/b.py","stub":"b.py","purpose":"second"}
                    ]
                }
            ]
        }"#
    .to_string();

    let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = Arc::new(PlanThenCodeMock {
        calls: calls.clone(),
        out_dir: out_dir.clone(),
        assignments_body,
    });
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    engine
        .run("plan-then-code", "x".into(), Some(out_dir.clone()))
        .await
        .expect("plan-then-code workflow ok");

    let recorded = calls.lock().unwrap().clone();

    // Expect exactly 3 calls: 1 plan + 2 per-file code-agent invocations.
    // If the wave loop didn't trigger, this would be 2 (plan + 1 code).
    assert_eq!(
        recorded.len(),
        3,
        "expected plan + 2 per-file code calls, got {recorded:?}"
    );
    assert_eq!(recorded[0].0, "plan-mock");
    assert_eq!(recorded[1].0, "code-agent");
    assert_eq!(recorded[2].0, "code-agent");

    // Each per-file prompt must name its assigned file.
    assert!(
        recorded[1].1.contains("src/a.py"),
        "first code call missing src/a.py: {}",
        recorded[1].1
    );
    assert!(
        recorded[2].1.contains("src/b.py"),
        "second code call missing src/b.py: {}",
        recorded[2].1
    );

    // Assignments.json was written by plan-mock and survived the run.
    assert!(out_dir.join("assignments.json").exists());

    // Both assigned files landed on disk (written by the mock via env var).
    assert!(out_dir.join("src/a.py").exists());
    assert!(out_dir.join("src/b.py").exists());
}

/// #88: A workflow without assignments.json runs the code phase the old
/// way — one invocation of the code-agent, not per-file.
#[tokio::test]
async fn wave_loop_skipped_when_assignments_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().to_path_buf();
    // Intentionally: no assignments.json written.

    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("bc-test.json");
    let wf_json = r#"{
            "name": "bc-test",
            "phases": [
                {"name":"code","agent":"code-agent","context_template":"{{task}}"}
            ]
        }"#;
    tokio::fs::write(&wf_path, wf_json).await.unwrap();

    let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let max_turns_snapshots: Arc<Mutex<Vec<Option<u32>>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = Arc::new(WaveLoopMock {
        calls: calls.clone(),
        max_turns_snapshots: max_turns_snapshots.clone(),
        out_dir: out_dir.clone(),
    });
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    engine
        .run("bc-test", "x".into(), Some(out_dir.clone()))
        .await
        .expect("bc workflow ok");

    // Exactly one call — the single monolithic code-agent invocation.
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1, "expected 1 call, got {recorded:?}");

    // CRIT-1 (#90): Legacy (non-wave) path must NOT supply a
    // max_turns_override so the single-shot invocation honors the agent
    // TOML's default. The RunContext carries `None` for non-wave calls.
    let turns = max_turns_snapshots.lock().unwrap().clone();
    assert_eq!(turns, vec![None]);
}

/// #108/#109: an engine configured with an `InitContext` must prepend
/// the project summary + memories prefix to every phase's rendered task.
/// We use a recording mock that captures the exact task text the runner
/// receives and assert the prefix appears before the task body.
#[tokio::test]
async fn init_context_is_prepended_to_phase_template() {
    struct RecordingRunner {
        tasks: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl AgentRunner for RecordingRunner {
        async fn run(&self, _agent_name: &str, task: &str) -> Result<AgentOutput> {
            self.tasks.lock().unwrap().push(task.to_string());
            Ok(AgentOutput {
                content: "ok".into(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("init-test.json");
    let wf_json = r#"{
            "name": "init-test",
            "description": "single phase",
            "phases": [
                {"name":"research","agent":"research-agent","context_template":"TASK={{task}}"}
            ]
        }"#;
    tokio::fs::write(&wf_path, wf_json).await.unwrap();

    let tasks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = Arc::new(RecordingRunner {
        tasks: tasks.clone(),
    });

    let ic = InitContext {
        project_summary: "# Project: demo\nindex body".into(),
        relevant_memories: vec!["prior fact".into()],
        initialized_at: chrono::Utc::now(),
    };

    let engine = WorkflowEngine::new(mock, workflows_dir.clone()).with_init_context(Some(ic));
    let _ = engine
        .run("init-test", "my-task".into(), None)
        .await
        .expect("workflow ok");

    let recorded = tasks.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    let seen = &recorded[0];
    assert!(
        seen.contains("## Project Context (auto-indexed)"),
        "seen: {seen}"
    );
    assert!(seen.contains("prior fact"), "seen: {seen}");
    assert!(seen.contains("TASK=my-task"), "seen: {seen}");
    // Ordering: prefix must come before task body.
    let pidx = seen.find("Project Context").unwrap();
    let tidx = seen.find("TASK=my-task").unwrap();
    assert!(pidx < tidx, "prefix should appear before task body");
}

// ---- #150: Pre-create empty package markers ----

fn fa_raw(
    path: &str,
    stub: Option<&str>,
    purpose: &str,
) -> crate::workflow::config::FileAssignment {
    crate::workflow::config::FileAssignment {
        path: path.to_string(),
        stub: stub.map(String::from),
        purpose: purpose.to_string(),
        depends_on: Vec::new(),
        max_lines: None,
    }
}

#[test]
fn should_precreate_detects_init_py() {
    // #150: __init__.py always qualifies for pre-creation, even when the
    // plan-agent attached a stub and purpose — engineers still skip them.
    let f = fa_raw("pkg/__init__.py", Some("init.py"), "package marker");
    assert!(should_precreate(&f));

    let f2 = fa_raw("a/b/c/__init__.py", None, "");
    assert!(should_precreate(&f2));
}

#[test]
fn should_precreate_detects_empty_stub_and_purpose() {
    // #150: stub:null + empty purpose signals a plan-agent placeholder.
    let f = fa_raw("src/placeholder.py", None, "");
    assert!(should_precreate(&f));

    let f_ws = fa_raw("src/placeholder.py", None, "   ");
    assert!(should_precreate(&f_ws));
}

#[test]
fn should_precreate_rejects_normal_file() {
    // #150: Normal files with real purpose are left to the engineer.
    let f = fa_raw("src/main.py", Some("main.py"), "entrypoint");
    assert!(!should_precreate(&f));

    let f_no_stub = fa_raw("src/main.py", None, "entrypoint logic");
    assert!(!should_precreate(&f_no_stub));
}

#[tokio::test]
async fn precreate_package_markers_creates_init_py() {
    // #150: Given an assignments plan with an __init__.py, pre-creation
    // writes an empty file at the expected path under out_dir so the
    // wave-loop presence check passes even if the agent skips it.
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path();

    let assignments = Assignments {
        error_convention: None,
        waves: vec![crate::workflow::config::WaveDef {
            wave: 1,
            files: vec![
                fa_raw("git_analyzer/src/git_analyzer/__init__.py", None, ""),
                fa_raw("src/main.py", Some("main.py"), "entrypoint"),
            ],
        }],
    };

    precreate_package_markers(&assignments, out_dir)
        .await
        .expect("pre-create ok");

    let init = out_dir.join("git_analyzer/src/git_analyzer/__init__.py");
    assert!(init.exists(), "__init__.py should be pre-created");
    let content = tokio::fs::read(&init).await.unwrap();
    assert!(content.is_empty(), "pre-created file must be empty");

    // Non-placeholder file must NOT be pre-created.
    let main = out_dir.join("src/main.py");
    assert!(!main.exists(), "main.py should be left to the engineer");
}

#[tokio::test]
async fn precreate_package_markers_preserves_existing_content() {
    // #150: If a file already exists (e.g. from a prior wave), pre-create
    // must NOT overwrite it.
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path();

    let existing = out_dir.join("pkg/__init__.py");
    tokio::fs::create_dir_all(existing.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&existing, b"from .x import *\n")
        .await
        .unwrap();

    let assignments = Assignments {
        error_convention: None,
        waves: vec![crate::workflow::config::WaveDef {
            wave: 1,
            files: vec![fa_raw("pkg/__init__.py", None, "")],
        }],
    };

    precreate_package_markers(&assignments, out_dir)
        .await
        .expect("pre-create ok");

    let content = tokio::fs::read(&existing).await.unwrap();
    assert_eq!(content, b"from .x import *\n");
}

/// #160: Regression test — if the plan-agent writes `assignments.json`
/// at the git project root (because its claude CLI anchors relative
/// Write-tool paths to the inherited CWD instead of
/// `RunContext::working_dir`), the post-plan relocation step must move
/// it into `out_dir` so the wave-loop check succeeds.
#[tokio::test]
async fn post_plan_relocates_assignments_json_from_git_root() {
    // Arrange: separate simulated project root and out_dir.
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    let out_dir = tmp.path().join("out");
    tokio::fs::create_dir_all(&project_root).await.unwrap();
    tokio::fs::create_dir_all(&out_dir).await.unwrap();

    // Simulate plan-agent's misroute: assignments.json lands at project
    // root instead of out_dir.
    let misrouted = project_root.join("assignments.json");
    let body = r#"{"error_convention":"exceptions","waves":[{"wave":1,"files":[{"path":"app/main.py","stub":"main.py","purpose":"entry","depends_on":[],"max_lines":100}]}]}"#;
    tokio::fs::write(&misrouted, body).await.unwrap();

    // Also simulate a stubs/ directory at project root.
    let misrouted_stubs = project_root.join("stubs");
    tokio::fs::create_dir_all(&misrouted_stubs).await.unwrap();
    tokio::fs::write(misrouted_stubs.join("main.py"), b"# stub")
        .await
        .unwrap();

    // Act: run the relocation logic against the simulated project root.
    relocate_plan_outputs_from(&project_root, &out_dir)
        .await
        .expect("relocation should succeed");

    // Assert: assignments.json is now in out_dir with correct contents.
    let relocated = out_dir.join("assignments.json");
    assert!(
        relocated.is_file(),
        "assignments.json should be relocated to out_dir, but {} is missing",
        relocated.display()
    );
    let read_body = tokio::fs::read_to_string(&relocated).await.unwrap();
    assert_eq!(
        read_body, body,
        "relocated assignments.json content mismatch"
    );

    // Assert: the misrouted file at project root is gone (moved, not copied).
    assert!(
        !misrouted.exists(),
        "misrouted assignments.json should be removed from project root after relocation"
    );

    // Assert: stubs/ was also relocated.
    let relocated_stubs = out_dir.join("stubs");
    assert!(
        relocated_stubs.is_dir(),
        "stubs/ should be relocated to out_dir"
    );
    assert!(
        relocated_stubs.join("main.py").is_file(),
        "stubs/main.py should be present at out_dir/stubs/main.py"
    );
}

/// #160: If `out_dir/assignments.json` already exists, relocation is a
/// no-op — we must not clobber the happy-path output with whatever is
/// sitting at the project root.
#[tokio::test]
async fn post_plan_relocation_is_noop_when_out_dir_has_assignments() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    let out_dir = tmp.path().join("out");
    tokio::fs::create_dir_all(&project_root).await.unwrap();
    tokio::fs::create_dir_all(&out_dir).await.unwrap();

    let good = out_dir.join("assignments.json");
    tokio::fs::write(&good, b"GOOD").await.unwrap();

    // Planted stale/bad file at project root that must NOT be moved.
    let stale = project_root.join("assignments.json");
    tokio::fs::write(&stale, b"STALE").await.unwrap();

    relocate_plan_outputs_from(&project_root, &out_dir)
        .await
        .expect("noop relocation ok");

    assert_eq!(tokio::fs::read(&good).await.unwrap(), b"GOOD");
    assert!(
        stale.exists(),
        "stale file at project root must not be removed"
    );
}

// ── #173: pre-plan skill discovery ────────────────────────────────────
//
// Why: The engine should derive `TaskSignals` from the task text, query
// the tag-indexed registry, and prepend a "## Available Skills" block to
// the plan-agent's prompt — without the plan-agent ever calling
// `list_skills`. These tests pin the contract.

/// Build a tag-indexed registry from a fresh temp dir containing a few
/// `python` / `fastapi` / `pytest` skills.
fn temp_tag_registry_with_python_skills() -> (tempfile::TempDir, Arc<TagSkillRegistry>) {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, desc: &str, tags: &[&str]| {
        let tags_str = tags
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let content = format!(
            "---\nname: {name}\ndescription: {desc}\ntags: [{tags_str}]\n---\n\n# {name}\nbody\n",
        );
        std::fs::write(dir.path().join(format!("{name}.md")), content).unwrap();
    };
    write(
        "fastapi",
        "FastAPI application patterns, TestClient usage, module-level state",
        &["python", "fastapi", "api"],
    );
    write(
        "pytest",
        "async fixtures, parametrize, conftest patterns",
        &["python", "testing", "pytest"],
    );
    write(
        "python",
        "type hints, dataclasses, NLP setup",
        &["python", "packaging"],
    );
    write("rust", "Rust patterns", &["rust", "tokio"]);

    let reg = TagSkillRegistry::load(&[dir.path().to_path_buf()]);
    (dir, Arc::new(reg))
}

/// #173: discovery must pull `python` + `fastapi` skills out of the
/// tag-indexed registry given a task that mentions Python and FastAPI.
/// The Rust-only skill must NOT appear because no rust signals match.
#[test]
fn skill_discovery_extracts_python_fastapi_tags() {
    let (_keep, reg) = temp_tag_registry_with_python_skills();

    struct NopRunner;
    #[async_trait]
    impl AgentRunner for NopRunner {
        async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
            Ok(AgentOutput {
                content: String::new(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    let engine = WorkflowEngine::new(Arc::new(NopRunner), PathBuf::from("."))
        .with_tag_skill_registry(Some(reg));

    let task = "Build a Python FastAPI service with pytest tests for the REST endpoints";
    let discovered = engine.discover_skills_for_task(task, 8);

    let names: Vec<&str> = discovered.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"fastapi"),
        "fastapi should be discovered: got {names:?}"
    );
    assert!(
        names.contains(&"pytest"),
        "pytest should be discovered: got {names:?}"
    );
    assert!(
        names.contains(&"python"),
        "python should be discovered: got {names:?}"
    );
    assert!(
        !names.contains(&"rust"),
        "rust must not be matched: got {names:?}"
    );

    // Each discovered skill carries a non-empty summary + tags.
    for s in &discovered {
        assert!(!s.summary.is_empty(), "summary empty for {}", s.name);
        assert!(!s.tags.is_empty(), "tags empty for {}", s.name);
    }
}

/// #173: when many skills tie on raw tag-overlap, effectiveness scores
/// drive the top-N ordering — the engine must respect the registry's
/// ranking and only return the top `limit`.
#[test]
fn skill_discovery_returns_top_n_by_effectiveness() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, desc: &str, tags: &[&str]| {
        let tags_str = tags
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let content =
            format!("---\nname: {name}\ndescription: {desc}\ntags: [{tags_str}]\n---\n\nbody\n",);
        std::fs::write(dir.path().join(format!("{name}.md")), content).unwrap();
    };
    // Five skills, all matching the single "python" tag — effectiveness
    // breaks the tie. Discovery order (insertion) is the secondary
    // tie-breaker so we drive ranking purely via effectiveness.
    write("a", "d", &["python"]);
    write("b", "d", &["python"]);
    write("c", "d", &["python"]);
    write("d", "d", &["python"]);
    write("e", "d", &["python"]);

    let mut reg = TagSkillRegistry::load(&[dir.path().to_path_buf()]);
    // Push c and a to the top via effectiveness boost.
    reg.update_effectiveness("c", 1.0);
    reg.update_effectiveness("c", 1.0);
    reg.update_effectiveness("c", 1.0);
    reg.update_effectiveness("a", 1.0);

    struct NopRunner;
    #[async_trait]
    impl AgentRunner for NopRunner {
        async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
            Ok(AgentOutput {
                content: String::new(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    let engine = WorkflowEngine::new(Arc::new(NopRunner), PathBuf::from("."))
        .with_tag_skill_registry(Some(Arc::new(reg)));

    let discovered = engine.discover_skills_for_task("Write a python script", 2);
    assert_eq!(discovered.len(), 2, "limit must be honored");
    // The boosted skills should come first; we don't assert exact order
    // beyond "c is first" because effectiveness EMA + tie-breakers can
    // shift across same-effectiveness siblings.
    assert_eq!(
        discovered[0].name,
        "c",
        "highest-effectiveness skill should rank first; got {:?}",
        discovered.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// #173: discovery returns empty when the registry is absent or empty —
/// the engine must NOT panic and must NOT inject anything into the
/// plan-agent prompt downstream.
#[test]
fn skill_discovery_returns_empty_when_registry_absent() {
    struct NopRunner;
    #[async_trait]
    impl AgentRunner for NopRunner {
        async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
            Ok(AgentOutput {
                content: String::new(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    let engine = WorkflowEngine::new(Arc::new(NopRunner), PathBuf::from("."));
    let discovered = engine.discover_skills_for_task("python fastapi", 8);
    assert!(discovered.is_empty(), "no registry → empty discovery");

    let empty_reg = Arc::new(TagSkillRegistry::empty());
    let engine = engine.with_tag_skill_registry(Some(empty_reg));
    let discovered = engine.discover_skills_for_task("python fastapi", 8);
    assert!(discovered.is_empty(), "empty registry → empty discovery");
}

/// #173: end-to-end — when the engine runs a workflow whose `plan` phase
/// matches discovered skills, the runner sees the assembled task text
/// containing the "## Available Skills" header. Other phases must NOT
/// receive that block.
#[tokio::test]
async fn plan_agent_context_includes_skill_summaries() {
    let (_keep, reg) = temp_tag_registry_with_python_skills();

    struct RecordingRunner {
        tasks: Arc<Mutex<Vec<(String, String)>>>,
    }
    #[async_trait]
    impl AgentRunner for RecordingRunner {
        async fn run(&self, agent: &str, task: &str) -> Result<AgentOutput> {
            self.tasks
                .lock()
                .unwrap()
                .push((agent.to_string(), task.to_string()));
            Ok(AgentOutput {
                content: "ok".into(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("plan-skills.json");
    // Two phases: a research phase (must NOT get the block) and a plan
    // phase (must receive it).
    let wf_json = r#"{
            "name": "plan-skills",
            "phases": [
                {"name":"research","agent":"research-agent","context_template":"R={{task}}"},
                {"name":"plan","agent":"plan-agent","context_template":"P={{task}}"}
            ]
        }"#;
    tokio::fs::write(&wf_path, wf_json).await.unwrap();

    let tasks: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = Arc::new(RecordingRunner {
        tasks: tasks.clone(),
    });

    let engine =
        WorkflowEngine::new(mock, workflows_dir.clone()).with_tag_skill_registry(Some(reg));
    // #196: pin persona to engineer so the persona heuristic doesn't
    // accidentally classify this as "hacker" (the substring `fast` in
    // "FastAPI" matches the hacker keyword) and skip the research phase.
    engine
        .run(
            "plan-skills",
            "[engineer] Build a Python FastAPI service with pytest tests".into(),
            None,
        )
        .await
        .expect("workflow ok");

    let recorded = tasks.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2);

    let (research_agent, research_task) = &recorded[0];
    assert_eq!(research_agent, "research-agent");
    assert!(
        !research_task.contains("## Available Skills"),
        "research phase must not receive the discovery block: {research_task}"
    );

    let (plan_agent, plan_task) = &recorded[1];
    assert_eq!(plan_agent, "plan-agent");
    assert!(
        plan_task.contains("## Available Skills"),
        "plan phase prompt must contain '## Available Skills': {plan_task}"
    );
    // The block must precede the rendered template body.
    let header_idx = plan_task.find("## Available Skills").unwrap();
    let body_idx = plan_task.find("P=Build a Python").expect("body present");
    assert!(
        header_idx < body_idx,
        "skills block must come before the task body"
    );
}

// ── #123: post-code reconciliation + QA path injection ─────────────────

/// #123: When the code phase ran under a `claude-code` runner and that
/// runner wrote a file declared in `assignments.json` to the project
/// root instead of `out_dir`, the post-code reconciliation step must
/// move it into `out_dir` so QA finds it.
#[tokio::test]
async fn post_code_reconciles_files_from_project_root() {
    // Arrange: separate simulated project_root and out_dir, plus an
    // assignments.json declaring two files. One file lands at out_dir
    // (happy path). The other lands at project_root (misroute) and must
    // be relocated.
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    let out_dir = tmp.path().join("out");
    tokio::fs::create_dir_all(&project_root).await.unwrap();
    tokio::fs::create_dir_all(&out_dir).await.unwrap();

    // Plant assignments.json into out_dir so reconcile can read it.
    let asg_body = r#"{"error_convention":"exceptions","waves":[{"wave":1,"files":[{"path":"src/good.py","stub":null,"purpose":"on-disk","depends_on":[],"max_lines":100},{"path":"src/stray.py","stub":null,"purpose":"misrouted","depends_on":[],"max_lines":100}]}]}"#;
    tokio::fs::write(out_dir.join("assignments.json"), asg_body)
        .await
        .unwrap();

    // good.py is already in out_dir (happy path).
    tokio::fs::create_dir_all(out_dir.join("src"))
        .await
        .unwrap();
    tokio::fs::write(out_dir.join("src/good.py"), b"# good")
        .await
        .unwrap();

    // stray.py landed at project_root instead — this is the misroute we
    // need to reconcile.
    tokio::fs::create_dir_all(project_root.join("src"))
        .await
        .unwrap();
    tokio::fs::write(project_root.join("src/stray.py"), b"# stray")
        .await
        .unwrap();

    // Act: run reconciliation with the simulated project_root.
    reconcile_code_outputs_from(&project_root, &out_dir)
        .await
        .expect("reconciliation should succeed");

    // Assert: stray.py is now in out_dir, with correct content.
    let relocated = out_dir.join("src/stray.py");
    assert!(
        relocated.is_file(),
        "stray.py should be relocated into out_dir, but {} is missing",
        relocated.display()
    );
    let body = tokio::fs::read(&relocated).await.unwrap();
    assert_eq!(body, b"# stray", "relocated content mismatch");

    // Assert: project_root no longer holds the stray file.
    assert!(
        !project_root.join("src/stray.py").exists(),
        "stray.py should be removed from project_root after relocation"
    );

    // Assert: good.py was untouched (still at out_dir).
    let good_body = tokio::fs::read(out_dir.join("src/good.py")).await.unwrap();
    assert_eq!(good_body, b"# good");
}

/// #123: Reconciliation refuses to act on an unsafe path even if a
/// malicious assignments.json slipped past validation. We simulate this
/// by writing assignments.json directly (bypassing `Assignments::load`'s
/// validator is not actually possible, so we instead verify the reconcile
/// step skips when no assignments are present — the safe default).
#[tokio::test]
async fn post_code_reconcile_is_noop_without_assignments() {
    let tmp = tempfile::tempdir().unwrap();
    let project_root = tmp.path().join("project");
    let out_dir = tmp.path().join("out");
    tokio::fs::create_dir_all(&project_root).await.unwrap();
    tokio::fs::create_dir_all(&out_dir).await.unwrap();

    // No assignments.json in out_dir. Plant a file at project_root that
    // would otherwise be a tempting target.
    tokio::fs::write(project_root.join("src.py"), b"# no plan")
        .await
        .unwrap();

    reconcile_code_outputs_from(&project_root, &out_dir)
        .await
        .expect("noop reconcile ok");

    // The file must still be at project_root — without assignments.json
    // we have no list of files to reconcile, so we touch nothing.
    assert!(project_root.join("src.py").exists());
    assert!(!out_dir.join("src.py").exists());
}

/// #123: When the code phase ran under a `claude-code` runner, the QA
/// phase's rendered task must include the project root path so QA knows
/// where to look for any files that escaped reconciliation. Verifies the
/// engine prepends the path-search hint to the QA prompt, while leaving
/// it absent for non-claude-code runners.
#[tokio::test]
async fn qa_receives_correct_path_for_claude_code_runner() {
    // Arrange: a workflow with a code phase that uses an agent backed by
    // the claude-code runner, then a QA phase. We capture the rendered
    // prompts the runner sees.
    struct CapturingRunner {
        tasks: Arc<Mutex<Vec<(String, String)>>>,
    }
    #[async_trait]
    impl AgentRunner for CapturingRunner {
        async fn run(&self, agent: &str, task: &str) -> Result<AgentOutput> {
            self.tasks
                .lock()
                .unwrap()
                .push((agent.to_string(), task.to_string()));
            Ok(AgentOutput {
                content: "ok".into(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_path = workflows_dir.join("qa-path.json");
    // The "engineer" agent ships with `runner = "claude-code"` in its
    // bundled TOML, so the engine sets `code_phase_used_claude_code`.
    let wf_json = r#"{
            "name": "qa-path",
            "phases": [
                {"name":"code","agent":"engineer","context_template":"CODE={{task}}"},
                {"name":"qa","agent":"qa-agent","context_template":"QA={{task}} ROOT={{project_root}}"}
            ]
        }"#;
    tokio::fs::write(&wf_path, wf_json).await.unwrap();

    let tasks: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let mock = Arc::new(CapturingRunner {
        tasks: tasks.clone(),
    });

    let out_dir = tmp.path().join("out");
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    engine
        .run("qa-path", "build it".into(), Some(out_dir.clone()))
        .await
        .expect("workflow ok");

    let recorded = tasks.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        2,
        "expected code + qa calls, got {recorded:?}"
    );
    let (qa_agent, qa_task) = &recorded[1];
    assert_eq!(qa_agent, "qa-agent");

    // The QA prompt must contain the path-search hint AND the resolved
    // project_root from {{project_root}} substitution.
    assert!(
        qa_task.contains("claude-code runner was used"),
        "QA prompt missing claude-code hint: {qa_task}"
    );
    let cwd = std::env::current_dir().unwrap().display().to_string();
    assert!(
        qa_task.contains(&cwd),
        "QA prompt should include project_root path '{cwd}': {qa_task}"
    );
    // The hint must appear BEFORE the rendered template body.
    let hint_idx = qa_task.find("claude-code runner was used").unwrap();
    let body_idx = qa_task.find("QA=build it").expect("body present");
    assert!(
        hint_idx < body_idx,
        "claude-code hint must precede the rendered task body"
    );
}
/// A transient 5xx error on the first call must be retried; success on the
/// second call must be returned to the wave loop.
///
/// Why: The original code returned the first error immediately; this test
/// pins the new retry contract.
/// What: Mock runner fails once with "status 500 internal server error"
/// then succeeds; assert the final result is Ok and that two calls were
/// made.
/// Test: this function.
#[tokio::test]
async fn wave_loop_retries_on_transient_error() {
    use std::sync::atomic::{AtomicU32, Ordering};

    struct TransientMock {
        calls: AtomicU32,
        out_dir: PathBuf,
    }

    #[async_trait]
    impl AgentRunner for TransientMock {
        async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
            unimplemented!("wave loop uses run_with_context")
        }

        async fn run_with_context(
            &self,
            _agent_name: &str,
            _task: &str,
            ctx: &RunContext,
        ) -> Result<AgentOutput> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First call: simulate a transient 5xx.
                return Err(anyhow::anyhow!(
                    "API returned status 500: internal server error"
                ));
            }
            // Second call: succeed and write the file.
            if let Some(path) = &ctx.assigned_file {
                let dest = self.out_dir.join(path);
                if let Some(parent) = dest.parent() {
                    tokio::fs::create_dir_all(parent).await.ok();
                }
                tokio::fs::write(&dest, b"# ok\n").await.ok();
            }
            Ok(AgentOutput {
                content: "success after retry".into(),
                summary: Some("ok".into()),
                usage: TokenUsage::default(),
            })
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().to_path_buf();

    let assignments_json = r#"{
            "error_convention": "exceptions",
            "waves": [
                {
                    "wave": 1,
                    "files": [
                        {"path":"src/module.py","stub":null,"purpose":"main module"}
                    ]
                }
            ]
        }"#;
    tokio::fs::write(out_dir.join("assignments.json"), assignments_json)
        .await
        .unwrap();

    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_json = r#"{
            "name":"retry-test",
            "description":"retry on 5xx",
            "phases":[{"name":"code","agent":"eng","context_template":"{{task}}"}]
        }"#;
    tokio::fs::write(workflows_dir.join("retry-test.json"), wf_json)
        .await
        .unwrap();

    let mock = Arc::new(TransientMock {
        calls: AtomicU32::new(0),
        out_dir: out_dir.clone(),
    });
    let call_count = &mock.calls as *const AtomicU32;
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    let result = engine
        .run("retry-test", "do it".into(), Some(out_dir.clone()))
        .await;

    assert!(result.is_ok(), "expected Ok after retry, got {result:?}");
    // SAFETY: mock outlives this assertion (it's in the Arc in the engine).
    let total_calls = unsafe { (*call_count).load(Ordering::SeqCst) };
    assert_eq!(total_calls, 2, "expected 2 calls (1 fail + 1 success)");
}

/// A fatal (non-retryable) error must NOT be retried; the wave loop must
/// fail immediately after the first call.
///
/// Why: Retrying 4xx errors wastes time and could mask misconfiguration.
/// What: Mock runner always returns "status 400 bad request"; assert
/// exactly one call is made and the error propagates.
/// Test: this function.
#[tokio::test]
async fn wave_loop_does_not_retry_fatal_error() {
    use std::sync::atomic::{AtomicU32, Ordering};

    struct FatalMock {
        calls: AtomicU32,
    }

    #[async_trait]
    impl AgentRunner for FatalMock {
        async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
            unimplemented!()
        }

        async fn run_with_context(&self, _: &str, _: &str, _: &RunContext) -> Result<AgentOutput> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(anyhow::anyhow!("status 400 bad request: invalid prompt"))
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().to_path_buf();

    let assignments_json = r#"{
            "error_convention": "exceptions",
            "waves": [
                {
                    "wave": 1,
                    "files": [
                        {"path":"src/fail.py","stub":null,"purpose":"will fail"}
                    ]
                }
            ]
        }"#;
    tokio::fs::write(out_dir.join("assignments.json"), assignments_json)
        .await
        .unwrap();

    let workflows_dir = tmp.path().join("workflows");
    tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
    let wf_json = r#"{
            "name":"fatal-test",
            "description":"no retry on 4xx",
            "phases":[{"name":"code","agent":"eng","context_template":"{{task}}"}]
        }"#;
    tokio::fs::write(workflows_dir.join("fatal-test.json"), wf_json)
        .await
        .unwrap();

    let mock = Arc::new(FatalMock {
        calls: AtomicU32::new(0),
    });
    let call_count = &mock.calls as *const AtomicU32;
    let engine = WorkflowEngine::new(mock, workflows_dir.clone());
    let result = engine
        .run("fatal-test", "do it".into(), Some(out_dir.clone()))
        .await;

    assert!(result.is_err(), "expected Err for fatal error");
    let total_calls = unsafe { (*call_count).load(Ordering::SeqCst) };
    assert_eq!(total_calls, 1, "fatal error must not be retried");
}

/// #231: After all transient retries on the base model fail, the wave
/// loop must make ONE more attempt using the elevation model.
///
/// Why: Engineer agents start on Sonnet for cost; some hard files require
/// Opus. Elevation lets the harness automatically retry on a stronger
/// model after repeated failures rather than requiring operator
/// intervention.
/// What: Mock runner always returns transient 5xx. With
/// `elevation_threshold=2` and `elevation_model="claude-opus-4-6"`, after
/// MAX_WAVE_RETRIES+1 base-model attempts fail, the runner must be called
/// once more with `ctx.model = Some("claude-opus-4-6")`.
/// Test: this function.
#[tokio::test]
async fn elevation_triggers_after_n_failures() {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct ElevatingMock {
        calls: AtomicU32,
        seen_models: Mutex<Vec<Option<String>>>,
    }

    #[async_trait]
    impl AgentRunner for ElevatingMock {
        async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
            unimplemented!("elevation test uses run_with_context")
        }

        async fn run_with_context(
            &self,
            _agent_name: &str,
            _task: &str,
            ctx: &RunContext,
        ) -> Result<AgentOutput> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen_models.lock().unwrap().push(ctx.model.clone());
            // If the runner sees the elevated model, succeed; otherwise
            // simulate transient 5xx errors so the retry loop exhausts.
            if ctx.model.as_deref() == Some("claude-opus-4-6") {
                Ok(AgentOutput {
                    content: "elevated success".into(),
                    summary: Some("ok".into()),
                    usage: TokenUsage::default(),
                })
            } else {
                Err(anyhow::anyhow!(
                    "API returned status 503: service unavailable"
                ))
            }
        }
    }

    let mock = ElevatingMock {
        calls: AtomicU32::new(0),
        seen_models: Mutex::new(Vec::new()),
    };

    let ctx = RunContext::default();
    // No sleep needed — but the retry helper does sleep with exponential
    // backoff. Use tokio's pause/auto-advance to keep the test fast.
    tokio::time::pause();
    let handle = tokio::spawn(async move {
        let result = run_wave_file_with_retry(
            &mock,
            "engineer",
            "build it",
            &ctx,
            Some(2),
            Some("claude-opus-4-6"),
        )
        .await;
        (result, mock)
    });
    // Auto-advance virtual time so the backoff sleeps complete instantly.
    // Loop a few times; total of 6s of virtual time covers 2s + 4s backoffs.
    for _ in 0..10 {
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
    }
    let (result, mock) = handle.await.unwrap();

    assert!(
        result.is_ok(),
        "expected elevation to succeed, got {result:?}"
    );
    let total = mock.calls.load(Ordering::SeqCst);
    // 3 base-model attempts (initial + 2 retries) + 1 elevated retry = 4
    assert_eq!(total, 4, "expected 3 base + 1 elevated call, got {total}");

    let seen = mock.seen_models.lock().unwrap();
    assert_eq!(
        seen.len(),
        4,
        "expected 4 recorded model overrides, got {}",
        seen.len()
    );
    // First three calls: no override (base model).
    assert!(seen[0].is_none() && seen[1].is_none() && seen[2].is_none());
    // Final call: elevated.
    assert_eq!(seen[3].as_deref(), Some("claude-opus-4-6"));
}

/// #222: When `assignments_dir` and `code_target` diverge, the
/// reconcile step must read the manifest from `assignments_dir` and
/// move misrouted files into `code_target` (the user's project tree),
/// not back into `assignments_dir`.
///
/// Why: This locks in the invariant that `--out-dir` (artifacts) and
/// `--project-dir` (code) stay separated. A regression that confused
/// the two would silently put generated source files back under
/// `out/` — exactly the bug #222 tracks.
/// What: Plants a divergent layout (project_root, code_target,
/// assignments_dir all distinct), with one file misrouted at
/// project_root, and asserts the reconciler relocates it to
/// `code_target`, NOT `assignments_dir`.
/// Test: This test.
#[tokio::test]
async fn reconcile_code_outputs_against_divergent_dirs() {
    // Note: `reconcile_code_outputs_against` reads CWD as project_root.
    // To keep the test deterministic we exercise the divergence by
    // writing the stray file at the real CWD's relative location and
    // then cleaning up. This mirrors what the wave-loop sees in
    // production where claude-code anchors writes at the git repo root.
    let tmp = tempfile::tempdir().unwrap();
    let assignments_dir = tmp.path().join("artifacts");
    let code_target = tmp.path().join("project");
    tokio::fs::create_dir_all(&assignments_dir).await.unwrap();
    tokio::fs::create_dir_all(&code_target).await.unwrap();

    // Sanity: divergent paths.
    assert_ne!(assignments_dir, code_target);

    // assignments.json with one declared file under a uniquely-named
    // subdirectory so this test can't collide with anything actually
    // present in the test runner's CWD.
    let unique = format!("oss_222_test_{}", uuid::Uuid::new_v4().simple());
    let rel_path = format!("{unique}/foo.py");
    let asg_body = format!(
        r#"{{"error_convention":"exceptions","waves":[{{"wave":1,"files":[{{"path":"{rel_path}","stub":null,"purpose":"test","depends_on":[],"max_lines":100}}]}}]}}"#,
        rel_path = rel_path
    );
    tokio::fs::write(assignments_dir.join("assignments.json"), asg_body)
        .await
        .unwrap();

    // Plant the misrouted file at the real CWD (= project_root for
    // `reconcile_code_outputs_against`). Use a unique subdirectory so
    // the test is isolated; clean up regardless of outcome.
    let cwd = std::env::current_dir().unwrap();
    let stray = cwd.join(&rel_path);
    tokio::fs::create_dir_all(stray.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&stray, b"# misrouted").await.unwrap();

    // Act.
    let result = reconcile_code_outputs_against(&assignments_dir, &code_target).await;

    // Cleanup unique subdir at CWD regardless of outcome.
    let cleanup_dir = cwd.join(&unique);
    let _ = tokio::fs::remove_dir_all(&cleanup_dir).await;

    result.expect("reconciliation should succeed");

    // Assert: file landed in `code_target`, NOT `assignments_dir`.
    let code_path = code_target.join(&rel_path);
    let artifacts_path = assignments_dir.join(&rel_path);
    assert!(
        code_path.is_file(),
        "#222: file should be in code_target (project dir), got missing at {}",
        code_path.display()
    );
    assert!(
        !artifacts_path.exists(),
        "#222: file must NOT land in assignments_dir (out_dir / artifacts)"
    );
}
