//! End-to-end tests for `tcode run-task` (#1034, #1035) — all offline.
//!
//! Why: The closer must be provable without a live LLM or API key. Every M1
//! acceptance criterion — the PM delegates to the engineer, the engineer's
//! file change shows up in the diff, the transcript carries both roles, usage +
//! cost aggregate, exit codes reflect outcome, and the per-run `--engineer-model`
//! swap routes the engineer — is driven through a scripted `LlmClientTrait` mock
//! so no network call is ever made.
//! What: A `ScriptedLlm` replays a queue of `ChatResponse`s; because the PM and
//! engineer share one inner client, the script is consumed in call order
//! (PM-turn-1, engineer-turn-1, …). Fixtures build agents-dir TOMLs and a project
//! dir; tests assert on the returned `RunReport`.
//! Test: this module is itself the test surface.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{ExitCode, RedelegationCapSignal, RunTaskParams, execute_run_task};
use crate::agent_loop::AgentLoopError;
use crate::llm::{ChatRequest, ChatResponse, LlmClientTrait, LlmError};
use crate::tools::{AgentOutput, EngineerCompletionSignal};

// ── Scripted offline LLM ───────────────────────────────────────────────────────

/// Replays a fixed queue of responses and records every request's model.
///
/// Why: Deterministic, offline substitute for the network client shared by the
/// PM and engineer loops. Recording the per-request model lets the #1035 tests
/// assert which slug drove the engineer's turns (alongside the transcript).
/// What: Holds JSON-deserialised responses, an atomic cursor, and a log of the
/// models seen. Exhaustion yields a transport-style error so a runaway loop fails
/// loudly rather than hanging.
/// Test: Used by every test below.
struct ScriptedLlm {
    responses: Vec<ChatResponse>,
    cursor: AtomicUsize,
    models: Mutex<Vec<String>>,
    /// Every request's `max_tokens`, in call order — lets
    /// `pm_llm_max_tokens_reaches_chat_request` assert the PM's configured
    /// `[llm].max_tokens` reached the wire request rather than being silently
    /// dropped in favour of the agent-loop default (the run_task max-tokens
    /// bug this test guards against).
    max_tokens: Mutex<Vec<Option<u32>>>,
    /// Every request's advertised tool-schema names, in call order (#2348:
    /// lets `run_task_registry_never_registers_recall_session` assert the
    /// one-shot path's actual wire-level tool set, rather than re-deriving
    /// the registration logic in the test).
    tool_names: Mutex<Vec<Vec<String>>>,
}

impl ScriptedLlm {
    fn from_json(fixtures: &[Value]) -> Self {
        let responses = fixtures
            .iter()
            .map(|v| serde_json::from_value(v.clone()).expect("valid ChatResponse fixture"))
            .collect();
        Self {
            responses,
            cursor: AtomicUsize::new(0),
            models: Mutex::new(Vec::new()),
            max_tokens: Mutex::new(Vec::new()),
            tool_names: Mutex::new(Vec::new()),
        }
    }

    /// Every model slug the client was asked to use, in call order.
    fn models_seen(&self) -> Vec<String> {
        self.models.lock().expect("models lock").clone()
    }

    /// The `max_tokens` of the first recorded request (the PM's first turn).
    fn first_max_tokens(&self) -> Option<u32> {
        self.max_tokens
            .lock()
            .expect("max_tokens lock")
            .first()
            .copied()
            .expect("at least one request recorded")
    }

    /// The tool-schema names advertised on the FIRST recorded request (the
    /// PM's first turn, where its own `pm_registry` schemas are attached).
    fn first_tool_names(&self) -> Vec<String> {
        self.tool_names
            .lock()
            .expect("tool_names lock")
            .first()
            .cloned()
            .expect("at least one request recorded")
    }
}

#[async_trait]
impl LlmClientTrait for ScriptedLlm {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        self.models
            .lock()
            .expect("models lock")
            .push(req.model.clone());
        self.max_tokens
            .lock()
            .expect("max_tokens lock")
            .push(req.max_tokens);
        let names = req
            .tools
            .as_ref()
            .map(|tools| tools.iter().map(|t| t.function.name.clone()).collect())
            .unwrap_or_default();
        self.tool_names.lock().expect("tool_names lock").push(names);
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        match self.responses.get(idx) {
            Some(resp) => Ok(resp.clone()),
            None => Err(LlmError::MissingConfig(format!(
                "scripted LLM exhausted at call {idx}"
            ))),
        }
    }
}

// ── Fixture builders ───────────────────────────────────────────────────────────

