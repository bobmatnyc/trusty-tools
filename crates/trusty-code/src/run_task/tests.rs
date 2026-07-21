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
//! (PM-turn-1, engineer-turn-1, …). Fixtures build agents-dir `.md` agents and a
//! project dir; tests assert on the returned `RunReport`.
//! Test: this module is itself the test surface.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{
    ExitCode, RedelegationCapSignal, RunTaskParams, execute_run_task as real_execute_run_task,
    resolve_agent_model_slug,
};
use crate::agent_loop::AgentLoopError;
use crate::agents::AgentConfig;
use crate::llm::{ChatRequest, ChatResponse, LlmClientTrait, LlmError};
use crate::runner::RegistryFactory;
use crate::tools::{AgentOutput, EngineerCompletionSignal, RunContext};

// ── Hermeticity (#3361): never let a real ambient daemon reach into these
// tests' sandboxed temp dirs ────────────────────────────────────────────────

/// One-time, process-lifetime isolation of the two ambient daemon endpoints
/// `execute_run_task` reaches for: trusty-search (background indexing, via
/// `ensure_project_indexed_in_background`) and trusty-memory (the PM catch-up
/// digest, via `catchup::pm_catchup_context`).
///
/// Why (#3361): on a machine with a live `trusty-memory` and/or
/// `trusty-search` daemon, `execute_run_task` discovers and contacts them for
/// real — `trusty-memory` because `catchup::pm_catchup_context` resolves its
/// base URL via `resolve_memory_base_url_or_unreachable()`
/// (`TRUSTY_MEMORY_URL` env override, else the daemon's on-disk discovery
/// file), and `trusty-search` because
/// `ensure_project_indexed_in_background` -> `ensure_project_indexed` finds
/// the daemon via `resolve_daemon_base_url("trusty-search")` (same on-disk
/// discovery, gated by `TRUSTY_DATA_DIR_OVERRIDE`) and, once found, actually
/// registers + reindexes the test's tempdir `project` against the LIVE
/// daemon — which then writes its own colocated storage (e.g.
/// `.trusty-search/schema_version.json`) INSIDE the sandboxed tempdir. That
/// extra on-disk file shows up in `diff::capture_snapshot`'s before/after
/// diff, so any test asserting an empty diff (`no_changes_yields_no_changes_exit`,
/// `missing_disk_pm_config_falls_back_to_embedded_pm`) or a specific
/// `RunFailure`-vs-`Partial` exit keyed on "no deliverable exists"
/// (`exit_code_reflects_run_failure`) spuriously fails — not because of a
/// product regression, but because the sandbox was never actually hermetic.
///
/// This is the SAME class of bug `catchup::pm_catchup_context_with_memory_url`
/// (#3003) and `session::memory_sink::TurnMemorySink` already fixed for their
/// OWN unit tests by threading the daemon URL through as a parameter instead
/// of touching the process-global env var per test. `execute_run_task` has no
/// such injection seam (it is the black-box entry point under test here), so
/// the fix at THIS layer uses the two seams the production code itself
/// already treats as authoritative overrides — `TRUSTY_MEMORY_URL` (an
/// explicit override always wins over discovery,
/// `resolve_memory_base_url_or_unreachable`'s own doc) and
/// `TRUSTY_DATA_DIR_OVERRIDE` (documented in `trusty_common::data_dir` as
/// "intended for tests only... the only reliable cross-platform way to
/// isolate test data paths") — rather than reaching for `#[serial]` or hoping
/// no daemon happens to be running.
///
/// Both are set EXACTLY ONCE, guarded by `std::sync::Once`, and are NEVER
/// restored. This deliberately differs from a per-test set/restore guard
/// (the pattern #3434 shows is unsafe for a process-global var under
/// `cargo test`'s parallel threads): because every test in this module wants
/// the identical "no ambient daemon is reachable" outcome, and the value is
/// fixed for the remainder of the test binary's lifetime once installed,
/// there is no window in which one thread can observe a different, another
/// thread's transient value — the mutate-once-and-never-restore shape has no
/// race to hide behind slower runs, unlike a mutate/restore pattern would.
/// Test: exercised implicitly by every test that calls
/// [`execute_run_task`] (this module's wrapper) below; the before/after
/// behaviour is demonstrated by `exit_code_reflects_run_failure`,
/// `no_changes_yields_no_changes_exit`, and
/// `missing_disk_pm_config_falls_back_to_embedded_pm`, which fail under a
/// live ambient trusty-memory/trusty-search daemon without this guard and
/// pass with it.
fn isolate_ambient_daemons() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        // A reserved, unassigned TCP port: connection attempts fail fast
        // rather than hanging or timing out. Mirrors
        // `catchup::tests::UNREACHABLE_MEMORY_URL` and
        // `trusty_common::mcp::memory_rpc`'s own `UNREACHABLE_PLACEHOLDER`
        // convention. An explicit `TRUSTY_MEMORY_URL` always wins over
        // discovery (see `resolve_memory_base_url`), so this alone is
        // sufficient to stop `catchup::pm_catchup_context` from ever
        // resolving the host's real trusty-memory address.
        // SAFETY: `Once` guarantees this runs exactly one time, before any
        // reader in this process observes it (no test spawns a thread that
        // reads it before this call happens on the same thread that reaches
        // this line first) — see the doc comment above for why no restore
        // (and thus no unsafe-across-threads mutation) is needed.
        unsafe {
            std::env::set_var(
                trusty_common::mcp::memory_rpc::TRUSTY_MEMORY_URL_ENV,
                "http://127.0.0.1:1",
            );
        }

        // An empty, dedicated, never-reused directory: `resolve_data_dir`
        // will create `<this>/trusty-search/` (and `<this>/trusty-memory/`
        // for any code path that falls through to discovery instead of the
        // env override above) with no `http_addr` file inside, so
        // `resolve_daemon_base_url` always resolves to `None` — the daemon
        // is treated as "never started" rather than guessing/reaching a
        // real port.
        let isolated_data_dir = std::env::temp_dir().join(format!(
            "trusty-code-run-task-test-daemon-isolation-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&isolated_data_dir);
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, &isolated_data_dir);
        }
    });
}

