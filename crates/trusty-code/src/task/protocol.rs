//! `task.run` JSON-RPC method (#2056, vision spec §4.3): start a
//! session-driven task execution.
//!
//! Why: this is the API surface the M1 control-plane cut line needs — a
//! caller creates (or targets an existing) session and asks the daemon to
//! actually EXECUTE a task against it, asynchronously, streaming progress via
//! the #2054/#2055 `session.attach` path. Kept as its own JSON-RPC method
//! (rather than overloading `session.send`) because `session.send`'s
//! existing #2054/#2055 contract — record an observable `SessionInput` event
//! — is already tested and shipped; `task.run` is the deliberate, explicit
//! "start executing" entry point the issue names.
//! What: [`register`] wires `task.run` onto a [`Router`], sharing the SAME
//! `Arc<SessionRegistry>` every `session.*` method uses. Params match the
//! vision spec §4.3 example almost verbatim: `task_description`,
//! `agent_name` (top-level/PM agent, default `"pm"`), `context` (accepted for
//! forward-compatibility; not yet injected into the prompt — project
//! `CLAUDE.md` context already flows through `run_task`'s existing
//! machinery, and free-form per-call `context` injection is a smaller
//! follow-up, not core to this cut line), `model_override` (pins the
//! delegated ENGINEER's model for this run), and an ADDITIONAL optional
//! `session_id` for "sessionful" execution against an already-`session.create`d
//! session (the spec's "single-shot or sessionful" phrasing).
//! Test: `task::protocol::tests::*`; the full flow end-to-end (a real
//! subprocess) in `tests/task_e2e.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::jsonrpc::{ConnectionContext, Router, RpcError};
use crate::session::SessionRegistry;

use super::executor::{TaskRunParams, spawn_task_run};
use super::mock_llm::build_llm_client;

/// Register `task.run` onto `router`.
///
/// Why: the one place that wires the method, mirroring
/// `session::protocol::register`'s role for `session.*`.
/// What: closes over `registry`, `project`, and `agents_dir` — all shared,
/// cheap to clone (`Arc`/`PathBuf`) per call.
/// Test: `task::protocol::tests::register_wires_task_run`.
pub fn register(
    router: &mut Router,
    registry: Arc<SessionRegistry>,
    project: PathBuf,
    agents_dir: PathBuf,
) {
    router.register("task.run", move |params: Value, _ctx: ConnectionContext| {
        let registry = Arc::clone(&registry);
        let project = project.clone();
        let agents_dir = agents_dir.clone();
        async move { task_run(registry, params, project, agents_dir).await }
    });
}

/// `task.run` request params (vision spec §4.3).
#[derive(Deserialize)]
struct TaskRunRequestParams {
    task_description: String,
    #[serde(default)]
    agent_name: Option<String>,
    /// Accepted for forward-compatibility; not yet injected into the
    /// assembled prompt — see the module docs.
    #[serde(default)]
    #[allow(dead_code)]
    context: Option<String>,
    #[serde(default)]
    model_override: Option<String>,
    /// #2056 extension beyond the spec's literal example: when present,
    /// runs against an EXISTING session (created via `session.create`)
    /// instead of minting a fresh one — the spec's "sessionful" execution
    /// mode.
    #[serde(default)]
    session_id: Option<String>,
}