/// A response where the assistant calls `delegate_to_agent(python-engineer, …)`.
fn delegate_response(task: &str) -> Value {
    json!({
        "id": "gen-delegate",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-del",
                    "type": "function",
                    "function": {
                        "name": "delegate_to_agent",
                        "arguments": json!({"agent_name": "python-engineer", "task": task}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 50, "completion_tokens": 12, "total_tokens": 62}
    })
}

/// A response where the assistant calls `write_file(path, content)`.
fn write_file_response(path: &str, content: &str) -> Value {
    json!({
        "id": "gen-write",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-wf",
                    "type": "function",
                    "function": {
                        "name": "write_file",
                        "arguments": json!({"path": path, "content": content}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 30, "completion_tokens": 8, "total_tokens": 38}
    })
}

/// A response where the assistant calls `bash(command)` (#2279).
fn bash_response(command: &str) -> Value {
    json!({
        "id": "gen-bash",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-bash",
                    "type": "function",
                    "function": {
                        "name": "bash",
                        "arguments": json!({"command": command}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 10, "total_tokens": 30}
    })
}

/// A response where the assistant calls `finish_task(status, summary)`
/// (#2279).
fn finish_task_response(status: &str, summary: &str) -> Value {
    json!({
        "id": "gen-finish",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-finish",
                    "type": "function",
                    "function": {
                        "name": "finish_task",
                        "arguments": json!({"status": status, "summary": summary}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 15, "completion_tokens": 8, "total_tokens": 23}
    })
}

/// A response where the assistant emits final text and stops.
fn stop_response(text: &str) -> Value {
    json!({
        "id": "gen-stop",
        "choices": [{
            "message": {"role": "assistant", "content": text, "tool_calls": []},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 15, "completion_tokens": 5, "total_tokens": 20}
    })
}

/// A response where the assistant emits final text and stops, ALSO
/// reporting a specific RESOLVED `model` slug — for the #1475 bug 2 test
/// (`transcript_records_resolved_model_not_requested_slug`), which needs a
/// response whose resolved model differs from what was requested.
fn stop_response_with_model(text: &str, model: &str) -> Value {
    json!({
        "id": "gen-stop",
        "model": model,
        "choices": [{
            "message": {"role": "assistant", "content": text, "tool_calls": []},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 15, "completion_tokens": 5, "total_tokens": 20}
    })
}

/// Build an agents dir with `pm.toml` and `python-engineer.toml`.
///
/// Why: `run-task` loads the PM config from `<agents_dir>/pm.toml` and the
/// engineer from `<agents_dir>/python-engineer.toml`; tests need both on disk.
/// What: Writes both TOMLs (engineer pinned to `engineer_model`) and returns the
/// tempdir.
fn agents_dir(engineer_model: &str) -> TempDir {
    let tmp = tempfile::tempdir().expect("agents tempdir");
    std::fs::write(
        tmp.path().join("pm.toml"),
        "[agent]\nname = \"pm\"\nmodel = \"openai/gpt-4o-mini\"\n[system_prompt]\ncontent = \"You are the PM.\"\n",
    )
    .expect("write pm.toml");
    std::fs::write(
        tmp.path().join("python-engineer.toml"),
        format!(
            "[agent]\nname = \"python-engineer\"\nmodel = \"{engineer_model}\"\n[system_prompt]\ncontent = \"You are a Python engineer.\"\n"
        ),
    )
    .expect("write python-engineer.toml");
    tmp
}

/// Build the standard params for a run against the given dirs.
///
/// The `engineer_model` slot carries the per-run `--engineer-model` override
/// (#1035); `None` routes the engineer via its own config model.
fn params(agents: &TempDir, project: &TempDir, engineer_model: Option<&str>) -> RunTaskParams {
    RunTaskParams {
        agent: "pm".into(),
        task: "write hello.py".into(),
        project: project.path().to_path_buf(),
        agents_dir: agents.path().to_path_buf(),
        engineer_model: engineer_model.map(str::to_string),
        deadline_secs: None,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

/// Full path: the PM delegates to the engineer, who writes a file; the diff
/// reflects that change, the transcript carries both roles, and exit is Success.
///
/// Why: The core #1034 acceptance criterion — `run-task` actually executes
/// PM→engineer end-to-end and reports the real change.
/// What: Script [PM delegate, engineer write_file, engineer stop, PM stop].
/// Run; assert exit=Success, the diff names `hello.py` with its content, and the
/// transcript has a "pm" turn and a "python-engineer" turn.
/// Test: this test.
#[tokio::test]
async fn end_to_end_pm_delegates_to_engineer() {
    let agents = agents_dir("deepseek/deepseek-chat");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        delegate_response("create hello.py"),
        write_file_response("hello.py", "print('hello from engineer')"),
        stop_response("engineer: wrote hello.py"),
        stop_response("pm: task complete"),
    ]));

    let report = execute_run_task(params(&agents, &project, None), llm).await;

    assert_eq!(
        report.exit,
        ExitCode::Success,
        "run with a file change must exit Success; diff was: {}",
        report.diff
    );
    assert!(
        report.diff.contains("hello.py"),
        "diff must name the engineer's file, got: {}",
        report.diff
    );
    assert!(
        report.diff.contains("hello from engineer"),
        "diff must contain the new content, got: {}",
        report.diff
    );

    // File actually landed in the project.
    let written = std::fs::read_to_string(project.path().join("hello.py")).expect("file written");
    assert_eq!(written, "print('hello from engineer')");

    // Transcript carries both roles.
    let roles: Vec<&str> = report.transcript.iter().map(|t| t.role.as_str()).collect();
    assert!(
        roles.contains(&"pm"),
        "transcript must have a PM turn: {roles:?}"
    );
    assert!(
        roles.contains(&"python-engineer"),
        "transcript must have an engineer turn: {roles:?}"
    );
}

/// The PM's configured `[llm].max_tokens` reaches its `ChatRequest` via
/// `execute_run_task` — the exact entry point of the run_task max-tokens bug.
///
/// Why: `execute_run_task` previously built the PM's `AgentLoopConfig` via
/// `AgentLoopConfig { model: pm_model, ..AgentLoopConfig::default() }`,
/// dropping `pm_config.llm.max_tokens` entirely so every PM turn was capped at
/// the hard-coded default regardless of what `pm.toml` declared. This is the
/// end-to-end regression guard: a `pm.toml` with `[llm].max_tokens = 8192`
/// must produce a `ChatRequest.max_tokens` of `8192`, never the old 1024.
/// What: `pm.toml` declares `[llm].max_tokens = 8192`; script a single PM stop
/// turn; assert the scripted client observed `Some(8192)`.
/// Test: this test.
#[tokio::test]
async fn pm_llm_max_tokens_reaches_chat_request() {
    let agents = tempfile::tempdir().expect("agents tempdir");
    std::fs::write(
        agents.path().join("pm.toml"),
        "[agent]\nname = \"pm\"\nmodel = \"openai/gpt-4o-mini\"\n\
         [llm]\nmax_tokens = 8192\n\
         [system_prompt]\ncontent = \"You are the PM.\"\n",
    )
    .expect("write pm.toml");
    std::fs::write(
        agents.path().join("python-engineer.toml"),
        "[agent]\nname = \"python-engineer\"\nmodel = \"deepseek/deepseek-chat\"\n[system_prompt]\ncontent = \"You are a Python engineer.\"\n",
    )
    .expect("write python-engineer.toml");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response(
        "pm: nothing to do",
    )]));

    let _report = execute_run_task(params(&agents, &project, None), llm.clone()).await;

    assert_eq!(
        llm.first_max_tokens(),
        Some(8192),
        "PM's configured [llm].max_tokens must reach its ChatRequest, not the old 1024 default"
    );
}

/// The transcript must record the RESOLVED model slug a response reports,
/// not merely the slug that was requested (#1475 bug 2).
///
/// Why: `RecordingLlmClient::chat` previously recorded `req.model` (what was
/// asked for); if the provider remaps/falls back to a different concrete
/// model, that divergence must be visible in the transcript for the #1035
/// cross-model comparison to be trustworthy.
/// What: Script the PM's FINAL (stop) response with a `model` field that
/// differs from the PM agent config's own model
/// (`openai/gpt-4o-mini` vs. the response's `openai/gpt-4o-mini-2024-07-18`);
/// assert the recorded PM turn's `model` is the response's resolved slug.
/// Test: this test.
#[tokio::test]
async fn transcript_records_resolved_model_not_requested_slug() {
    let agents = agents_dir("deepseek/deepseek-chat");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        delegate_response("create hello.py"),
        write_file_response("hello.py", "print('hi')"),
        stop_response("engineer: wrote hello.py"),
        stop_response_with_model("pm: task complete", "openai/gpt-4o-mini-2024-07-18"),
    ]));

    let report = execute_run_task(params(&agents, &project, None), llm).await;

    let pm_final_turn = report
        .transcript
        .iter()
        .rev()
        .find(|t| t.role == "pm")
        .expect("a pm turn must be recorded");
    assert_eq!(
        pm_final_turn.model, "openai/gpt-4o-mini-2024-07-18",
        "transcript must record the RESOLVED model, not the requested slug \
         (openai/gpt-4o-mini)"
    );
}