/// Test-only wrapper around [`real_execute_run_task`] that installs the
/// hermetic-daemon isolation (#3361) before every call.
///
/// Why: centralising the call here (rather than repeating
/// `isolate_ambient_daemons()` at the top of every test) means a new test
/// added to this file gets the guard automatically just by calling
/// `execute_run_task` like every existing test already does — there is
/// nothing extra to remember.
/// What: calls [`isolate_ambient_daemons`] then forwards to
/// [`real_execute_run_task`] unchanged.
/// Test: see [`isolate_ambient_daemons`].
async fn execute_run_task(params: RunTaskParams, llm: Arc<dyn LlmClientTrait>) -> super::RunReport {
    isolate_ambient_daemons();
    real_execute_run_task(params, llm).await
}

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

    /// The tool-schema names advertised on the request at `idx` (0-based, call
    /// order) — lets a test reach a non-first turn's advertised schema, e.g.
    /// the delegated engineer's first turn (request idx 1, right after the
    /// PM's own delegate call at idx 0).
    fn tool_names_at(&self, idx: usize) -> Vec<String> {
        self.tool_names
            .lock()
            .expect("tool_names lock")
            .get(idx)
            .cloned()
            .unwrap_or_else(|| panic!("no request recorded at idx {idx}"))
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

/// Build an agents dir with `pm.md` and `python-engineer.md`.
///
/// Why: `run-task` loads the PM config from `<agents_dir>/pm.md` and the
/// engineer from `<agents_dir>/python-engineer.md` (#2897 Slice D); tests
/// need both on disk.
/// What: Writes both `.md` agents (engineer pinned to `engineer_model`) and
/// returns the tempdir.
fn agents_dir(engineer_model: &str) -> TempDir {
    let tmp = tempfile::tempdir().expect("agents tempdir");
    std::fs::write(
        tmp.path().join("pm.md"),
        "---\nname: pm\nmodel: openai/gpt-4o-mini\n---\n\nYou are the PM.\n",
    )
    .expect("write pm.md");
    std::fs::write(
        tmp.path().join("python-engineer.md"),
        format!(
            "---\nname: python-engineer\nmodel: {engineer_model}\n---\n\nYou are a Python engineer.\n"
        ),
    )
    .expect("write python-engineer.md");
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
/// the hard-coded default regardless of what `pm.md` declared. This is the
/// end-to-end regression guard: a `pm.md` with `max_tokens: 8192` must
/// produce a `ChatRequest.max_tokens` of `8192`, never the old 1024.
/// What: `pm.md` declares `max_tokens: 8192`; script a single PM stop turn;
/// assert the scripted client observed `Some(8192)`.
/// Test: this test.
#[tokio::test]
async fn pm_llm_max_tokens_reaches_chat_request() {
    let agents = tempfile::tempdir().expect("agents tempdir");
    std::fs::write(
        agents.path().join("pm.md"),
        "---\nname: pm\nmodel: openai/gpt-4o-mini\nmax_tokens: 8192\n---\n\n\
         You are the PM.\n",
    )
    .expect("write pm.md");
    std::fs::write(
        agents.path().join("python-engineer.md"),
        "---\nname: python-engineer\nmodel: deepseek/deepseek-chat\n---\n\nYou are a Python engineer.\n",
    )
    .expect("write python-engineer.md");
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

/// An agent name with NEITHER a disk config NOR an embedded default is a
/// configuration error (no panic, distinct exit code).
///
/// Why: Bad configuration must produce a faithful `ConfigError`, not a crash.
/// This is the regression pin `missing_pm_config_is_config_error` used to
/// cover with `agent: "pm"` before #3437 added an embedded `pm` default —
/// `"pm"` now ALWAYS resolves (see
/// `missing_disk_pm_config_falls_back_to_embedded_pm` below), so the
/// still-valid "unresolvable agent is a config error" case is exercised here
/// with an agent name that is genuinely absent from both disk and
/// [`crate::assets::DEFAULT_AGENTS`].
/// What: Point the params at an empty agents dir with agent name
/// `"totally-not-a-real-agent"`; assert exit=ConfigError.
/// Test: this test.
#[tokio::test]
async fn missing_agent_config_is_config_error() {
    let empty_agents = tempfile::tempdir().expect("agents tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response("unused")]));

    let report = execute_run_task(
        RunTaskParams {
            agent: "totally-not-a-real-agent".into(),
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
        "an agent name absent from both disk and the embedded roster must be a config error"
    );
}

/// `agent: "pm"` with NO disk `pm.md` now resolves via the embedded default
/// (#3437) instead of failing agent resolution.
///
/// Why: this is the acceptance test for #3437 at the `run_task` level — the
/// exact call shape (`agent: "pm"`, empty `agents_dir`) that used to hit
/// `ConfigError` before the embedded `pm` agent existed must now actually
/// run the PM loop and reach a normal exit code.
/// What: Same empty-agents-dir shape as `missing_agent_config_is_config_error`,
/// but `agent: "pm"` with a scripted `[PM stop]` response; asserts the run
/// reaches `ExitCode::NoChanges` (the PM concluded without delegating), not
/// `ConfigError`.
/// Test: this test.
#[tokio::test]
async fn missing_disk_pm_config_falls_back_to_embedded_pm() {
    let empty_agents = tempfile::tempdir().expect("agents tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let llm = Arc::new(ScriptedLlm::from_json(&[stop_response(
        "pm: nothing to do",
    )]));

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
        ExitCode::NoChanges,
        "'pm' with no disk config must resolve via the embedded default (#3437) and run, \
         not fail with ConfigError; report: {report:?}"
    );
}

/// `resolve_agent_model_slug` degrades to `"unknown"` when the agent's `.md`
/// config is missing, rather than panicking or propagating an error.
///
/// Why: pricing (`crate::perf::cost_usd`) must degrade gracefully for an
/// unrecognised model, not abort the whole run over a missing/renamed agent
/// config — see this function's own doc comment.
/// What: an empty agents dir, no `ghost.md`; asserts the literal string
/// `"unknown"`.
/// Test: this test.
#[test]
fn resolve_agent_model_slug_falls_back_when_config_missing() {
    let empty_agents = tempfile::tempdir().expect("agents tempdir");
    let slug = resolve_agent_model_slug(empty_agents.path(), "ghost", None);
    assert_eq!(slug, "unknown");
}

/// `resolve_agent_model_slug`'s `model_override` wins over the agent
/// config's own `model:`.
///
/// Why: mirrors `RunContext`'s override precedence — a per-run
/// `--engineer-model` swap must be reflected in the PRICED model, not just
/// the one that actually drove the loop.
/// What: `python-engineer.md` pins `openai/gpt-4o-mini`; pass
/// `Some("deepseek/deepseek-chat")` as the override; asserts the override
/// slug wins.
/// Test: this test.
#[test]
fn resolve_agent_model_slug_honours_override() {
    let agents = tempfile::tempdir().expect("agents tempdir");
    std::fs::write(
        agents.path().join("python-engineer.md"),
        "---\nname: python-engineer\nmodel: openai/gpt-4o-mini\n---\n\nengineer\n",
    )
    .expect("write python-engineer.md");

    let slug = resolve_agent_model_slug(
        agents.path(),
        "python-engineer",
        Some("deepseek/deepseek-chat"),
    );
    assert_eq!(slug, "deepseek/deepseek-chat");
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

/// `assemble_report` maps a retry-exhausted run WITH a deliverable to
/// `Partial`, labelled "re-delegation limit reached" (#2852).
///
/// Why: Post-#2852 this is arguably the most common real-world shape: retry
/// exhaustion is now RECOVERABLE, so the PM keeps its turns and the loop ends
/// on some other error while `retry_budget_exhausted` stays latched. Reporting
/// keys off that flag rather than only `cap_reached` precisely so such runs
/// keep their honest "the engineer was retried to exhaustion" diagnosis instead
/// of silently degrading to an opaque `run_failure` — the exact regression
/// #2852 set out to prevent. Nothing pinned this label's text, so it could have
/// rotted freely; this test is that pin.
/// What: Calls `assemble_report` directly with a signal that has ONLY
/// `retry_budget_exhausted` latched (never `cap_reached`), a non-`TurnCap` PM
/// error, and a before/after pair that differ. Assert `Partial`, and that the
/// label names the re-delegation limit and the attempt count — NOT the
/// invocation ceiling, which did not fire.
/// Test: this test.
#[test]
fn assemble_report_maps_retry_exhausted_with_deliverable_to_partial() {
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

    // Deliberately NOT a turn cap: the point is that the retry-exhausted flag
    // alone carries the label, independent of which loop error surfaced.
    let pm_result: Result<AgentOutput, AgentLoopError> =
        Err(AgentLoopError::Llm(LlmError::ApiError {
            status: 500,
            body: "upstream exploded".to_string(),
        }));

    let signal = RedelegationCapSignal::retry_exhausted_for_test();
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
        "a retry-exhausted run that still produced a deliverable must map to Partial, \
         not RunFailure (#2852), got label: {}",
        report.task
    );
    assert!(
        report.task.contains("re-delegation limit reached"),
        "the label must name the re-delegation limit so the diagnosis is not lost, \
         got: {}",
        report.task
    );
    assert!(
        report
            .task
            .contains(&super::redelegation::MAX_REDELEGATIONS.to_string()),
        "the label must quote the per-delegation attempt count, got: {}",
        report.task
    );
    assert!(
        !report.task.contains("failure ceiling"),
        "the run-wide ceiling never fired — the label must not claim it did, got: {}",
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

// ── #2265 fix #5 / #2852: PM stops re-delegating once the run-wide ceiling ─────
// ── latches (a single delegation's retry exhaustion must NOT stop it) ──────────

/// Answers every PM turn with a `delegate_to_agent` call; lets the engineer's
/// very first call write the deliverable, then fails every engineer call after
/// it with a retryable transport error.
///
/// Why: The #2852 ceiling test needs a PM that keeps delegating while every
/// engineer invocation ultimately fails — a scenario `FlakyThenRepeatLlm`'s
/// global call-index scripting cannot express, because the PM's and engineer's
/// calls interleave unpredictably once retries are in play. Keying off the
/// request's advertised tool schema (only the PM is offered
/// `delegate_to_agent`) makes the roles unambiguous and the test fully
/// deterministic regardless of how many retries occur.
/// What: Inspects `req.tools` to classify the caller. PM requests get a canned
/// delegate call. The engineer's first request replays `first` (a `write_file`
/// that puts a real deliverable on disk); every later engineer request returns
/// `LlmError::ApiError`, which `redelegation_hint` classifies as retryable.
/// Test: `run_wide_ceiling_stops_the_pm_loop_and_ends_partial_promptly`.
struct FirstEngineerCallWritesThenAllFail {
    first: Arc<ScriptedLlm>,
    delegate: ChatResponse,
    pm_calls: AtomicUsize,
    engineer_calls: AtomicUsize,
}

#[async_trait]
impl LlmClientTrait for FirstEngineerCallWritesThenAllFail {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let is_pm = req
            .tools
            .as_ref()
            .is_some_and(|tools| tools.iter().any(|t| t.function.name == "delegate_to_agent"));
        if is_pm {
            self.pm_calls.fetch_add(1, Ordering::SeqCst);
            return Ok(self.delegate.clone());
        }
        if self.engineer_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return self.first.chat(req).await;
        }
        Err(LlmError::ApiError {
            status: 500,
            body: "synthetic retryable transport failure".to_string(),
        })
    }
}

/// Once the run-wide engineer-failure ceiling latches, the PM must stop
/// issuing `delegate_to_agent` calls (bounded well under its `max_turns` of 8)
/// and the run must still end `Partial` with the deliverable preserved.
///
/// Why: This is #2265 fix #5's regression, re-based onto #2852's split. Fix #5
/// stops the PM burning its remaining `max_turns` on doomed calls once the cap
/// latches (bake-off L1 evidence: "re-delegation limit reached after 10
/// attempts … turn cap of 8 exceeded", 3 productive attempts + 7 wasted PM
/// turns). #2852 narrowed WHICH condition may fire that unrecoverable hook to
/// the one a fresh delegation cannot clear — the run-wide
/// `MAX_FAILED_INVOCATIONS` ceiling — so this test now drives the ceiling
/// rather than a single delegation's retry exhaustion.
///
/// This test previously asserted `pm_turns == 1`: that a PM whose FIRST
/// delegation exhausted its retries never got a second turn. That assertion is
/// dropped as a deliberate POLICY change, and the record is worth stating
/// precisely: the old scenario (one delegation whose engineer failed its own
/// retries three times) was the cap's original, still-valid purpose — a
/// genuinely struggling engineer — NOT run-6's bug, which was several separate
/// SUCCESSFUL delegations starving a later one. So the old assertion was not
/// itself defective, and an earlier version of this comment overstated the case
/// by claiming it "encoded the bug". What actually changed is the policy: under
/// #2852 that scenario now latches `retry_budget_exhausted` (recoverable), so
/// the PM does get a second turn — which is the intended behaviour, because a
/// fresh delegation with a full budget may well succeed. The scenario is
/// therefore rebuilt around the ceiling, and the bound is now "well under
/// max_turns" rather than "exactly 1".
/// What: The engineer writes the deliverable on its first turn, then every
/// engineer call fails retryably forever while the PM keeps delegating. That
/// first success costs no ceiling budget (#2852 — only failures count);
/// each subsequent delegation burns `MAX_REDELEGATIONS` failures, so the
/// ceiling latches on delegation 5. Assert: `exit == Partial`, the diff still names `hello.py`,
/// the report names the ceiling, and the PM stopped strictly before its own
/// 8-turn cap — proof the stop signal, not the turn cap, ended the run.
/// Test: this test.
#[tokio::test]
async fn run_wide_ceiling_stops_the_pm_loop_and_ends_partial_promptly() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");

    // The engineer's FIRST invocation writes the deliverable, then fails; every
    // later invocation fails outright. Delegating the write via a one-shot
    // scripted response keeps a real diff on disk to assert Partial against.
    let first = Arc::new(ScriptedLlm::from_json(&[write_file_response(
        "hello.py", "print(1)",
    )]));
    let llm = Arc::new(FirstEngineerCallWritesThenAllFail {
        first,
        delegate: serde_json::from_value(delegate_response("do work"))
            .expect("valid delegate fixture"),
        pm_calls: AtomicUsize::new(0),
        engineer_calls: AtomicUsize::new(0),
    });

    let report = execute_run_task(params(&agents, &project, None), llm.clone()).await;

    assert_eq!(
        report.exit,
        ExitCode::Partial,
        "the ceiling latched but the engineer's write_file already produced a \
         deliverable, so this must be Partial, not RunFailure; got task: {}",
        report.task
    );
    assert!(
        report.diff.contains("hello.py"),
        "the deliverable diff must be preserved, got: {}",
        report.diff
    );
    assert!(
        report.task.contains("engineer failure ceiling reached"),
        "the report must name the run-wide ceiling (#2852) as the terminal \
         condition, got: {}",
        report.task
    );

    let pm_turns = report.transcript.iter().filter(|t| t.role == "pm").count();
    assert!(
        pm_turns < 8,
        "the stop signal — not the PM's own 8-turn cap — must have ended the \
         run, got {pm_turns} PM turns"
    );
    // Precise invocation bounding is asserted at the unit level in
    // `redelegation::tests::failure_ceiling_latches_cap_reached`;
    // here it is enough that the PM stopped delegating rather than spinning
    // out its whole turn budget on calls the ceiling would only refuse.
    assert!(
        llm.pm_calls.load(Ordering::SeqCst) < 8,
        "the PM must stop delegating once the ceiling latches, got {} PM calls",
        llm.pm_calls.load(Ordering::SeqCst)
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
    super::ensure_project_indexed_in_background(tmp.path().to_path_buf(), None);
    assert!(
        start.elapsed() < std::time::Duration::from_millis(500),
        "must return immediately (spawn-and-detach), never block on the network"
    );
}

/// The readiness observer must be invoked exactly once, on the detached
/// thread, even when nothing is probeable.
///
/// Why: the daemon path publishes `Event::IndexReadiness` from this observer.
/// If it were skipped whenever the probe returned `None`, a session with no
/// reachable trusty-search daemon would emit NO readiness event at all — and
/// the UI would silently fall back to rendering "no results found" for an
/// index it cannot even see, which is the precise confusion #2784 exists to
/// remove. `None` is a reportable state, not a reason to stay silent.
/// What: calls the function against a `.git`-less tempdir with an observer
/// that latches into a channel; asserts the observer ran. It deliberately does
/// NOT assert WHICH state was observed — that depends on whether a
/// trusty-search daemon happens to be up on the machine running the tests, and
/// the contract under test is "the observer is always invoked, whatever the
/// probe found", not the probe's result (which
/// `trusty_common::search_readiness`'s own tests already pin).
/// Test: this test.
#[test]
fn background_indexing_invokes_readiness_observer() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (tx, rx) = std::sync::mpsc::channel();

    super::ensure_project_indexed_in_background(
        tmp.path().to_path_buf(),
        Some(Box::new(move |readiness| {
            let _ = tx.send(readiness.is_some());
        })),
    );

    // The detached thread does one short-timeout probe; 10s is far beyond its
    // ~1.5s cap while still failing fast on a genuine "never called" bug.
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .expect("the readiness observer must be invoked, even when nothing is probeable");
}

