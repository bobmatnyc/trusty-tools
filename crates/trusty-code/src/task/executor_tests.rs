//! Tests for #2056's background task-execution orchestration. Split out of
//! `executor.rs` per the crate's `_tests.rs` sibling-file convention (see
//! `intent::classifier_tests` for precedent) to keep the production file
//! under the 500-SLOC cap.

use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::llm::LlmClientTrait;
use crate::session::SessionRegistry;
use crate::task::mock_llm::EchoLlmClient;

/// Agents dir fixture with `pm.toml` + `python-engineer.toml` (mirrors
/// `run_task::tests::agents_dir`).
fn agents_dir() -> TempDir {
    let tmp = tempfile::tempdir().expect("agents tempdir");
    std::fs::write(
        tmp.path().join("pm.toml"),
        "[agent]\nname = \"pm\"\nmodel = \"openai/gpt-4o-mini\"\n[system_prompt]\ncontent = \"You are the PM.\"\n",
    )
    .expect("write pm.toml");
    std::fs::write(
        tmp.path().join("python-engineer.toml"),
        "[agent]\nname = \"python-engineer\"\nmodel = \"deepseek/deepseek-chat\"\n[system_prompt]\ncontent = \"You are a Python engineer.\"\n",
    )
    .expect("write python-engineer.toml");
    tmp
}

fn params(agents: &TempDir, project: &TempDir, session_id: &str) -> TaskRunParams {
    TaskRunParams {
        session_id: session_id.to_string(),
        task: "do something".to_string(),
        agent_name: "pm".to_string(),
        project: project.path().to_path_buf(),
        agents_dir: agents.path().to_path_buf(),
        model_override: None,
        mode: crate::mode::HarnessMode::default(),
    }
}

/// A second `spawn_task_run` against the SAME session before the first
/// finishes must be rejected — the core "no overlapping runs" guarantee.
#[tokio::test]
async fn spawn_task_run_rejects_second_overlapping_run() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    let llm: Arc<dyn LlmClientTrait> = Arc::new(EchoLlmClient::new());

    let p = params(&agents, &project, &session.id);

    spawn_task_run(Arc::clone(&registry), Arc::clone(&llm), p.clone())
        .expect("first run must start");
    let err = spawn_task_run(Arc::clone(&registry), llm, p)
        .expect_err("second overlapping run must be rejected");
    assert_eq!(err.code, -32003);

    // Let the background task actually finish so this test doesn't leak a
    // dangling tokio task past its own scope.
    registry
        .shutdown_executions(std::time::Duration::from_secs(5))
        .await;
}

/// `spawn_task_run` against an unknown session must error rather than
/// spawning anything.
#[tokio::test]
async fn spawn_task_run_unknown_session_errors() {
    let registry = Arc::new(SessionRegistry::new());
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    let llm: Arc<dyn LlmClientTrait> = Arc::new(EchoLlmClient::new());

    let p = params(&agents, &project, "does-not-exist");
    let err = spawn_task_run(registry, llm, p).unwrap_err();
    assert_eq!(err.code, -32007);
}

// `aggregate_usage_per_role` itself is now `run_task::aggregate_usage_per_role`
// (#2061, #1475 bug 1) — its per-role pricing behaviour is tested once, at
// its actual definition site, in `run_task::report::tests`
// (`aggregate_usage_per_role_prices_each_role_separately`). This module only
// tests THIS daemon path's own wrapper (`resolve_engineer_model`, below).

/// `resolve_engineer_model` must fall back to `"unknown"` (never panic) when
/// the engineer config is missing.
#[test]
fn resolve_engineer_model_falls_back_when_config_missing() {
    let empty_agents = tempfile::tempdir().expect("empty agents tempdir");
    let p = TaskRunParams {
        session_id: "s".to_string(),
        task: "do something".to_string(),
        agent_name: "pm".to_string(),
        project: empty_agents.path().to_path_buf(),
        agents_dir: empty_agents.path().to_path_buf(),
        model_override: None,
        mode: crate::mode::HarnessMode::default(),
    };
    let model = resolve_engineer_model(&p);
    assert_eq!(model, "unknown");
}

