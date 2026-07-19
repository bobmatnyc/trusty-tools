//! Tests for `task.run` (#2056, #3178's per-call project convergence). Split
//! out of `protocol.rs` per the crate's `_tests.rs` sibling-file convention
//! (see `executor_tests.rs` for precedent) to keep the production file under
//! the 500-SLOC cap.

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
        crate::workstreams::test_shared_store().await,
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
        crate::workstreams::test_shared_store().await,
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
        crate::workstreams::test_shared_store().await,
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
        crate::workstreams::test_shared_store().await,
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
        crate::workstreams::test_shared_store().await,
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
        ProjectBinding::resolve(Some(project2.path().to_path_buf())).expect("tempdir must bind"),
        agents.path().to_path_buf(),
        crate::workstreams::test_shared_store().await,
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
        ProjectBinding::resolve(Some(project2.path().to_path_buf())).expect("tempdir must bind"),
        agents.path().to_path_buf(),
        crate::workstreams::test_shared_store().await,
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
        ProjectBinding::resolve(Some(project2.path().to_path_buf())).expect("tempdir must bind"),
        agents.path().to_path_buf(),
        crate::workstreams::test_shared_store().await,
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
        crate::workstreams::test_shared_store().await,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32007);
}

/// Omitting `project` must keep today's process-boot-time binding
/// unchanged (#3178 back-compat) — the daemon's `binding` param, not
/// `ProjectBinding::None`.
#[tokio::test]
async fn task_run_without_project_keeps_boot_binding() {
    let _guard = super::super::mock_llm::MOCK_LLM_ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var(
            super::super::mock_llm::MOCK_LLM_ENV,
            super::super::mock_llm::MOCK_LLM_ECHO,
        );
    }
    let registry = Arc::new(SessionRegistry::new());
    let agents = agents_dir();
    let boot_project = tempfile::tempdir().expect("project tempdir");
    let boot_binding = ProjectBinding::resolve(Some(boot_project.path().to_path_buf()))
        .expect("tempdir must bind");

    let value = task_run(
        registry,
        json!({"task_description": "say hi"}),
        boot_binding.clone(),
        agents.path().to_path_buf(),
        crate::workstreams::test_shared_store().await,
    )
    .await
    .expect("task.run should succeed");
    unsafe {
        std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
    }
    assert_eq!(value["binding"], boot_binding.to_json());
}

/// A per-call `project` must override the boot-time binding for this
/// call, resolved through the same `ProjectBinding::resolve` helper
/// `session.create` uses (#3178, DOC-39 §5.5/AC-16.2).
#[tokio::test]
async fn task_run_with_project_overrides_boot_binding() {
    let _guard = super::super::mock_llm::MOCK_LLM_ENV_LOCK.lock().await;
    unsafe {
        std::env::set_var(
            super::super::mock_llm::MOCK_LLM_ENV,
            super::super::mock_llm::MOCK_LLM_ECHO,
        );
    }
    let registry = Arc::new(SessionRegistry::new());
    let agents = agents_dir();
    let call_project = tempfile::tempdir().expect("project tempdir");

    let value = task_run(
        registry,
        json!({
            "task_description": "say hi",
            "project": call_project.path().to_string_lossy(),
        }),
        ProjectBinding::None,
        agents.path().to_path_buf(),
        crate::workstreams::test_shared_store().await,
    )
    .await
    .expect("task.run should succeed");
    unsafe {
        std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
    }
    assert_eq!(
        value["binding"]["state"], "directory",
        "the per-call project must bind, not stay projectless: {value}"
    );
}

/// A `project` naming a nonexistent path must map to `-32003
/// invalid_argument`, matching `session.create`'s error mapping for the
/// same failure mode.
#[tokio::test]
async fn task_run_rejects_invalid_project() {
    let registry = Arc::new(SessionRegistry::new());
    let agents = agents_dir();

    let err = task_run(
        registry,
        json!({"task_description": "say hi", "project": "/no/such/path/anywhere"}),
        ProjectBinding::None,
        agents.path().to_path_buf(),
        crate::workstreams::test_shared_store().await,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32003);
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
        crate::workstreams::test_shared_store().await,
    )
    .await;
    unsafe {
        std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
    }
    let value = result.expect("task.run should succeed");
    assert_eq!(value["session_id"], existing.id);
}

/// `session_id` + a `project` that RESTATES the reused session's own
/// binding root must succeed (#3178, code-critic PR #3189 fix) — the
/// invariant only forbids a DIFFERENT root, never the same one.
#[tokio::test]
async fn task_run_session_id_with_matching_project_succeeds() {
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
    let session_binding =
        ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind");
    let existing = registry.create("say hi".to_string(), None, session_binding);

    let result = task_run(
        Arc::clone(&registry),
        json!({
            "task_description": "say hi",
            "session_id": existing.id,
            "project": project.path().to_string_lossy(),
        }),
        ProjectBinding::None,
        agents.path().to_path_buf(),
        crate::workstreams::test_shared_store().await,
    )
    .await;
    unsafe {
        std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
    }
    let value = result.expect("a project restating the session's own binding must succeed");
    assert_eq!(value["session_id"], existing.id);
    assert_eq!(value["binding"]["state"], "directory");
}