/// Usage and cost aggregate across PM and engineer turns.
///
/// Why: The report must sum token usage over the whole run (criterion c).
/// What: Same script as the happy path; assert the report's prompt/completion
/// totals equal the sum of every scripted turn's usage, and cost is computed.
/// Test: this test.
#[tokio::test]
async fn usage_and_cost_aggregate_end_to_end() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        delegate_response("create out.py"),   // 50 + 12
        write_file_response("out.py", "x=1"), // 30 + 8
        stop_response("engineer done"),       // 15 + 5
        stop_response("pm done"),             // 15 + 5
    ]));

    let report = execute_run_task(params(&agents, &project, None), llm).await;

    assert_eq!(
        report.usage.prompt_tokens,
        50 + 30 + 15 + 15,
        "prompt tokens must sum across all PM + engineer turns"
    );
    assert_eq!(
        report.usage.completion_tokens,
        12 + 8 + 5 + 5,
        "completion tokens must sum across all turns"
    );
    assert!(
        report.cost_usd.is_some_and(|c| c >= 0.0),
        "cost must be computed and non-negative"
    );
}

/// A run that changes nothing reports `NoChanges` and an empty diff.
///
/// Why: The distinct no-op exit code must fire when the engineer writes nothing.
/// What: Script [PM stop] only — the PM concludes without delegating, so no file
/// is touched. Assert exit=NoChanges and an empty diff.
/// Test: this test.
#[tokio::test]
async fn no_changes_yields_no_changes_exit() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response(
        "pm: nothing to do",
    )]));

    let report = execute_run_task(params(&agents, &project, None), llm).await;

    assert_eq!(
        report.exit,
        ExitCode::NoChanges,
        "a run that touches no files must exit NoChanges; diff: {}",
        report.diff
    );
    assert!(report.diff.trim().is_empty(), "diff must be empty");
}

/// A missing PM config is a configuration error (no panic, distinct exit code).
///
/// Why: Bad configuration must produce a faithful `ConfigError`, not a crash.
/// What: Point the params at an agents dir with no `pm.toml`; assert
/// exit=ConfigError.
/// Test: this test.
#[tokio::test]
async fn missing_pm_config_is_config_error() {
    let empty_agents = tempfile::tempdir().expect("agents tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("unused")]));

    let report = execute_run_task(
        RunTaskParams {
            agent: "pm".into(),
            task: "anything".into(),
            project: project.path().to_path_buf(),
            agents_dir: empty_agents.path().to_path_buf(),
            engineer_model: None,
            deadline_secs: None,
        },
        llm,
    )
    .await;

    assert_eq!(
        report.exit,
        ExitCode::ConfigError,
        "missing pm.toml must be a config error"
    );
}

/// A PM loop that errors (scripted client exhausts on a tool-call turn) yields
/// `RunFailure`.
///
/// Why: A runtime failure must map to the `RunFailure` exit code (criterion d).
/// What: Script only a delegate turn but no engineer responses, so the engineer
/// loop hits the exhausted client and errors, surfacing back through the PM. With
/// the PM then also exhausted, the PM loop errors. Assert exit=RunFailure.
/// Test: this test.
#[tokio::test]
async fn exit_code_reflects_run_failure() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");

    // PM delegates, but there are NO further responses: the engineer loop's first
    // chat call exhausts the script and errors; the delegate tool surfaces a
    // recoverable error; the PM's next turn also exhausts → PM loop errors.
    let llm = Arc::new(ScriptedLlm::from_json(&[delegate_response("do work")]));

    let report = execute_run_task(params(&agents, &project, None), llm).await;

    assert_eq!(
        report.exit,
        ExitCode::RunFailure,
        "an LLM/loop error must map to RunFailure"
    );
}

// ── #2207/#2206: configurable deadline + distinct status + telemetry ───────────

/// A response in which the assistant calls `finish_task` with a required
/// field (`summary`) missing — recoverable per #2072's schema-validation
/// path, NOT terminal.
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

/// An `LlmClientTrait` that sleeps before its Nth `chat` call, then delegates.
///
/// Why: Deterministically drives the PM's own wall-clock deadline past its
/// configured budget — the FIRST call (index 0) completes instantly and is
/// recorded with real usage before the deadline fires, then a later call
/// sleeps well past the remaining budget, so `AgentLoop::run`'s outer
/// `tokio::time::timeout` aborts mid-flight rather than the loop reaching a
/// clean or genuinely failed terminal state.
/// What: Wraps a `ScriptedLlm`; sleeps `stall_for` on the call whose index
/// equals `stall_at_call`, then delegates to the inner scripted client.
/// Test: `exit_code_reflects_deadline_exceeded_distinct_from_run_failure`.
struct DeadlineTriggerLlm {
    inner: ScriptedLlm,
    counter: AtomicUsize,
    stall_at_call: usize,
    stall_for: std::time::Duration,
}

#[async_trait]
impl LlmClientTrait for DeadlineTriggerLlm {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let idx = self.counter.fetch_add(1, Ordering::SeqCst);
        if idx == self.stall_at_call {
            tokio::time::sleep(self.stall_for).await;
        }
        self.inner.chat(req).await
    }
}

