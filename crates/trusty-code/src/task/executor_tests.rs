//! Tests for #2056's background task-execution orchestration. Split out of
//! `executor.rs` per the crate's `_tests.rs` sibling-file convention (see
//! `intent::classifier_tests` for precedent) to keep the production file
//! under the 500-SLOC cap.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::agent_loop::with_cadence_env;
use crate::llm::{ChatRequest, ChatResponse, LlmClientTrait, LlmError};
use crate::session::{SessionRegistry, SessionStatus};
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
        binding: crate::binding::ProjectBinding::resolve(Some(project.path().to_path_buf()))
            .expect("tempdir must bind"),
        agents_dir: agents.path().to_path_buf(),
        model_override: None,
        mode: crate::mode::HarnessMode::default(),
        deadline_secs: None,
    }
}

/// A second `spawn_task_run` against the SAME session before the first
/// finishes must be rejected — the core "no overlapping runs" guarantee.
#[tokio::test]
async fn spawn_task_run_rejects_second_overlapping_run() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
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
        binding: crate::binding::ProjectBinding::resolve(Some(empty_agents.path().to_path_buf()))
            .expect("tempdir must bind"),
        agents_dir: empty_agents.path().to_path_buf(),
        model_override: None,
        mode: crate::mode::HarnessMode::default(),
        deadline_secs: None,
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

    assert!(
        daily_driver_skills_catalog(&p, p.binding.root().expect("bound in this test")).is_none()
    );
}

/// `daily_driver_skills_catalog` returns `None` when the project has no
/// `.claude/skills/` directory at all, even in `HarnessMode::DailyDriver`.
#[test]
fn daily_driver_skills_catalog_none_when_no_skills_dir() {
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let mut p = params(&agents, &project, "s");
    p.mode = crate::mode::HarnessMode::DailyDriver;

    assert!(
        daily_driver_skills_catalog(&p, p.binding.root().expect("bound in this test")).is_none()
    );
}

