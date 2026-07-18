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

use crate::binding::ProjectBinding;
use crate::jsonrpc::{ConnectionContext, Router, RpcError};
use crate::session::SessionRegistry;

use super::executor::{TaskRunParams, spawn_task_run};
use super::mock_llm::build_llm_client;

/// Register `task.run` onto `router`.
///
/// Why: the one place that wires the method, mirroring
/// `session::protocol::register`'s role for `session.*`.
/// What: closes over `registry`, `binding`, and `agents_dir` — all shared,
/// cheap to clone (`Arc`/`PathBuf`) per call. `binding` replaces what was a
/// REQUIRED `project: PathBuf`: a `PathBuf` cannot express "no project", so the
/// projectless state the shell's entry screen renders was unreachable — the
/// daemon could not be started, let alone run a task, without one. It is now a
/// [`ProjectBinding`], whose `None` variant is that state.
/// Test: `task::protocol::tests::register_wires_task_run`,
/// `task::protocol::tests::register_wires_task_run_projectless`.
pub fn register(
    router: &mut Router,
    registry: Arc<SessionRegistry>,
    binding: ProjectBinding,
    agents_dir: PathBuf,
) {
    router.register("task.run", move |params: Value, _ctx: ConnectionContext| {
        let registry = Arc::clone(&registry);
        let binding = binding.clone();
        let agents_dir = agents_dir.clone();
        async move { task_run(registry, params, binding, agents_dir).await }
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
    /// #2059: per-call `HarnessMode` override (vision spec §5.9's tier-2
    /// precedence source, above `.claude/settings.json` and below
    /// `TRUSTY_CODE_MODE`). Leniently parsed by
    /// `crate::mode::resolve_mode` — an unrecognised string here degrades
    /// to "this source does not contribute", NOT a request error.
    #[serde(default)]
    mode: Option<String>,
    /// #2207: per-call wall-clock deadline override, in seconds, applied to
    /// BOTH the PM's own loop and the delegated engineer's loop. `None`
    /// falls through to `crate::provider::resolve_deadline_secs`'s env-var
    /// (`TCODE_RUN_DEADLINE_SECONDS`) and default (1800s) tiers.
    #[serde(default)]
    deadline_secs: Option<u64>,
}

/// `task.run(task_description, agent_name?, context?, model_override?,
/// session_id?, mode?, deadline_secs?) -> { session_id, status, mode }`.
///
/// Why: the single entry point that turns a request into a running
/// background execution.
/// What: validates `task_description` is non-empty
/// (`-32003 invalid_argument`); resolves the target session — an existing
/// one by `session_id` (propagating `session_not_found` if it doesn't
/// exist) or a freshly `session.create`d one; resolves the effective
/// `HarnessMode` via `crate::mode::resolve_mode` (#2059's three-tier
/// precedence) and persists it onto the session (`SessionRegistry::set_mode`)
/// so it is queryable afterward via `session.status`/`session.list`
/// (`Session.mode`) and `session.get_transcript` (`TranscriptRecord.mode`);
/// builds the shared LLM client (real or the #2056 offline mock, per
/// `TCODE_MOCK_LLM`); and calls `spawn_task_run`, which reserves the
/// execution slot synchronously (rejecting a second overlapping run) before
/// handing off to the background task. Returns immediately — the caller
/// `session.attach`es to observe progress, per the ticket's "must not block
/// the request thread on the whole LLM run" requirement. The response's own
/// `mode` field is the SAME resolved value, surfaced immediately rather than
/// requiring a follow-up `session.status` call.
/// Test: `task::protocol::tests::task_run_rejects_empty_task_description`,
/// `task::protocol::tests::task_run_creates_session_when_none_given`,
/// `task::protocol::tests::task_run_sessionful_reuses_existing_session`,
/// `task::protocol::tests::task_run_unknown_session_id_errors`,
/// `task::protocol::tests::task_run_resolves_and_reports_mode`.
async fn task_run(
    registry: Arc<SessionRegistry>,
    params: Value,
    binding: ProjectBinding,
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
                binding.clone(),
            );
            session.id
        }
    };

    let mode = crate::mode::resolve_mode(p.mode.as_deref(), binding.root());
    registry.set_mode(&session_id, mode)?;

    let llm = build_llm_client()?;
    let task_params = TaskRunParams {
        session_id: session_id.clone(),
        task: p.task_description,
        agent_name,
        binding: binding.clone(),
        agents_dir,
        model_override: p.model_override,
        mode,
        deadline_secs: p.deadline_secs,
    };
    spawn_task_run(registry, llm, task_params)?;

    Ok(json!({
        "session_id": session_id,
        "status": "running",
        "mode": mode.as_str(),
        "binding": binding.to_json(),
    }))
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
            tmp.path().join("pm.md"),
            "---\nname: pm\nmodel: openai/gpt-4o-mini\n---\n\nYou are the PM.\n",
        )
        .expect("write pm.md");
        std::fs::write(
            tmp.path().join("python-engineer.md"),
            "---\nname: python-engineer\nmodel: deepseek/deepseek-chat\n---\n\nengineer\n",
        )
        .expect("write python-engineer.md");
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
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
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

    /// `task.run` must be callable with NO project bound at all (AC-16.1) —
    /// the change that makes the shell's entry screen (7a) implementable. This
    /// call was impossible before: `register` took a required `PathBuf`.
    #[tokio::test]
    async fn register_wires_task_run_projectless() {
        let _guard = super::super::mock_llm::MOCK_LLM_ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by the lock above.
        unsafe {
            std::env::set_var(
                super::super::mock_llm::MOCK_LLM_ENV,
                super::super::mock_llm::MOCK_LLM_ECHO,
            );
        }
        let registry = Arc::new(SessionRegistry::new());
        let agents = agents_dir();
        let mut router = Router::new();
        register(
            &mut router,
            registry,
            ProjectBinding::None,
            agents.path().to_path_buf(),
        );

        let req = Request {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "task.run".to_string(),
            params: Some(json!({"task_description": "just chat"})),
        };
        let resp = router.dispatch(req, &test_ctx()).await;
        unsafe {
            std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
        }
        assert!(
            resp.error.is_none(),
            "projectless task.run must succeed, got {:?}",
            resp.error
        );
        let result = resp.result.expect("a result");
        assert_eq!(result["status"], "running");
        assert_eq!(
            result["binding"]["state"], "projectless",
            "task.run must report the projectless binding back to the caller: {result}"
        );
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
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
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
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
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

    /// `task.run` must resolve `HarnessMode` per §5.9's three-tier
    /// precedence and report it BOTH in its own immediate response AND on
    /// the session (queryable via `session.status`/`get_transcript`
    /// afterward) — #2059.
    ///
    /// Why: this is the integration point proving the wiring
    /// `crate::mode::resolve_mode`'s own unit tests cannot: that
    /// `task::protocol::task_run` actually reads the `mode` request param,
    /// the project's `.claude/settings.json`, and `TRUSTY_CODE_MODE`, in
    /// that precedence, and persists the result via
    /// `SessionRegistry::set_mode` before spawning the run.
    /// What: covers default (nothing set), settings.json alone,
    /// task-param-over-settings.json, and env-var-over-everything.
    /// Test: this test.
    #[tokio::test]
    async fn task_run_resolves_and_reports_mode() {
        let _mock_guard = super::super::mock_llm::MOCK_LLM_ENV_LOCK.lock().await;
        let _mode_guard = crate::mode::MODE_ENV_LOCK.lock().await;
        // SAFETY: test-only env mutation; serialized by both locks above.
        unsafe {
            std::env::set_var(
                super::super::mock_llm::MOCK_LLM_ENV,
                super::super::mock_llm::MOCK_LLM_ECHO,
            );
            std::env::remove_var(crate::mode::MODE_ENV_VAR);
        }
        let agents = agents_dir();

        // 1. Nothing set anywhere -> default (daily-driver).
        let project = tempfile::tempdir().expect("project tempdir");
        let registry = Arc::new(SessionRegistry::new());
        let value = task_run(
            Arc::clone(&registry),
            json!({"task_description": "say hi"}),
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
            agents.path().to_path_buf(),
        )
        .await
        .expect("task.run should succeed");
        assert_eq!(value["mode"], "daily-driver");
        let session_id = value["session_id"].as_str().unwrap().to_string();
        assert_eq!(
            registry.status(&session_id).unwrap().mode,
            Some(crate::mode::HarnessMode::DailyDriver)
        );

        // 2. `.claude/settings.json` alone sets parity.
        let project2 = tempfile::tempdir().expect("project tempdir");
        std::fs::create_dir_all(project2.path().join(".claude")).expect("mkdir");
        std::fs::write(
            project2.path().join(".claude").join("settings.json"),
            r#"{"code_harness": {"mode": "parity"}}"#,
        )
        .expect("write settings.json");
        let registry2 = Arc::new(SessionRegistry::new());
        let value2 = task_run(
            Arc::clone(&registry2),
            json!({"task_description": "say hi"}),
            ProjectBinding::resolve(Some(project2.path().to_path_buf()))
                .expect("tempdir must bind"),
            agents.path().to_path_buf(),
        )
        .await
        .expect("task.run should succeed");
        assert_eq!(value2["mode"], "parity");

        // 3. A `mode` request param overrides settings.json (still parity
        //    from settings.json here, but requesting daily-driver must win).
        let registry3 = Arc::new(SessionRegistry::new());
        let value3 = task_run(
            Arc::clone(&registry3),
            json!({"task_description": "say hi", "mode": "daily-driver"}),
            ProjectBinding::resolve(Some(project2.path().to_path_buf()))
                .expect("tempdir must bind"),
            agents.path().to_path_buf(),
        )
        .await
        .expect("task.run should succeed");
        assert_eq!(
            value3["mode"], "daily-driver",
            "task.run's mode param must override settings.json"
        );

        // 4. TRUSTY_CODE_MODE overrides EVERYTHING, including a task param.
        unsafe {
            std::env::set_var(crate::mode::MODE_ENV_VAR, "parity");
        }
        let registry4 = Arc::new(SessionRegistry::new());
        let value4 = task_run(
            Arc::clone(&registry4),
            json!({"task_description": "say hi", "mode": "daily-driver"}),
            ProjectBinding::resolve(Some(project2.path().to_path_buf()))
                .expect("tempdir must bind"),
            agents.path().to_path_buf(),
        )
        .await
        .expect("task.run should succeed");
        assert_eq!(
            value4["mode"], "parity",
            "TRUSTY_CODE_MODE must win over a task.run mode param"
        );

        unsafe {
            std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
            std::env::remove_var(crate::mode::MODE_ENV_VAR);
        }
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
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
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
        let existing = registry.create(
            "say hi".to_string(),
            None,
            crate::binding::ProjectBinding::None,
        );
        let agents = agents_dir();
        let project = tempfile::tempdir().expect("project tempdir");

        let result = task_run(
            Arc::clone(&registry),
            json!({"task_description": "say hi", "session_id": existing.id}),
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
            agents.path().to_path_buf(),
        )
        .await;
        unsafe {
            std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
        }
        let value = result.expect("task.run should succeed");
        assert_eq!(value["session_id"], existing.id);
    }

    /// A second `task.run` at the JSON-RPC entry point, issued AFTER the
    /// first run against the same `session_id` has fully `Finished`, must be
    /// ACCEPTED (#2344 resumption) rather than erroring — the acceptance
    /// criterion "two sequential task.run calls on one session" exercised at
    /// the actual RPC surface, not just `spawn_task_run` directly
    /// (`task::executor::tests::spawn_task_run_second_call_after_finish_appends_to_cumulative_transcript`
    /// covers the transcript-accumulation half; this test covers that
    /// `task_run` itself — including its `registry.status(id)` existing-
    /// session lookup — does not reject the resumed call).
    #[tokio::test]
    async fn task_run_second_call_after_finish_continues_the_session() {
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

        let first = task_run(
            Arc::clone(&registry),
            json!({"task_description": "say hi"}),
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
            agents.path().to_path_buf(),
        )
        .await
        .expect("first task.run should succeed");
        let session_id = first["session_id"].as_str().unwrap().to_string();

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        loop {
            let status = registry.status(&session_id).expect("session must exist");
            if status.status.is_terminal() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "first run did not reach a terminal state within 5s"
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
        }

        let second = task_run(
            Arc::clone(&registry),
            json!({"task_description": "say hi again", "session_id": session_id}),
            ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
            agents.path().to_path_buf(),
        )
        .await;
        unsafe {
            std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
        }
        let value = second.expect("a second task.run on a Finished session must be accepted");
        assert_eq!(value["session_id"], session_id);
        assert_eq!(value["status"], "running");
    }
}