/// A tiny configured deadline yields `DeadlineExceeded` — distinct from
/// `RunFailure` — AND the report still carries non-zero usage/cost from the
/// turn(s) that DID complete before the deadline fired (#2207 + #2206).
///
/// Why: This is the exact scenario #2207 found blocking the M3 bake-off: a
/// real, otherwise-productive run cut off mid-flight must be distinguishable
/// from a genuine crash, and its accrued cost/usage must not be silently
/// zeroed (#2206) — the bake-off runner needs real telemetry on EVERY
/// terminal path, not just the clean one.
/// What: `deadline_secs: Some(1)`. The scenario stays entirely within the
/// PM's OWN loop (no delegation) to avoid a race against the delegated
/// engineer's own independently-resolved deadline: turn 0 is a malformed
/// `finish_task` call (instant, recoverable, recorded with real usage), then
/// turn 1 sleeps 3s — well past the 1s budget — so the PM's own outer
/// `tokio::time::timeout` fires before that second turn ever completes.
/// Assert `exit == DeadlineExceeded` (not `RunFailure`) and `usage`/`cost_usd`
/// are non-zero/`Some`.
/// Test: this test.
#[tokio::test]
async fn exit_code_reflects_deadline_exceeded_distinct_from_run_failure() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(DeadlineTriggerLlm {
        inner: ScriptedLlm::from_json(&[
            malformed_finish_task_response(),
            stop_response("recovered (never reached in time)"),
        ]),
        counter: AtomicUsize::new(0),
        stall_at_call: 1,
        stall_for: std::time::Duration::from_secs(3),
    });

    let mut task_params = params(&agents, &project, None);
    task_params.deadline_secs = Some(1);

    let started = std::time::Instant::now();
    let report = execute_run_task(task_params, llm).await;
    let elapsed = started.elapsed();

    assert_eq!(
        report.exit,
        ExitCode::DeadlineExceeded,
        "a deadline hit must map to DeadlineExceeded, not RunFailure"
    );
    assert!(
        report.usage.prompt_tokens > 0,
        "the PM's completed first turn must still contribute real usage, got {:?}",
        report.usage
    );
    assert!(
        report.cost_usd.is_some(),
        "cost must be populated (not None) on the deadline-exceeded path"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "the 1s deadline must fire well before the mock's 3s delay, elapsed={elapsed:?}"
    );

    let rendered = report.render_json();
    let parsed: Value = serde_json::from_str(&rendered).expect("report JSON must parse");
    assert_eq!(parsed["status"], "deadline_exceeded");
}

/// A generous configured deadline does NOT prematurely kill a normal run
/// (#2207).
///
/// Why: The companion regression guard — raising the deadline (e.g. for the
/// M3 bake-off's L2/L3 multi-hour tasks) must not itself break a run that
/// would otherwise complete quickly.
/// What: `deadline_secs: Some(5)` against the standard instant PM->engineer
/// happy path; assert `exit == Success`.
/// Test: this test.
#[tokio::test]
async fn generous_deadline_does_not_abort_a_normal_run() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        delegate_response("create j.py"),
        write_file_response("j.py", "y=2"),
        stop_response("engineer done"),
        stop_response("pm done"),
    ]));

    let mut task_params = params(&agents, &project, None);
    task_params.deadline_secs = Some(5);

    let report = execute_run_task(task_params, llm).await;

    assert_eq!(
        report.exit,
        ExitCode::Success,
        "a generous deadline must not abort a run that finishes well within budget"
    );
}

/// JSON render of the report is clean, parseable, and complete.
///
/// Why: `--json` mode must emit machine-readable output; the report is its source.
/// What: Run the happy path, render JSON, parse it back, assert status + diff +
/// transcript presence.
/// Test: this test.
#[tokio::test]
async fn report_json_is_parseable() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        delegate_response("create j.py"),
        write_file_response("j.py", "y=2"),
        stop_response("engineer done"),
        stop_response("pm done"),
    ]));

    let report = execute_run_task(params(&agents, &project, None), llm).await;
    let rendered = report.render_json();
    let parsed: Value = serde_json::from_str(&rendered).expect("report JSON must parse");

    assert_eq!(parsed["status"], "success");
    assert!(parsed["diff"].as_str().unwrap().contains("j.py"));
    assert!(parsed["transcript"].as_array().unwrap().len() >= 2);
}

// ── #1035: per-run engineer model swap ──────────────────────────────────────────

/// `engineer_model` override routes the engineer's turns to the given slug while
/// the PM model stays fixed.
///
/// Why: The #1035 acceptance criterion — a per-run `--engineer-model` reroutes
/// only the engineer; the PM model is unchanged.
/// What: Engineer config pins `deepseek/deepseek-chat`, but the run overrides it
/// to `anthropic/claude-haiku-4-5`. Assert the engineer transcript turns carry the
/// override slug and the PM turns carry the PM model.
/// Test: this test.
#[tokio::test]
async fn engineer_model_swap_routes_engineer() {
    let agents = agents_dir("deepseek/deepseek-chat");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        delegate_response("create s.py"),
        write_file_response("s.py", "z=3"),
        stop_response("engineer done"),
        stop_response("pm done"),
    ]));

    let report = execute_run_task(
        params(&agents, &project, Some("anthropic/claude-haiku-4-5")),
        llm,
    )
    .await;

    // Engineer turns must carry the OVERRIDE slug, not the config slug.
    let eng_models: Vec<&str> = report
        .transcript
        .iter()
        .filter(|t| t.role == "python-engineer")
        .map(|t| t.model.as_str())
        .collect();
    assert!(!eng_models.is_empty(), "expected engineer turns");
    assert!(
        eng_models
            .iter()
            .all(|m| *m == "anthropic/claude-haiku-4-5"),
        "engineer turns must route to the override slug, got: {eng_models:?}"
    );

    // PM turns must keep the PM model.
    let pm_models: Vec<&str> = report
        .transcript
        .iter()
        .filter(|t| t.role == "pm")
        .map(|t| t.model.as_str())
        .collect();
    assert!(
        pm_models.iter().all(|m| *m == "openai/gpt-4o-mini"),
        "PM turns must keep the PM model, got: {pm_models:?}"
    );
}

/// Two runs with different `--engineer-model` slugs route the engineer to each.
///
/// Why: Criterion (a) of #1035 — distinct slugs route distinctly across runs.
/// What: Run twice with two different override slugs; assert each run's engineer
/// turns carry its own slug.
/// Test: this test.
#[tokio::test]
async fn two_runs_route_engineer_to_distinct_slugs() {
    for slug in ["openai/gpt-4o", "anthropic/claude-sonnet-4-6"] {
        let agents = agents_dir("deepseek/deepseek-chat");
        let project = tempfile::tempdir().expect("project tempdir");
        let llm = Arc::new(ScriptedLlm::from_json(&[
            delegate_response("create m.py"),
            write_file_response("m.py", "v=1"),
            stop_response("engineer done"),
            stop_response("pm done"),
        ]));

        let report = execute_run_task(params(&agents, &project, Some(slug)), llm.clone()).await;

        let eng_models: Vec<String> = report
            .transcript
            .iter()
            .filter(|t| t.role == "python-engineer")
            .map(|t| t.model.clone())
            .collect();
        assert!(
            eng_models.iter().all(|m| m == slug),
            "engineer must route to {slug}, got: {eng_models:?}"
        );
        // Confirm the model log on the scripted client also saw the slug.
        assert!(
            llm.models_seen().iter().any(|m| m == slug),
            "scripted client must have seen {slug}"
        );
    }
}

// ── #2265: re-delegation cap, reuse-aware Llm hint, partial status ─────────────