/// A response where the assistant calls `use_skill(name)` (#2924).
fn use_skill_response(name: &str) -> Value {
    json!({
        "id": "gen-use-skill",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-use-skill",
                    "type": "function",
                    "function": {
                        "name": "use_skill",
                        "arguments": json!({"name": name}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 25, "completion_tokens": 6, "total_tokens": 31}
    })
}

/// The `--legacy-in-process`/bake-off `run_task` path (`execute_run_task`)
/// wires the skill catalog and `use_skill` tool into the PM's registry the
/// same way the daemon path (`task::executor::run_and_record`) already does
/// (#2924) — before this fix, `daily_driver_skills_catalog` and
/// `UseSkillTool` registration existed ONLY on the daemon path, so a PM
/// running through this CLI path could never discover or invoke a project
/// skill at all.
///
/// Why: mirrors `runner::tests::skills_catalog_reaches_daily_driver_prompt`,
/// but proves the wiring at the `execute_run_task` orchestration layer
/// (config → prompt → registry → transcript), not just at the
/// `InProcessAgentRunner` builder layer that test already covers.
/// What: seeds a project with `.claude/skills/demo/SKILL.md`; scripts the PM
/// to call `use_skill(name: "demo")` on its first turn, then stop; asserts
/// the resolved skill body reached the tool result (via the recorded
/// transcript's tool call) and that some `TurnRecord.tool_calls` contains
/// `"use_skill"`.
/// Test: this test.
#[tokio::test]
async fn use_skill_on_legacy_path_reaches_pm_and_transcript() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");
    let skills_dir = project.path().join(".claude").join("skills").join("demo");
    std::fs::create_dir_all(&skills_dir).expect("mkdir skill dir");
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: Demo skill\n---\nfull demo body\n",
    )
    .expect("write SKILL.md");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        use_skill_response("demo"),
        stop_response("pm: loaded the demo skill"),
    ]));

    let report = execute_run_task(params(&agents, &project, None), llm.clone()).await;

    // The PM's own prompt must have advertised the catalog and the tool.
    let names = llm.first_tool_names();
    assert!(
        names.contains(&"use_skill".to_string()),
        "PM registry must advertise use_skill when a skill catalog resolves; got {names:?}"
    );

    // The recorded transcript must show the PM actually calling use_skill.
    let saw_use_skill = report
        .transcript
        .iter()
        .any(|turn| turn.tool_calls.iter().any(|name| name == "use_skill"));
    assert!(
        saw_use_skill,
        "some TurnRecord.tool_calls must contain \"use_skill\"; transcript was: {:?}",
        report.transcript
    );
}

