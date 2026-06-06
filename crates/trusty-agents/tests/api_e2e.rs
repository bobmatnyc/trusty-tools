//! End-to-end tests for the trusty-agents HTTP API (#183).
//!
//! Why: The unit tests in `src/api/server.rs` exercise the axum router via
//! `tower::ServiceExt::oneshot`, which bypasses the actual TCP listener and
//! the subprocess-spawning code path. To catch regressions in argument
//! parsing, port binding, child-process plumbing, and the live JSON shape
//! over the wire we need tests that hit a real `trusty-agents --api` process.
//! What: Spawns the compiled binary via the `ApiServer` test helper, sends
//! HTTP requests, and asserts the responses. Tests that require an LLM API
//! key are marked `#[ignore]` so a no-credentials `cargo test` still passes;
//! they can be opted into with `cargo test --test api_e2e -- --ignored`.
//! Test: `cargo test --test api_e2e` (non-ignored) and
//! `cargo test --test api_e2e -- --ignored` (full suite, requires API key).

mod support;

use std::time::Duration;

use serde_json::Value;
use support::api_server::ApiServer;

/// Helper: pull an `OPENROUTER_API_KEY` (or equivalent) into env from
/// `.env.local` so locally-run `--ignored` tests pick up credentials the
/// same way the binary does. No-op if the file is absent.
fn load_env_local() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let _ = dotenvy::from_path(manifest.join(".env.local"));
}

/// Smoke test: `/api/health` returns 200 with status=ok and a semver-shaped
/// version string.
#[tokio::test]
async fn test_health_returns_version() {
    let server = ApiServer::spawn().await.expect("spawn api server");
    let resp: Value = reqwest::get(format!("{}/api/health", server.base_url()))
        .await
        .expect("GET /api/health")
        .json()
        .await
        .expect("parse health body");
    assert_eq!(resp["status"], "ok", "body={resp}");
    let v = resp["version"].as_str().expect("version is a string");
    assert!(v.contains('.'), "expected semver-shaped version, got {v}");
}

/// `/api/tasks` on a freshly-started server returns an empty array.
#[tokio::test]
async fn test_tasks_list_starts_empty() {
    let server = ApiServer::spawn().await.expect("spawn api server");
    let resp: Value = reqwest::get(format!("{}/api/tasks", server.base_url()))
        .await
        .expect("GET /api/tasks")
        .json()
        .await
        .expect("parse tasks body");
    let arr = resp.as_array().expect("tasks body is an array");
    assert!(arr.is_empty(), "expected empty array, got {resp}");
}

/// Submitting a task returns 202 with `{ id, status: "running" }` and the
/// id immediately becomes pollable via `/api/task/:id`.
///
/// Why: This validates the full POST -> store -> GET path without waiting
/// for the LLM-backed subprocess to finish. The background subprocess will
/// fail (no API key in CI) but the server still records a `failed`
/// PmResponse — that's fine; we only assert that *something* gets stored.
#[tokio::test]
async fn test_submit_task_then_poll() {
    let server = ApiServer::spawn().await.expect("spawn api server");
    // Submit with a tiny timeout-bounded wait. We don't need terminal
    // status — we only need to confirm the id is round-trippable.
    let id = server
        .submit_task("smoke-test: this task is allowed to fail")
        .await
        .expect("submit task");
    assert!(!id.is_empty(), "id should be non-empty");

    // Immediately poll once: the response may still be `running` (placeholder
    // hasn't been replaced yet) or already `failed` (if the subprocess
    // exited fast). Either way, the id MUST resolve.
    let url = format!("{}/api/task/{id}", server.base_url());
    let v: Value = reqwest::get(&url)
        .await
        .expect("GET /api/task/:id")
        .json()
        .await
        .expect("parse task body");
    assert_eq!(v["id"], id, "echoed id should match submitted; body={v}");
    let status = v["status"].as_str().unwrap_or("");
    assert!(
        ["running", "failed", "success", "partial"].contains(&status),
        "unexpected status {status}; body={v}"
    );
}

/// Unknown task ids return 404.
#[tokio::test]
async fn test_unknown_task_id_returns_404() {
    let server = ApiServer::spawn().await.expect("spawn api server");
    let resp = reqwest::get(format!("{}/api/task/not-a-real-task-id", server.base_url()))
        .await
        .expect("GET /api/task/:id");
    assert_eq!(resp.status().as_u16(), 404);
}

// -------- LLM-backed tests (require OPENROUTER_API_KEY / ANTHROPIC_API_KEY) --

/// CTRL chat: dispatch a conversational message via `--direct ctrl`.
///
/// Why: Validates that the API can route a request to the CTRL agent and
/// receive a non-error response. CTRL is exercised here as a single-agent
/// `--direct` invocation rather than a workflow.
/// Test: Marked `#[ignore]` because it spends real LLM tokens.
#[tokio::test]
#[ignore = "requires OPENROUTER_API_KEY (or ANTHROPIC_API_KEY)"]
async fn test_ctrl_chat_hello() {
    load_env_local();
    let server = ApiServer::spawn().await.expect("spawn api server");
    let id = server
        .submit_task_json(serde_json::json!({
            "task": "hello, please reply with a short greeting",
            "agent": "ctrl",
        }))
        .await
        .expect("submit ctrl task");

    let result = server
        .wait_for_task_with_timeout(&id, Duration::from_secs(180))
        .await
        .expect("wait for ctrl task");
    assert_ne!(result["status"], "failed", "ctrl chat failed: {result}");
    let narrative = result["narrative"].as_str().unwrap_or("");
    assert!(
        !narrative.is_empty(),
        "ctrl should have produced narrative content; body={result}"
    );
}

/// CTRL self-knowledge: can answer a question about its own project.
///
/// Why: Validates the CTRL agent has access to self-project tools / context.
#[tokio::test]
#[ignore = "requires OPENROUTER_API_KEY (or ANTHROPIC_API_KEY)"]
async fn test_ctrl_knows_self_project() {
    load_env_local();
    let server = ApiServer::spawn().await.expect("spawn api server");
    let id = server
        .submit_task_json(serde_json::json!({
            "task": "what project are you running inside? answer briefly.",
            "agent": "ctrl",
        }))
        .await
        .expect("submit ctrl self-project task");
    let result = server
        .wait_for_task_with_timeout(&id, Duration::from_secs(180))
        .await
        .expect("wait for ctrl task");
    assert_ne!(result["status"], "failed", "ctrl failed: {result}");
}

/// CTRL connecting to an external test project: spin up a separate
/// `Project` tempdir and ask CTRL to inspect it via `project_path`.
#[tokio::test]
#[ignore = "requires OPENROUTER_API_KEY (or ANTHROPIC_API_KEY)"]
async fn test_ctrl_connects_to_test_project() {
    load_env_local();
    let project = support::project::Project::new();
    let server = ApiServer::spawn().await.expect("spawn api server");

    let task = format!(
        "you are connected to a project at {}. List the agent names you find under .trusty-agents/agents/.",
        project.root.path().display()
    );
    let id = server
        .submit_task_json(serde_json::json!({
            "task": task,
            "agent": "ctrl",
            "project_path": project.root.path().to_string_lossy(),
        }))
        .await
        .expect("submit ctrl project-aware task");

    let result = server
        .wait_for_task_with_timeout(&id, Duration::from_secs(240))
        .await
        .expect("wait for ctrl task");
    assert_ne!(result["status"], "failed", "ctrl failed: {result}");
    let narrative = result["narrative"].as_str().unwrap_or("");
    assert!(
        !narrative.is_empty(),
        "ctrl should have produced project info; body={result}"
    );
}