/// Repeated `AgentLoopError::Llm` failures from the engineer trigger the
/// re-delegation cap (fix #1) — NOT the PM's own turn cap — producing a
/// report that names the cap explicitly, using only a couple of PM turns
/// regardless of how many internal engineer retries occurred (fix #3,
/// decoupled design).
///
/// Why: This is the exact regression #2265 closes: before this fix, each
/// PM-driven retry could burn up to 40 engineer turns (#2233) while the PM
/// itself had only 8 attempts to notice and react, so a recurring retryable
/// Bedrock/transport error blew up session turn counts (~40 to ~111) and hit
/// the PM's OWN turn cap, reported as an opaque `run_failure`. Now the cap in
/// `run_task::redelegation` governs retry count entirely inside ONE
/// `delegate_to_agent` dispatch.
/// What: Script ONLY the PM's initial `delegate_to_agent` call — every
/// subsequent `chat` call (every engineer retry attempt, and the PM's own
/// follow-up turn) exhausts the scripted queue and returns
/// `LlmError::MissingConfig`, i.e. a retryable `AgentLoopError::Llm` per
/// fix #2. No file is ever written, so the report is a genuine `RunFailure`
/// (no deliverable) — but assert its `task` text names "re-delegation limit
/// reached" (the fix #1 label), not a bare/opaque failure, and that at most 2
/// PM-role transcript turns were recorded — proof the engineer's internal
/// retries never touched the PM's own turn budget.
/// Test: this test.
#[tokio::test]
async fn repeated_llm_errors_trigger_redelegation_cap_not_pm_turn_cap() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(ScriptedLlm::from_json(&[delegate_response("do work")]));

    let report = execute_run_task(params(&agents, &project, None), llm).await;

    assert_eq!(
        report.exit,
        ExitCode::RunFailure,
        "no deliverable was ever produced, so this must stay a genuine failure"
    );
    assert!(
        report.task.contains("re-delegation limit reached"),
        "the report must name the re-delegation cap (fix #1), not an opaque \
         failure, got: {}",
        report.task
    );

    let pm_turns = report.transcript.iter().filter(|t| t.role == "pm").count();
    assert!(
        pm_turns <= 2,
        "the PM must need only a couple of turns regardless of how many internal \
         engineer retries occurred — decoupling proof (fix #3), got {pm_turns} PM turns"
    );
}

/// `assemble_report` maps a PM-loop `TurnCapExceeded` error to the new
/// `Partial` status (fix #4) — NOT `RunFailure` — when a non-empty diff shows
/// a deliverable was actually produced.
///
/// Why: This is fix #4's core contract: trusty-code cannot generically verify
/// correctness, so a turn-cap hit must not discard a working, on-disk
/// solution as an opaque crash purely because the PM ran out of turns.
/// What: Calls the crate-private `assemble_report` directly with a
/// `TurnCapExceeded` `pm_result` and a before/after `Snapshot::Files` pair
/// that differ (one new file). Assert `exit == ExitCode::Partial`, the
/// rendered diff is non-empty, and the task label mentions the turn cap.
/// Test: this test.
#[test]
fn assemble_report_maps_turn_cap_exceeded_with_deliverable_to_partial() {
    let params = RunTaskParams {
        agent: "pm".into(),
        task: "write hello.py".into(),
        project: PathBuf::from("/tmp/does-not-matter"),
        agents_dir: PathBuf::from("/tmp/does-not-matter-agents"),
        engineer_model: None,
        deadline_secs: None,
    };
    let transcript: super::SharedTranscript = Arc::new(Mutex::new(Vec::new()));

    let before = super::diff::Snapshot::Files(std::collections::BTreeMap::new());
    let mut after_map = std::collections::BTreeMap::new();
    after_map.insert("hello.py".to_string(), "print('hi')".to_string());
    let after = super::diff::Snapshot::Files(after_map);

    let pm_result: Result<AgentOutput, AgentLoopError> = Err(AgentLoopError::TurnCapExceeded {
        max_turns: 8,
        partial: Box::new(AgentOutput::from_content("partial pm output")),
    });

    let signal = RedelegationCapSignal::new();
    let completion = EngineerCompletionSignal::new();
    let report = super::assemble_report(
        &params,
        &transcript,
        "openai/gpt-4o-mini",
        "openai/gpt-4o-mini",
        super::RunOutcome {
            before,
            after,
            pm_result,
        },
        &signal,
        &completion,
    );

    assert_eq!(
        report.exit,
        ExitCode::Partial,
        "a TurnCapExceeded PM error with a real deliverable must map to Partial, \
         not RunFailure (fix #4)"
    );
    assert!(
        report.diff.contains("hello.py"),
        "the deliverable's diff must still be rendered, got: {}",
        report.diff
    );
    assert!(
        report.task.contains("turn cap"),
        "the label must explain the turn-cap condition, got: {}",
        report.task
    );
}

/// The mirror negative case: a `TurnCapExceeded` PM error with an EMPTY diff
/// (no deliverable at all) stays `RunFailure` — the gate is purely "was work
/// produced", per fix #4's requirement to not weaken the genuine-failure path.
///
/// Why: Guards against over-broadly reclassifying every turn-cap hit as
/// `Partial` regardless of whether anything was actually produced.
/// What: Same as the positive test, but before == after (no change). Assert
/// `exit == ExitCode::RunFailure`.
/// Test: this test.
#[test]
fn assemble_report_keeps_turn_cap_exceeded_with_no_deliverable_as_run_failure() {
    let params = RunTaskParams {
        agent: "pm".into(),
        task: "write hello.py".into(),
        project: PathBuf::from("/tmp/does-not-matter"),
        agents_dir: PathBuf::from("/tmp/does-not-matter-agents"),
        engineer_model: None,
        deadline_secs: None,
    };
    let transcript: super::SharedTranscript = Arc::new(Mutex::new(Vec::new()));

    let before = super::diff::Snapshot::Files(std::collections::BTreeMap::new());
    let after = super::diff::Snapshot::Files(std::collections::BTreeMap::new());

    let pm_result: Result<AgentOutput, AgentLoopError> = Err(AgentLoopError::TurnCapExceeded {
        max_turns: 8,
        partial: Box::new(AgentOutput::from_content("partial pm output")),
    });

    let signal = RedelegationCapSignal::new();
    let completion = EngineerCompletionSignal::new();
    let report = super::assemble_report(
        &params,
        &transcript,
        "openai/gpt-4o-mini",
        "openai/gpt-4o-mini",
        super::RunOutcome {
            before,
            after,
            pm_result,
        },
        &signal,
        &completion,
    );

    assert_eq!(
        report.exit,
        ExitCode::RunFailure,
        "a TurnCapExceeded PM error with NO deliverable must stay RunFailure (fix #4)"
    );
    let _ = &report;
}