/// Experimental (refs #2892): the delegated ENGINEER's own per-delegation
/// registry (`ProjectToolFactory::build`) now also registers `use_skill`,
/// backed by the SAME resolver the PM's catalog/tool were built from —
/// mirrors `use_skill_on_legacy_path_reaches_pm_and_transcript` above, but
/// scripts the ENGINEER (not the PM) to be the one that calls `use_skill`.
///
/// Why: before this change, only the PM could fetch a skill's full body on
/// this legacy/bake-off CLI path; the delegated engineer received the
/// catalog text in its prompt (`build_engineer_runner`'s
/// `.with_skills_catalog`) but had no tool to act on it.
/// What: seeds a project with `.claude/skills/demo/SKILL.md`; scripts
/// [PM delegate, engineer use_skill, engineer stop, PM stop]. Asserts (1) the
/// tool schema advertised on the engineer's first request (call idx 1, right
/// after the PM's own delegate call at idx 0) includes `"use_skill"`, and (2)
/// some `TurnRecord` tagged `"python-engineer"` has a `tool_calls` entry for
/// `"use_skill"`.
/// Test: this test.
#[tokio::test]
async fn use_skill_on_legacy_path_reaches_engineer_and_transcript() {
    let agents = agents_dir("openai/gpt-4o-mini");
    let project = tempfile::tempdir().expect("project tempdir");
    let skills_dir = project.path().join(".claude").join("skills").join("demo");
    std::fs::create_dir_all(&skills_dir).expect("mkdir skill dir");
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: Demo skill\n---\nfull demo body\n",
    )
    .expect("write SKILL.md");

    let llm = Arc::new(ScriptedLlm::from_json(&[
        delegate_response("use the demo skill"),
        use_skill_response("demo"),
        stop_response("engineer: loaded the demo skill"),
        stop_response("pm: task complete"),
    ]));

    let report = execute_run_task(params(&agents, &project, None), llm.clone()).await;

    // The engineer's own registry (advertised on its first request, idx 1 —
    // idx 0 is the PM's delegate_to_agent turn) must include use_skill.
    let names = llm.tool_names_at(1);
    assert!(
        names.contains(&"use_skill".to_string()),
        "engineer registry must advertise use_skill when a skill catalog resolves; got {names:?}"
    );

    // The recorded transcript must show the ENGINEER (not just the PM)
    // actually calling use_skill.
    let saw_engineer_use_skill = report.transcript.iter().any(|turn| {
        turn.role == "python-engineer" && turn.tool_calls.iter().any(|name| name == "use_skill")
    });
    assert!(
        saw_engineer_use_skill,
        "some \"python-engineer\" TurnRecord.tool_calls must contain \"use_skill\"; \
         transcript was: {:?}",
        report.transcript
    );
}

