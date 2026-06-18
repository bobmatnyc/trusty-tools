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

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{ExitCode, RunTaskParams, execute_run_task};
use crate::llm::{ChatRequest, ChatResponse, LlmClientTrait, LlmError};

// ── Scripted offline LLM ───────────────────────────────────────────────────────

/// Replays a fixed queue of responses in call order.
///
/// Why: Deterministic, offline substitute for the network client shared by the
/// PM and engineer loops. The transcript recorder captures each turn's model from
/// the response side, so the tests assert routing via `report.transcript`.
/// What: Holds JSON-deserialised responses and an atomic cursor. Exhaustion yields
/// a transport-style error so a runaway loop fails loudly rather than hanging.
/// Test: Used by every test below.
struct ScriptedLlm {
    responses: Vec<ChatResponse>,
    cursor: AtomicUsize,
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
        }
    }
}

#[async_trait]
impl LlmClientTrait for ScriptedLlm {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let _ = req;
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
/// The `_engineer_model` slot is the #1035 hook; it is unused in the #1034 tests
/// (the engineer routes via its own config model) and wired up in #1035.
fn params(agents: &TempDir, project: &TempDir, _engineer_model: Option<&str>) -> RunTaskParams {
    RunTaskParams {
        agent: "pm".into(),
        task: "write hello.py".into(),
        project: project.path().to_path_buf(),
        agents_dir: agents.path().to_path_buf(),
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