// ── #2683: a completed engineer must never be mislabeled `partial`/exit-6 ──────

/// (#2683) `assemble_report` maps a PM-loop `TurnCapExceeded` error to
/// `Success` — NOT `Partial` — when the delegated engineer already reported an
/// explicit successful `finish_task` completion AND a deliverable is on disk.
///
/// Why: This is the exact data-integrity bug the issue's 2026-07-15 recurrence
/// comment reports: the engineer finished with all tests passing, then the PM
/// fired one more gratuitous re-verify `delegate_to_agent` round that ran the
/// PM's loop out of turns, and the complete, correct run was mislabeled
/// `partial`/exit-6, corrupting run status/telemetry. A satisfied task must
/// report success.
/// What: Calls `assemble_report` directly with a `TurnCapExceeded` `pm_result`,
/// a before/after `Snapshot` pair that differ (one new file), and a
/// completion signal that has latched. Assert `exit == Success` and the diff is
/// preserved.
/// Test: this test.
#[test]
fn assemble_report_maps_completed_engineer_with_deliverable_to_success() {
    let params = RunTaskParams {
        agent: "pm".into(),
        task: "write hello.py".into(),
        project: PathBuf::from("/tmp/does-not-matter"),
        agents_dir: PathBuf::from("/tmp/does-not-matter-agents"),
        engineer_model: None,
        deadline_secs: None,
    };
    let transcript: super::SharedTranscript = Arc::new(Mutex::new(Vec::new()));

    let before = super::diff::Snapshot::Files(std::collections::BTreeMap::new());
    let mut after_map = std::collections::BTreeMap::new();
    after_map.insert("hello.py".to_string(), "print('hi')".to_string());
    let after = super::diff::Snapshot::Files(after_map);

    // The PM loop ran out of turns on the gratuitous re-verify round …
    let pm_result: Result<AgentOutput, AgentLoopError> = Err(AgentLoopError::TurnCapExceeded {
        max_turns: 8,
        partial: Box::new(AgentOutput::from_content("partial pm output")),
    });

    let signal = RedelegationCapSignal::new();
    // … but the engineer already reported a successful completion.
    let completion = EngineerCompletionSignal::new();
    completion.mark_completed();

    let report = super::assemble_report(
        &params,
        &transcript,
        "openai/gpt-4o-mini",
        "openai/gpt-4o-mini",
        super::RunOutcome {
            before,
            after,
            pm_result,
        },
        &signal,
        &completion,
    );

    assert_eq!(
        report.exit,
        ExitCode::Success,
        "a completed engineer with a real deliverable must report Success, never \
         Partial, even when the PM's loop later hit its turn cap (#2683); task: {}",
        report.task
    );
    assert!(
        report.diff.contains("hello.py"),
        "the completed deliverable's diff must still be rendered, got: {}",
        report.diff
    );
}

/// (#2683) A completed engineer with an EMPTY diff maps to `NoChanges`, not
/// `RunFailure` — a satisfied, no-op task is still a success outcome.
///
/// Why: Guards the empty-diff arm of the completion override so a genuinely
/// completed run that happened to change nothing is not conflated with a crash.
/// What: Same as above but before == after (no change); assert `NoChanges`.
/// Test: this test.
#[test]
fn assemble_report_maps_completed_engineer_without_deliverable_to_no_changes() {
    let params = RunTaskParams {
        agent: "pm".into(),
        task: "analyze the repo".into(),
        project: PathBuf::from("/tmp/does-not-matter"),
        agents_dir: PathBuf::from("/tmp/does-not-matter-agents"),
        engineer_model: None,
        deadline_secs: None,
    };
    let transcript: super::SharedTranscript = Arc::new(Mutex::new(Vec::new()));

    let before = super::diff::Snapshot::Files(std::collections::BTreeMap::new());
    let after = super::diff::Snapshot::Files(std::collections::BTreeMap::new());

    let pm_result: Result<AgentOutput, AgentLoopError> = Err(AgentLoopError::TurnCapExceeded {
        max_turns: 8,
        partial: Box::new(AgentOutput::from_content("partial pm output")),
    });

    let signal = RedelegationCapSignal::new();
    let completion = EngineerCompletionSignal::new();
    completion.mark_completed();

    let report = super::assemble_report(
        &params,
        &transcript,
        "openai/gpt-4o-mini",
        "openai/gpt-4o-mini",
        super::RunOutcome {
            before,
            after,
            pm_result,
        },
        &signal,
        &completion,
    );

    assert_eq!(
        report.exit,
        ExitCode::NoChanges,
        "a completed engineer that changed nothing must be NoChanges, not RunFailure (#2683)"
    );
}

/// (#2683) End-to-end: after the engineer completes via `finish_task`, a
/// gratuitous PM re-delegation is REFUSED (the engineer is not re-invoked) and
/// the run reports `Success` rather than `partial`.
///
/// Why: Proves both halves of the fix through the real `execute_run_task`
/// wiring: (b) the `DelegateToAgentTool` refuses re-delegation once the
/// completion signal latches, and the data-integrity half — a complete run is
/// labeled Success even when the PM keeps trying to re-delegate.
/// What: Script [PM delegate, engineer write_file, engineer finish_task
/// (completed), PM delegate AGAIN (must be refused — engineer NOT re-invoked),
/// PM finish_task]. Assert `exit == Success`, the diff names `hello.py`, the
/// engineer was invoked exactly once (exactly two `python-engineer` turns), and
/// no wasted engineer round was spent (exactly five total chat calls).
/// Test: this test.
#[tokio::test]
async fn gratuitous_redelegation_after_finish_is_refused_and_run_succeeds() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        delegate_response("create hello.py"),
        write_file_response("hello.py", "print('hi')"),
        finish_task_response("completed", "wrote hello.py, all good"),
        // The PM gratuitously re-delegates to "re-verify" — this must be
        // refused by the delegate tool, so the engineer is NEVER re-invoked
        // (the next scripted response is the PM's own finish_task, not an
        // engineer turn).
        delegate_response("re-verify hello.py once more"),
        finish_task_response("completed", "confirmed complete"),
    ]));

    let report = execute_run_task(params(&agents, &project, None), llm.clone()).await;

    assert_eq!(
        report.exit,
        ExitCode::Success,
        "a completed run must report Success even with a gratuitous re-delegation, \
         never partial; task: {}",
        report.task
    );
    assert!(
        report.diff.contains("hello.py"),
        "the deliverable diff must be preserved, got: {}",
        report.diff
    );

    let engineer_turns = report
        .transcript
        .iter()
        .filter(|t| t.role == "python-engineer")
        .count();
    assert_eq!(
        engineer_turns, 2,
        "the engineer must be invoked exactly once (write_file + finish_task); a \
         second, refused delegation must never reach it, got {engineer_turns} engineer turns"
    );
    assert_eq!(
        llm.models_seen().len(),
        5,
        "no wasted engineer round: the refused re-delegation must not spend a chat call"
    );
}

