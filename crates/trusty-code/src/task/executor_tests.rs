//! Tests for #2056's background task-execution orchestration. Split out of
//! `executor.rs` per the crate's `_tests.rs` sibling-file convention (see
//! `intent::classifier_tests` for precedent) to keep the production file
//! under the 500-SLOC cap.

use std::sync::Arc;

use tempfile::TempDir;

use super::*;
use crate::llm::LlmClientTrait;
use crate::perf::TokenUsage;
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

/// Per-role pricing must charge the PM's turns at `pm_model` and the
/// engineer's at `engineer_model`, not blend everything under one slug —
/// the #1475 concern this function exists to avoid reintroducing.
#[test]
fn aggregate_usage_per_role_prices_each_role_separately() {
    let turns = vec![
        TurnRecord {
            role: "pm".to_string(),
            model: "anthropic/claude-sonnet-4-5".to_string(),
            text: String::new(),
            tool_calls: vec![],
            usage: TokenUsage::new(1000, 500, 0, 0),
        },
        TurnRecord {
            role: "python-engineer".to_string(),
            model: "anthropic/claude-haiku-4".to_string(),
            text: String::new(),
            tool_calls: vec![],
            usage: TokenUsage::new(1000, 500, 0, 0),
        },
    ];

    let (usage, cost) = aggregate_usage_per_role(
        &turns,
        "anthropic/claude-sonnet-4-5",
        "anthropic/claude-haiku-4",
    );
    assert_eq!(usage.prompt_tokens, 2000);
    assert_eq!(usage.completion_tokens, 1000);

    // Pricing the SAME two turns entirely under the PM model must differ
    // from the per-role split whenever the two models price differently —
    // the concrete regression the per-role split guards against.
    let blended_cost = crate::perf::cost_usd("anthropic/claude-sonnet-4-5", 2000, 1000, 0, 0);
    assert_ne!(
        cost, blended_cost,
        "per-role pricing must differ from blending everything under the PM model"
    );
}

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