/// `daily_driver_skills_catalog` returns `None` under `HarnessMode::Parity`,
/// even when the project has a real `.claude/skills/` catalog (#2069's
/// scope note: "Parity mode should NOT progressively disclose").
#[test]
fn daily_driver_skills_catalog_none_in_parity() {
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    let skills_dir = project.path().join(".claude").join("skills").join("demo");
    std::fs::create_dir_all(&skills_dir).expect("mkdir skill dir");
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: Demo skill\n---\nbody\n",
    )
    .expect("write SKILL.md");

    let mut p = params(&agents, &project, "s");
    p.mode = crate::mode::HarnessMode::Parity;

    assert!(daily_driver_skills_catalog(&p).is_none());
}

/// `daily_driver_skills_catalog` returns `None` when the project has no
/// `.claude/skills/` directory at all, even in `HarnessMode::DailyDriver`.
#[test]
fn daily_driver_skills_catalog_none_when_no_skills_dir() {
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let mut p = params(&agents, &project, "s");
    p.mode = crate::mode::HarnessMode::DailyDriver;

    assert!(daily_driver_skills_catalog(&p).is_none());
}

/// `daily_driver_skills_catalog` returns the rendered catalog + a working
/// resolver under `HarnessMode::DailyDriver` when skills exist.
#[test]
fn daily_driver_skills_catalog_some_when_skills_exist() {
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    let skills_dir = project.path().join(".claude").join("skills").join("demo");
    std::fs::create_dir_all(&skills_dir).expect("mkdir skill dir");
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: Demo skill\n---\nfull body\n",
    )
    .expect("write SKILL.md");

    let mut p = params(&agents, &project, "s");
    p.mode = crate::mode::HarnessMode::DailyDriver;

    let (catalog, resolver) = daily_driver_skills_catalog(&p).expect("catalog present");
    assert!(catalog.contains("demo: Demo skill"));
    assert_eq!(resolver.resolve("demo").as_deref(), Some("full body"));
}

/// `ProjectToolFactory::build` threads the run's resolved `HarnessMode` onto
/// the engineer's `EditTool` (#2073) — under `HarnessMode::Parity`, the
/// dispatched `edit` call must prefer a supplied unified-diff payload even
/// when the calling model slug would prefer SEARCH/REPLACE under the plain
/// per-model matrix, proving the daemon path actually wires `with_mode`
/// through, not just that `EditTool` supports it in isolation
/// (`tools::fs::edit::tests::edit_under_parity_mode_prefers_unified_diff_even_for_flagship_model_slug`
/// covers the tool itself).
#[tokio::test]
async fn project_tool_factory_threads_parity_mode_into_edit_tool() {
    let project = tempfile::tempdir().expect("project tempdir");
    std::fs::write(project.path().join("f.py"), "line1\nline2\n").expect("seed file");

    let factory = ProjectToolFactory {
        project: project.path().to_path_buf(),
        mode: crate::mode::HarnessMode::Parity,
    };
    let agent = crate::agents::AgentConfig::default();
    let ctx = crate::tools::RunContext {
        model: Some("anthropic/claude-opus-4-5".to_string()),
        ..Default::default()
    };
    let registry = factory.build(&agent, &ctx).await;

    let result = registry
        .dispatch_gated(
            "edit",
            serde_json::json!({
                "path": "f.py",
                "old_string": "not-present",
                "new_string": "x",
                "diff": "@@ -2,1 +2,1 @@\n-line2\n+line2-diffed\n"
            }),
            None,
        )
        .await;

    assert!(!result.is_error(), "unexpected error: {}", result.content());
    assert!(result.content().contains("unified_diff"));
    let updated = std::fs::read_to_string(project.path().join("f.py")).expect("read");
    assert_eq!(updated, "line1\nline2-diffed\n");
}