/// Negative counterpart to `use_skill_on_legacy_path_reaches_engineer_and_transcript`
/// (code-critic finding on PR #2943): asserts the ELSE branch of
/// `ProjectToolFactory::build` — when `skill_resolver` is `None`, the
/// engineer's advertised registry must NOT include `use_skill`.
///
/// Why: the positive test above proves `Some(resolver)` registers `use_skill`;
/// nothing asserted the inverse. Driving this end-to-end via `execute_run_task`
/// is impractical/flaky here because the #2895 embedded-skill fallback (see
/// `skills::mod`'s `FsSkillResolver`/`format_skill_catalog` docs) makes
/// `daily_driver_skills_catalog` resolve a non-empty catalog for almost any
/// real project directory, so `skill_resolver` is virtually never `None` on
/// the full end-to-end path. Constructing `ProjectToolFactory` directly and
/// calling `RegistryFactory::build` is the clean, direct way to exercise the
/// `None` branch in isolation without fighting that fallback.
/// What: builds a `ProjectToolFactory` with `skill_resolver: None` against an
/// empty project tempdir, calls `.build()` with default `AgentConfig`/
/// `RunContext`, and asserts the resulting `ToolRegistry` does not contain
/// `"use_skill"`, while sanity-checking a known-always-present tool
/// (`"finish_task"`) IS present — guarding against a broken/empty registry
/// vacuously satisfying the absence assertion.
/// Test: this test.
#[tokio::test]
async fn use_skill_absent_from_engineer_when_no_skills() {
    let project = tempfile::tempdir().expect("project tempdir");
    let factory = super::ProjectToolFactory {
        project: project.path().to_path_buf(),
        skill_resolver: None,
    };

    let registry = factory
        .build(&AgentConfig::default(), &RunContext::default())
        .await;

    assert!(
        !registry.contains("use_skill"),
        "engineer registry must NOT advertise use_skill when skill_resolver is None"
    );
    assert!(
        registry.contains("finish_task"),
        "sanity check: registry should still contain other always-present tools"
    );
}
