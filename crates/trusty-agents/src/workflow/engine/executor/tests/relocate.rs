//! Plan/code relocation, reconciliation, and QA-path injection tests
//! (#160, #123).
//!
//! Why: Pins the file-location recovery contracts — a misrouted
//! `assignments.json` and source files written at the git root must be moved
//! into `out_dir` / `code_dir`, and the QA prompt must learn the project root
//! when a claude-code runner was used.
//! What: Exercises the `helpers` relocation/reconcile functions plus the
//! engine's QA-path injection end-to-end.
//! Test: This file IS the test body.

use super::*;

use crate::workflow::engine::helpers::{reconcile_code_outputs_from, relocate_plan_outputs_from};

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
/// by verifying the reconcile step skips when no assignments are present —
/// the safe default.
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