/// `task.run(task_description, agent_name?, context?, model_override?,
/// session_id?) -> { session_id, status }`.
///
/// Why: the single entry point that turns a request into a running
/// background execution.
/// What: validates `task_description` is non-empty
/// (`-32003 invalid_argument`); resolves the target session — an existing
/// one by `session_id` (propagating `session_not_found` if it doesn't
/// exist) or a freshly `session.create`d one; builds the shared LLM client
/// (real or the #2056 offline mock, per `TCODE_MOCK_LLM`); and calls
/// `spawn_task_run`, which reserves the execution slot synchronously
/// (rejecting a second overlapping run) before handing off to the
/// background task. Returns immediately — the caller `session.attach`es to
/// observe progress, per the ticket's "must not block the request thread on
/// the whole LLM run" requirement.
/// Test: `task::protocol::tests::task_run_rejects_empty_task_description`,
/// `task::protocol::tests::task_run_creates_session_when_none_given`,
/// `task::protocol::tests::task_run_sessionful_reuses_existing_session`,
/// `task::protocol::tests::task_run_unknown_session_id_errors`.
async fn task_run(
    registry: Arc<SessionRegistry>,
    params: Value,
    project: PathBuf,
    agents_dir: PathBuf,
) -> Result<Value, RpcError> {
    let p: TaskRunRequestParams = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("task.run: {e}")))?;
    if p.task_description.trim().is_empty() {
        return Err(RpcError::invalid_argument(
            "task_description must not be empty",
        ));
    }
    let agent_name = p.agent_name.unwrap_or_else(|| "pm".to_string());

    let session_id = match &p.session_id {
        Some(id) => {
            registry.status(id)?; // propagate session_not_found verbatim
            id.clone()
        }
        None => {
            let session = registry.create(
                p.task_description.clone(),
                Some(agent_name.clone()),
                Some(project.display().to_string()),
            );
            session.id
        }
    };

    let llm = build_llm_client()?;
    let task_params = TaskRunParams {
        session_id: session_id.clone(),
        task: p.task_description,
        agent_name,
        project,
        agents_dir,
        model_override: p.model_override,
    };
    spawn_task_run(registry, llm, task_params)?;

    Ok(json!({ "session_id": session_id, "status": "running" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonrpc::Request;
    use tokio::sync::mpsc;

    fn test_ctx() -> ConnectionContext {
        let (tx, _rx) = mpsc::unbounded_channel();
        ConnectionContext::new(tx)
    }

    fn agents_dir() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("agents tempdir");
        std::fs::write(
            tmp.path().join("pm.toml"),
            "[agent]\nname = \"pm\"\nmodel = \"openai/gpt-4o-mini\"\n[system_prompt]\ncontent = \"You are the PM.\"\n",
        )
        .expect("write pm.toml");
        std::fs::write(
            tmp.path().join("python-engineer.toml"),
            "[agent]\nname = \"python-engineer\"\nmodel = \"deepseek/deepseek-chat\"\n[system_prompt]\ncontent = \"engineer\"\n",
        )
        .expect("write python-engineer.toml");
        tmp
    }

    /// `register` must wire `task.run` so the router recognises it.
    #[tokio::test]
    async fn register_wires_task_run() {
        let _guard = super::super::mock_llm::MOCK_LLM_ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by `ENV_LOCK` above.
        unsafe {
            std::env::set_var(
                super::super::mock_llm::MOCK_LLM_ENV,
                super::super::mock_llm::MOCK_LLM_ECHO,
            );
        }
        let registry = Arc::new(SessionRegistry::new());
        let agents = agents_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let mut router = Router::new();
        register(
            &mut router,
            registry,
            project.path().to_path_buf(),
            agents.path().to_path_buf(),
        );

        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "task.run".to_string(),
            params: Some(json!({"task_description": "say hi"})),
        };
        let resp = router.dispatch(req, &test_ctx()).await;
        unsafe {
            std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
        }
        assert!(
            resp.error.is_none(),
            "task.run should succeed, got {:?}",
            resp.error
        );
        assert_eq!(resp.result.unwrap()["status"], "running");
    }

    /// An empty `task_description` must map to `-32003 invalid_argument`.
    #[tokio::test]
    async fn task_run_rejects_empty_task_description() {
        let registry = Arc::new(SessionRegistry::new());
        let agents = agents_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let err = task_run(
            registry,
            json!({"task_description": "   "}),
            project.path().to_path_buf(),
            agents.path().to_path_buf(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32003);
    }

    /// Omitting `session_id` must mint a fresh session.
    #[tokio::test]
    async fn task_run_creates_session_when_none_given() {
        let _guard = super::super::mock_llm::MOCK_LLM_ENV_LOCK.lock().await;
        unsafe {
            std::env::set_var(
                super::super::mock_llm::MOCK_LLM_ENV,
                super::super::mock_llm::MOCK_LLM_ECHO,
            );
        }
        let registry = Arc::new(SessionRegistry::new());
        let agents = agents_dir();
        let project = tempfile::tempdir().expect("project tempdir");

        let result = task_run(
            Arc::clone(&registry),
            json!({"task_description": "say hi"}),
            project.path().to_path_buf(),
            agents.path().to_path_buf(),
        )
        .await;
        unsafe {
            std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
        }
        let value = result.expect("task.run should succeed");
        let session_id = value["session_id"].as_str().expect("session_id string");
        assert!(
            registry.status(session_id).is_ok(),
            "a new session must have been created"
        );
    }

    /// A `session_id` that does not exist must propagate `session_not_found`.
    #[tokio::test]
    async fn task_run_unknown_session_id_errors() {
        let registry = Arc::new(SessionRegistry::new());
        let agents = agents_dir();
        let project = tempfile::tempdir().expect("project tempdir");

        let err = task_run(
            registry,
            json!({"task_description": "say hi", "session_id": "does-not-exist"}),
            project.path().to_path_buf(),
            agents.path().to_path_buf(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32007);
    }

    /// A `session_id` for an existing session must reuse it, not mint a new
    /// one.
    #[tokio::test]
    async fn task_run_sessionful_reuses_existing_session() {
        let _guard = super::super::mock_llm::MOCK_LLM_ENV_LOCK.lock().await;
        unsafe {
            std::env::set_var(
                super::super::mock_llm::MOCK_LLM_ENV,
                super::super::mock_llm::MOCK_LLM_ECHO,
            );
        }
        let registry = Arc::new(SessionRegistry::new());
        let existing = registry.create("say hi".to_string(), None, None);
        let agents = agents_dir();
        let project = tempfile::tempdir().expect("project tempdir");

        let result = task_run(
            Arc::clone(&registry),
            json!({"task_description": "say hi", "session_id": existing.id}),
            project.path().to_path_buf(),
            agents.path().to_path_buf(),
        )
        .await;
        unsafe {
            std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
        }
        let value = result.expect("task.run should succeed");
        assert_eq!(value["session_id"], existing.id);
    }
}
