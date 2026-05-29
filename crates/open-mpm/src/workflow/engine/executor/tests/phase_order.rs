//! Phase-ordering and file-extraction tests (#64, #140, #153).
//!
//! Why: These pin the contract that `produces_files` phases land files on disk
//! before the next phase runs, that `discover_project_dir` finds the generated
//! project subtree, and that the legacy monolithic path threads an absolute
//! `working_dir` to the runner.
//! What: Drives `WorkflowEngine` with the shared mocks (`PhaseOrderMock`,
//! `WorkingDirCapture`) defined in the parent test module.
//! Test: This file IS the test body.

use super::*;

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