// ── #2265 fix #5: PM stops re-delegating once the cap latches ──────────────────

/// An `LlmClientTrait` that forces a retryable `LlmError` at specific GLOBAL
/// call indices (shared across the PM's and the engineer's chat calls) and
/// otherwise forwards to an inner `ScriptedLlm`, whose own fixture cursor
/// only advances on forwarded (non-forced-error) calls.
///
/// Why: `repeated_llm_errors_trigger_redelegation_cap_not_pm_turn_cap` proves
/// the cap latches within one `delegate_to_agent` dispatch, but its mock
/// exhausts entirely at that point — a bare-exhaustion error also aborts the
/// PM's OWN next `chat` call, so that test cannot distinguish "the PM's loop
/// stopped itself before calling chat again" (this fix) from "the PM's chat
/// call happened and failed anyway" (the pre-fix behaviour would produce the
/// same shape of error there). This wrapper keeps VALID `delegate_to_agent`
/// fixtures queued up behind the forced-error window, standing in for a real
/// model that keeps deciding to re-delegate because it does not understand
/// the cap-reached tool error — the exact bake-off L1 failure mode. If the
/// PM's loop does NOT stop itself, those fixtures get consumed and recorded;
/// if it does, the global call counter never reaches them.
/// What: `error_at` global call indices return `LlmError::ApiError` (a
/// retryable `AgentLoopError::Llm` per `redelegation_hint`) without touching
/// `inner`; every other index forwards to `inner.chat`, so `inner`'s fixture
/// list is consumed strictly in the order its calls are actually forwarded.
/// Test: `pm_stops_redelegating_once_cap_latched_ends_partial_promptly`.
struct FlakyThenRepeatLlm {
    inner: ScriptedLlm,
    counter: AtomicUsize,
    error_at: Vec<usize>,
}

#[async_trait]
impl LlmClientTrait for FlakyThenRepeatLlm {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let idx = self.counter.fetch_add(1, Ordering::SeqCst);
        if self.error_at.contains(&idx) {
            return Err(LlmError::ApiError {
                status: 500,
                body: "synthetic retryable transport failure".to_string(),
            });
        }
        self.inner.chat(req).await
    }
}

/// Once the shared re-delegation cap latches, the PM must stop issuing
/// `delegate_to_agent` calls immediately (bounded total chat calls, well
/// under the PM's `max_turns` of 8) and the run must still end `Partial`
/// with the deliverable the engineer already wrote preserved in the diff.
///
/// Why: This is the exact #2265 follow-up regression closed by fix #5: before
/// this fix, `AgentLoop::run_inner` had no way to learn the cap had latched,
/// so the PM's own LLM — seeing only an opaque tool error, with nothing
/// telling it re-delegating again is pointless — could keep calling
/// `delegate_to_agent` every remaining turn, each one an instant, guaranteed
/// dead end (bake-off L1 evidence: "re-delegation limit reached after 10
/// attempts … turn cap of 8 exceeded", 3 productive attempts + 7 wasted PM
/// turns). The exit code must NOT change — `assemble_report` already maps a
/// cap-latched, deliverable-bearing run to `Partial` regardless of which
/// `AgentLoopError` variant fired; this test proves the fix trims the wasted
/// turns without touching that mapping.
/// What: Script [PM delegate, engineer write_file (the deliverable), then 3
/// forced retryable `Llm` errors — engineer's own second turn plus its two
/// internal reuse-hint retries — landing the shared cap at
/// `MAX_REDELEGATIONS` attempts entirely inside that ONE PM dispatch]. Queue
/// SIX MORE valid `delegate_to_agent` fixtures behind that window (as if a
/// real model kept trying) — enough to fill the PM's remaining `max_turns`
/// budget if the fix did not stop it. Assert: `exit == Partial`, the diff
/// still names `hello.py`, the PM's own turn count is exactly 1 (it never
/// reached a second turn), and the total number of chat calls made across
/// the whole run stayed small (well below the ~12 calls an unfixed run would
/// need to hit its own `TurnCapExceeded`) — direct proof the trailing
/// `delegate_response` fixtures were never consumed.
/// Test: this test.
#[tokio::test]
async fn pm_stops_redelegating_once_cap_latched_ends_partial_promptly() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");

    let inner = ScriptedLlm::from_json(&[
        delegate_response("do work"),
        write_file_response("hello.py", "print(1)"),
        // Trailing fixtures a real, cap-unaware model might trigger by
        // calling delegate_to_agent again on turns 2..8 — must NEVER be
        // consumed once the fix is in place.
        delegate_response("task-2"),
        delegate_response("task-3"),
        delegate_response("task-4"),
        delegate_response("task-5"),
        delegate_response("task-6"),
        delegate_response("task-7"),
    ]);
    let llm = Arc::new(FlakyThenRepeatLlm {
        inner,
        counter: AtomicUsize::new(0),
        // Global calls: 0=PM delegate, 1=engineer write_file, then 2,3,4 are
        // the engineer's own failing turn plus its two internal reuse-hint
        // retries — landing attempt 4 (> MAX_REDELEGATIONS=3) and latching
        // the cap without a 5th call.
        error_at: vec![2, 3, 4],
    });

    let report = execute_run_task(params(&agents, &project, None), llm.clone()).await;

    assert_eq!(
        report.exit,
        ExitCode::Partial,
        "the cap latched but the engineer's write_file already produced a \
         deliverable, so this must be Partial, not RunFailure; got task: {}",
        report.task
    );
    assert!(
        report.diff.contains("hello.py"),
        "the deliverable diff must be preserved, got: {}",
        report.diff
    );

    let pm_turns = report.transcript.iter().filter(|t| t.role == "pm").count();
    assert_eq!(
        pm_turns, 1,
        "the PM must stop after its single delegating turn — it must never \
         reach a second turn once the cap has latched"
    );

    let total_calls = llm.counter.load(Ordering::SeqCst);
    assert!(
        total_calls <= 5,
        "the PM must not have issued any further delegate_to_agent calls \
         after the cap latched (bounded, not spinning to the ~12 calls an \
         unfixed 8-turn PM loop would need), got {total_calls} total chat calls"
    );
}