/// A project's `.claude/settings.json` `code_harness.cadence_turns` override
/// reaches `resolve_cadence_config` exactly the way `run_and_record` calls it
/// (#2346) — the same `params.project` path a real `spawn_task_run` would use.
///
/// Why: #2346's acceptance criteria explicitly call for an integration-style
/// proof that the settings.json precedence chain changes `cadence_turns`, not
/// just a unit test isolated to `agent_loop::cadence`'s own module (see
/// `cadence::tests::resolve_cadence_config_settings_json_override` for that
/// unit-level coverage) — this test exercises the SAME `TaskRunParams.project`
/// shape `run_and_record`'s `cadence: Some(resolve_cadence_config(&params.project))`
/// wiring consumes.
/// What: Build a project `TempDir` (via the same `params` helper every other
/// executor test uses) with a `.claude/settings.json` overriding
/// `cadence_turns` to `3`; assert `crate::agent_loop::resolve_cadence_config`
/// against `p.project` returns `3`, not the built-in default of `8`.
/// Test: this test.
///
/// Hermeticity: `resolve_cadence_config` reads the process-global
/// `TCODE_CADENCE_TURNS` env var, which the sibling
/// `crate::agent_loop::cadence::tests::resolve_cadence_config_env_wins_over_settings_json`
/// test sets to `"5"` while it runs. This test therefore resolves under
/// `with_cadence_env(None, None, …)` — holding `CADENCE_ENV_LOCK` and forcing
/// the env var unset — so a concurrent env-setting test cannot bleed its
/// `TCODE_CADENCE_TURNS=5` into this resolver and make the `== 3` assertion
/// observe `5` (fixing the pre-existing flake this test guards against).
#[tokio::test]
async fn settings_json_cadence_turns_override_reaches_resolver() {
    with_cadence_env(None, None, || {
        let agents = agents_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::create_dir_all(project.path().join(".claude")).expect("mkdir .claude");
        std::fs::write(
            project.path().join(".claude").join("settings.json"),
            r#"{"code_harness": {"cadence_turns": 3}}"#,
        )
        .expect("write settings.json");

        let p = params(&agents, &project, "s");
        let cfg = crate::agent_loop::resolve_cadence_config(
            p.binding.root().expect("bound in this test"),
        );
        assert_eq!(cfg.cadence_turns, 3);
        assert_ne!(
            cfg.cadence_turns,
            crate::agent_loop::CadenceConfig::default().cadence_turns
        );
    })
    .await;
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

    let (catalog, resolver) =
        daily_driver_skills_catalog(&p, p.binding.root().expect("bound in this test"))
            .expect("catalog present");
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

// ── #2207/#2206: daemon-path deadline wiring + distinct status + telemetry ─────

/// A response in which the assistant calls `finish_task` with a required
/// field (`summary`) missing — recoverable per #2072's schema-validation
/// path, NOT terminal (mirrors `run_task::tests::malformed_finish_task_response`).
fn malformed_finish_task_response() -> Value {
    json!({
        "id": "gen-finish-malformed",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-missing",
                    "type": "function",
                    "function": {
                        "name": "finish_task",
                        "arguments": json!({"status": "completed"}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 40, "completion_tokens": 10, "total_tokens": 50}
    })
}

/// A response in which the assistant emits final text and stops.
fn stop_response(text: &str) -> Value {
    json!({
        "id": "gen-stop",
        "choices": [{
            "message": { "role": "assistant", "content": text, "tool_calls": [] },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 15, "completion_tokens": 5, "total_tokens": 20 }
    })
}

/// An `LlmClientTrait` that sleeps before its Nth `chat` call, then replays a
/// scripted response (mirrors `run_task::tests::DeadlineTriggerLlm`).
///
/// Why: deterministically drives the PM's own wall-clock deadline past its
/// configured budget, entirely within the PM's own loop (no delegation), so
/// the daemon path's `run_and_record` observes a genuine
/// `AgentLoopError::Timeout` rather than racing against the delegated
/// engineer's own independently-resolved deadline.
struct DeadlineTriggerLlm {
    responses: Vec<ChatResponse>,
    cursor: AtomicUsize,
    stall_at_call: usize,
    stall_for: std::time::Duration,
}

impl DeadlineTriggerLlm {
    fn new(fixtures: &[Value], stall_at_call: usize, stall_for: std::time::Duration) -> Self {
        let responses = fixtures
            .iter()
            .map(|v| serde_json::from_value(v.clone()).expect("valid ChatResponse fixture"))
            .collect();
        Self {
            responses,
            cursor: AtomicUsize::new(0),
            stall_at_call,
            stall_for,
        }
    }
}

#[async_trait]
impl LlmClientTrait for DeadlineTriggerLlm {
    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        if idx == self.stall_at_call {
            tokio::time::sleep(self.stall_for).await;
        }
        match self.responses.get(idx) {
            Some(resp) => Ok(resp.clone()),
            None => Err(LlmError::MissingConfig(format!(
                "scripted LLM exhausted at call {idx}"
            ))),
        }
    }
}

/// A tiny `deadline_secs` override on the daemon path yields
/// `SessionStatus::DeadlineExceeded` (distinct from `Failed`), and the
/// persisted transcript/usage still reflect the turn that completed before
/// the deadline fired (#2207 + #2206's daemon-path equivalent of
/// `run_task::tests::exit_code_reflects_deadline_exceeded_distinct_from_run_failure`).
///
/// Why: `task::executor::run_and_record` calls `registry.set_run_outcome`
/// unconditionally before branching on the loop's `result` (#2206 was
/// already correct here — this test pins that #2207's new
/// `SessionStatus::DeadlineExceeded` arm doesn't regress it), and the
/// deadline must actually reach the PM's `AgentLoopConfig` via
/// `resolve_deadline_secs(params.deadline_secs)`.
/// What: `deadline_secs: Some(1)`; turn 0 is a malformed `finish_task` call
/// (instant, recoverable, recorded with real usage), turn 1 sleeps 3s. Assert
/// the session ends `DeadlineExceeded` (not `Failed`) and its stored usage is
/// non-zero.
/// Test: this test.
#[tokio::test]
async fn spawn_task_run_deadline_exceeded_is_distinct_and_preserves_usage() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let llm: Arc<dyn LlmClientTrait> = Arc::new(DeadlineTriggerLlm::new(
        &[
            malformed_finish_task_response(),
            stop_response("recovered (never reached in time)"),
        ],
        1,
        std::time::Duration::from_secs(3),
    ));

    let mut p = params(&agents, &project, &session.id);
    p.deadline_secs = Some(1);

    spawn_task_run(Arc::clone(&registry), llm, p).expect("run must start");

    // Poll for the run to reach a terminal state naturally (via its own 1s
    // deadline), WITHOUT `shutdown_executions` — that flips every tracked
    // execution's cancel flag immediately, which would race the deadline and
    // spuriously report `Cancelled` instead of `DeadlineExceeded`.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let status = registry.status(&session.id).expect("session must exist");
        if status.status.is_terminal() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "run did not reach a terminal state within 5s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let status = registry.status(&session.id).expect("session must exist");
    assert_eq!(
        status.status,
        SessionStatus::DeadlineExceeded,
        "a deadline hit must map to DeadlineExceeded, not Failed"
    );

    let transcript = registry
        .get_transcript(&session.id)
        .expect("transcript must exist");
    assert!(
        transcript.usage.prompt_tokens > 0,
        "the completed first turn must still contribute real usage, got {:?}",
        transcript.usage
    );
    assert!(
        transcript.cost_usd.is_some(),
        "cost must be populated (not None) on the deadline-exceeded path"
    );
}

// ── #2344: persistent session-scoped transcript across task.run calls ──────────

/// Poll `registry.status(id)` until it reaches a terminal state, bounded by
/// a 5s deadline (mirrors the inline poll loop in
/// `spawn_task_run_deadline_exceeded_is_distinct_and_preserves_usage`).
async fn wait_for_terminal(registry: &SessionRegistry, id: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let status = registry.status(id).expect("session must exist");
        if status.status.is_terminal() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "run did not reach a terminal state within 5s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// A SECOND `spawn_task_run` on the SAME session, issued AFTER the first run
/// has already `Finished`, must be ACCEPTED (not rejected as terminal, #2344)
/// and must APPEND onto the first run's turns/usage rather than overwrite
/// them — this is #2344's headline acceptance criterion exercised through
/// the real daemon wiring (`spawn_task_run` -> `run_and_record` ->
/// `SessionRegistry::begin_pm_transcript`/`begin_execution` resumption /
/// `set_run_outcome` accumulation), not just the unit-level registry/loop
/// tests.
///
/// Why: unit tests already cover each collaborator (`begin_pm_transcript`,
/// `begin_execution`'s `Finished`-resumption, `set_run_outcome`
/// accumulation, `AgentLoop::run_with_transcript`'s no-reseed/output-scoping
/// behaviour) in isolation; this test is the integration proof that
/// `task::executor` actually wires them together correctly end to end.
/// What: run 1 completes via the `EchoLlmClient` script; poll to terminal;
/// snapshot `get_transcript`. Run 2 (a FRESH `EchoLlmClient`, its own script
/// from call 0 — the daemon builds a new LLM client per `task.run`
/// regardless of session) targets the SAME `session_id`; assert
/// `spawn_task_run` accepts it (not `-32003` terminal-session rejection);
/// poll to terminal again; assert the session's cumulative turn count grew,
/// run 1's turns are still present unchanged at the front, and usage
/// accumulated (run 2's usage > 0 on top of run 1's).
/// Test: this test.
#[tokio::test]
async fn spawn_task_run_second_call_after_finish_appends_to_cumulative_transcript() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    // Run 1: completes the full delegate -> bash -> stop -> stop script.
    let llm1: Arc<dyn LlmClientTrait> = Arc::new(EchoLlmClient::new());
    let p1 = params(&agents, &project, &session.id);
    spawn_task_run(Arc::clone(&registry), llm1, p1).expect("run 1 must start");
    wait_for_terminal(&registry, &session.id).await;
    assert_eq!(
        registry.status(&session.id).unwrap().status,
        SessionStatus::Finished
    );

    let after_run_one = registry
        .get_transcript(&session.id)
        .expect("transcript must exist after run 1");
    assert!(
        !after_run_one.turns.is_empty(),
        "run 1 must have recorded turns"
    );

    // Run 2 against the SAME session_id, issued only AFTER run 1 fully
    // finished — must be ACCEPTED, not rejected as an already-terminal
    // session.
    let llm2: Arc<dyn LlmClientTrait> = Arc::new(EchoLlmClient::new());
    let mut p2 = params(&agents, &project, &session.id);
    p2.task = "do something else".to_string();
    spawn_task_run(Arc::clone(&registry), llm2, p2)
        .expect("a Finished session must accept a follow-up task.run (#2344)");
    wait_for_terminal(&registry, &session.id).await;
    assert_eq!(
        registry.status(&session.id).unwrap().status,
        SessionStatus::Finished,
        "run 2 must also finish successfully"
    );

    let after_run_two = registry
        .get_transcript(&session.id)
        .expect("transcript must exist after run 2");
    assert!(
        after_run_two.turns.len() > after_run_one.turns.len(),
        "run 2 must APPEND more turns onto run 1's, not replace them: {} vs {}",
        after_run_two.turns.len(),
        after_run_one.turns.len()
    );
    assert_eq!(
        after_run_two.turns[..after_run_one.turns.len()],
        after_run_one.turns[..],
        "run 1's turns must still be present, unchanged, at the front of the cumulative list"
    );
    assert!(
        after_run_two.usage.prompt_tokens > after_run_one.usage.prompt_tokens,
        "usage must accumulate across runs, not reset: run1={:?} run2={:?}",
        after_run_one.usage,
        after_run_two.usage
    );
}

/// A scripted `LlmClientTrait` that records the tool-schema names of every
/// request it receives, then immediately stops (no tool calls) — the D3
/// no-tool-call convention `finish_task`'s own docs describe, so a run
/// completes in a single PM turn with no delegation needed.
///
/// Why: #2348's `recall_session` registration is a daemon-session-path-only
/// concern (`task::executor::run_and_record`'s `pm_registry`, never
/// `run_task::execute_run_task`'s). Asserting on the ACTUAL tool schemas the
/// PM's `AgentLoop` sends the LLM (rather than re-deriving the registration
/// logic in the test) proves the real wiring, not a duplicate of it.
struct SchemaCapturingLlm {
    tool_names: Mutex<Vec<String>>,
}

impl SchemaCapturingLlm {
    fn new() -> Self {
        Self {
            tool_names: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl LlmClientTrait for SchemaCapturingLlm {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let names = req
            .tools
            .as_ref()
            .map(|tools| tools.iter().map(|t| t.function.name.clone()).collect())
            .unwrap_or_default();
        *self.tool_names.lock().expect("tool_names lock") = names;
        let fixture = json!({
            "id": "mock-stop",
            "choices": [{
                "message": {"role": "assistant", "content": "done", "tool_calls": []},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });
        serde_json::from_value(fixture).map_err(|e| LlmError::MissingConfig(e.to_string()))
    }
}

/// The daemon-session path's `pm_registry` (`task::executor::run_and_record`)
/// must register `recall_session` (#2348) alongside `delegate_to_agent` and
/// `finish_task`.
#[tokio::test]
async fn session_path_registers_recall_session_tool() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let mock = Arc::new(SchemaCapturingLlm::new());
    let llm: Arc<dyn LlmClientTrait> = Arc::clone(&mock) as Arc<dyn LlmClientTrait>;
    let p = params(&agents, &project, &session.id);

    spawn_task_run(Arc::clone(&registry), llm, p).expect("run must start");
    wait_for_terminal(&registry, &session.id).await;

    let names = mock.tool_names.lock().expect("tool_names lock").clone();
    assert!(
        names.contains(&"recall_session".to_string()),
        "daemon-session path must register recall_session; got {names:?}"
    );
    assert!(
        names.contains(&"finish_task".to_string()),
        "sanity: finish_task must still be registered alongside it; got {names:?}"
    );
}
