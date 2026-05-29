//! Wave-loop execution and package-marker pre-creation tests (#88, #150, #166).
//!
//! Why: These pin the per-file wave-loop contract — one code-agent invocation
//! per assignment in topological order, `max_turns_override` threaded via
//! `RunContext`, absolute write paths, the trigger firing only after the plan
//! phase writes `assignments.json`, and the `__init__.py` pre-creation guard.
//! What: Drives `WorkflowEngine` with `WaveLoopMock` / `PlanThenCodeMock` plus
//! the `should_precreate` / `precreate_package_markers` step-dispatch helpers.
//! Test: This file IS the test body.

use super::*;

use crate::workflow::engine::step_dispatch::{precreate_package_markers, should_precreate};

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

// ---- #150: Pre-create empty package markers ----

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