// ── #2279: PM-side verify-before-finish gate (symmetric with the engineer's) ────

/// (#2279) The PM's verify-before-finish gate is SATISFIED — its
/// `finish_task` call succeeds on the FIRST attempt — when the delegated
/// engineer's OWN transcript shows a matching `bash` test invocation, even
/// though the PM never calls `bash` itself.
///
/// Why: This is #2279's PM-symmetric acceptance criterion proven end to end
/// (not just at the `verify_gate::pm_finish_gate` unit level): the real
/// wiring in `execute_run_task` — the shared `SharedTranscript` both the PM's
/// and the engineer's `RecordingLlmClient` record into — must actually carry
/// the engineer's `ran_test_command` signal through to the PM's gate.
/// What: The task names `pytest tests/ -v`. Script [PM delegates, engineer
/// runs `pytest tests/ -v` via `bash`, engineer stops (D3), PM finish_task];
/// assert the scripted client saw EXACTLY four calls — proving the PM's
/// single `finish_task` attempt was accepted, not rejected into a fifth
/// retry turn — and the run did not surface as a failure.
/// Test: this test.
#[tokio::test]
async fn pm_finish_gate_satisfied_when_engineer_ran_named_tests() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        delegate_response("implement the parser; then run `pytest tests/ -v`"),
        bash_response("pytest tests/ -v"),
        stop_response("engineer: ran the suite"),
        finish_task_response("completed", "verified via the named test suite"),
    ]));

    let mut task_params = params(&agents, &project, None);
    task_params.task = "implement the parser; run `pytest tests/ -v` before finishing".into();

    let report = execute_run_task(task_params, llm.clone()).await;

    assert_eq!(
        llm.models_seen().len(),
        4,
        "the PM's single finish_task attempt must be accepted, not rejected \
         into a fifth retry turn; task: {}",
        report.task
    );
    assert_ne!(
        report.exit,
        ExitCode::RunFailure,
        "an accepted finish must not surface as a run failure; task: {}",
        report.task
    );
}

/// (#2279) The PM's verify-before-finish gate TRIPS — its `finish_task` call
/// is rejected as a RECOVERABLE retry, not a run failure — when the task
/// names a runnable test command but the delegated engineer's transcript
/// shows no matching invocation, and the PM's run still recovers/completes
/// via its OWN next turn.
///
/// Why: The negative half of the PM-symmetric acceptance criterion, proving
/// (a) the gate actually rejects a premature finish sourced from the
/// engineer's silence, and (b) that rejection is recoverable — the PM gets
/// exactly one more turn, not a hung loop or a hard failure.
/// What: The task names `pytest tests/ -v`. Script [PM delegates, engineer
/// writes a file WITHOUT running `bash` at all, engineer stops (D3), PM's
/// FIRST finish_task attempt (must be rejected), PM's SECOND turn (a plain
/// D3 stop — unaffected by the gate, proving recovery)]; assert the scripted
/// client saw exactly FIVE calls — one more than the satisfied-gate test's
/// four, precisely the one recoverable retry turn the gate forced — and the
/// run did not surface as a failure.
/// Test: this test.
#[tokio::test]
async fn pm_finish_gate_trips_when_engineer_never_ran_named_tests() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        delegate_response("implement the parser; then run `pytest tests/ -v`"),
        write_file_response("parser.py", "def parse():\n    pass\n"),
        stop_response("engineer: wrote the parser (did not run tests)"),
        finish_task_response("completed", "premature — tests never ran"),
        stop_response("pm: recovered after the rejected finish_task"),
    ]));

    let mut task_params = params(&agents, &project, None);
    task_params.task = "implement the parser; run `pytest tests/ -v` before finishing".into();

    let report = execute_run_task(task_params, llm.clone()).await;

    assert_eq!(
        llm.models_seen().len(),
        5,
        "the PM's premature finish_task must be rejected into exactly one \
         recoverable retry turn, not accepted outright and not looped \
         indefinitely; task: {}",
        report.task
    );
    assert_ne!(
        report.exit,
        ExitCode::RunFailure,
        "a gate rejection must be a recoverable retry, not a run failure; \
         task: {}",
        report.task
    );
}

/// #2348: `run-task`'s one-shot/bake-off path (`execute_run_task`) must
/// NEVER register `recall_session` — it has no session_id to scope a memory
/// query to, and Parity mode requires byte-identical prompts to the
/// pre-#2343 baseline (epic #2343 scope note: "run_task one-shot/bake-off
/// path explicitly untouched"). Registration lives only in
/// `task::executor::run_and_record`'s daemon-session `pm_registry`.
///
/// Why: asserts the ACTUAL wire-level tool schemas the PM's `AgentLoop`
/// sends (via `ScriptedLlm::first_tool_names`), not a re-derivation of the
/// registration logic, so this is a real regression guard against
/// `recall_session` ever being wired into this module by mistake.
/// What: Script [PM stop] only — no delegation needed to observe the PM's
/// own advertised tool set on its first (only) turn.
/// Test: this test.
#[tokio::test]
async fn run_task_registry_never_registers_recall_session() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");

    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response(
        "pm: nothing to do",
    )]));
    let _report = execute_run_task(params(&agents, &project, None), llm.clone()).await;

    let names = llm.first_tool_names();
    assert!(
        names.contains(&"finish_task".to_string()),
        "sanity: finish_task must still be registered; got {names:?}"
    );
    assert!(
        !names.contains(&"recall_session".to_string()),
        "run_task's one-shot registry must never register recall_session; got {names:?}"
    );
}

/// `ensure_project_indexed_in_background` spawns its indexing thread even when
/// the project path is NOT inside a git repository.
///
/// Why: regression guard for the removed `find_git_root` short-circuit (owner
/// directive: git is a nice-to-have for tcode, not a requirement — trusty-search
/// should index scratch directories same as any other project). Before this
/// fix, a project with no `.git` anywhere up its tree caused the function to
/// return before ever spawning the thread.
/// What: calls the function with a plain tempdir that has no `.git`, and
/// asserts it returns promptly (proving it did not skip out early nor block on
/// the network) without panicking. The spawned thread is detached and
/// fail-open, so no running trusty-search daemon is required for this test.
/// Test: this test.
#[test]
fn spawns_indexing_thread_for_non_git_project_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // No `.git` anywhere under `tmp` — exactly the scratch/bake-off case the
    // old guard used to skip.
    let start = std::time::Instant::now();
    super::ensure_project_indexed_in_background(tmp.path().to_path_buf());
    assert!(
        start.elapsed() < std::time::Duration::from_millis(500),
        "must return immediately (spawn-and-detach), never block on the network"
    );
}