/// `session_id` + a `project` naming a DIFFERENT root than the reused
/// session's own persisted binding must be rejected with `-32003
/// invalid_argument` (#3178, code-critic HIGH finding, PR #3189) — never
/// silently executed against a project `session.status`/`session.list`
/// would never agree the session is bound to.
#[tokio::test]
async fn task_run_session_id_with_mismatched_project_is_rejected() {
    let registry = Arc::new(SessionRegistry::new());
    let agents = agents_dir();
    let session_project = tempfile::tempdir().expect("session project tempdir");
    let other_project = tempfile::tempdir().expect("other project tempdir");
    let session_binding = ProjectBinding::resolve(Some(session_project.path().to_path_buf()))
        .expect("tempdir must bind");
    let existing = registry.create("say hi".to_string(), None, session_binding);

    let err = task_run(
        registry,
        json!({
            "task_description": "say hi",
            "session_id": existing.id,
            "project": other_project.path().to_string_lossy(),
        }),
        ProjectBinding::None,
        agents.path().to_path_buf(),
        crate::workstreams::test_shared_store().await,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32003);
    assert!(
        err.message.contains("does not match session"),
        "error message must name the mismatch: {}",
        err.message
    );
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
        crate::workstreams::test_shared_store().await,
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
        crate::workstreams::test_shared_store().await,
    )
    .await;
    unsafe {
        std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
    }
    let value = second.expect("a second task.run on a Finished session must be accepted");
    assert_eq!(value["session_id"], session_id);
    assert_eq!(value["status"], "running");
}

// -- Workstream binding (DOC-48 §4.1/§4.2, issue #3298) --

/// Seed a workstream and activate it on `workstreams`, returning its id.
async fn seed_active_workstream(
    workstreams: &crate::workstreams::SharedWorkstreamStore,
) -> crate::workstreams::WorkstreamId {
    let id = workstreams
        .lock()
        .await
        .create("active")
        .await
        .expect("create");
    crate::workstreams::activation::activate(workstreams, id, false)
        .await
        .expect("activate");
    id
}

/// `task.run` minting a NEW session with no explicit `workstream_id`, while
/// a workstream is active, must bind to the ACTIVE workstream (§4.2) and
/// publish `Event::SessionAdded`.
#[tokio::test]
async fn task_run_binds_ambient_active_workstream_and_publishes_session_added() {
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
    let workstreams = crate::workstreams::test_shared_store().await;
    let active_id = seed_active_workstream(&workstreams).await;

    let mut rx = crate::events::subscribe();
    let value = task_run(
        Arc::clone(&registry),
        json!({"task_description": "say hi"}),
        ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
        agents.path().to_path_buf(),
        workstreams.clone(),
    )
    .await;
    unsafe {
        std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
    }
    let value = value.expect("task.run should succeed");
    let session_id = value["session_id"].as_str().unwrap().to_string();

    let status = registry.status(&session_id).expect("status");
    assert_eq!(status.workstream_id, Some(active_id));

    let found = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let envelope = rx.recv().await.expect("event bus channel closed");
            if let crate::events::Event::SessionAdded {
                session_id: sid,
                workstream_id,
                ..
            } = envelope.event
                && sid == session_id
            {
                return workstream_id;
            }
        }
    })
    .await
    .expect("timed out waiting for SessionAdded");
    assert_eq!(found, active_id.to_string());
}

/// `task.run` reusing an EXISTING session that is already bound to a
/// workstream, with a `workstream_id` param naming a DIFFERENT workstream,
/// must be rejected (§4.1 AC-1.3 immutability) — mirroring the sibling
/// `project` mismatch guard.
#[tokio::test]
async fn task_run_mismatched_workstream_on_reuse_is_rejected() {
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
    let workstreams = crate::workstreams::test_shared_store().await;
    let bound_id = seed_active_workstream(&workstreams).await;

    let first = task_run(
        Arc::clone(&registry),
        json!({"task_description": "say hi"}),
        ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
        agents.path().to_path_buf(),
        workstreams.clone(),
    )
    .await
    .expect("first task.run should succeed");
    let session_id = first["session_id"].as_str().unwrap().to_string();

    let other_id = workstreams
        .lock()
        .await
        .create("other")
        .await
        .expect("create");
    let err = task_run(
        Arc::clone(&registry),
        json!({
            "task_description": "say hi again",
            "session_id": session_id,
            "workstream_id": other_id.to_string(),
        }),
        ProjectBinding::resolve(Some(project.path().to_path_buf())).expect("tempdir must bind"),
        agents.path().to_path_buf(),
        workstreams.clone(),
    )
    .await;
    unsafe {
        std::env::remove_var(super::super::mock_llm::MOCK_LLM_ENV);
    }
    let err = err.expect_err("a mismatched workstream_id on reuse must be rejected");
    assert_eq!(err.code, -32003);
    assert_ne!(bound_id, other_id);
}
