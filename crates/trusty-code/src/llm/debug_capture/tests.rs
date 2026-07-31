//! Tests for `llm::debug_capture` (#2264).
//!
//! Covers: the disabled (env-unset) path is a true no-op (same `Arc`, no
//! file); an enabled capture writes one JSONL record per `chat()` call
//! carrying the FULL request (messages, tool schemas) and FULL response
//! (text AND tool-call arguments); a failed inner call is still recorded
//! (`error` set, `response` null); directory-mode picks a fresh per-run
//! file while file-mode appends to the literal path; and two wrappers
//! sharing one sink (the pm/engineer shape every real call site uses) emit
//! a single globally-ordered turn sequence — the same shared-sink shape
//! `run_task::execute_run_task` and `task::executor::run_and_record` both
//! use in production.
//!
//! #4425 adds the streaming failure modes: a `chat_stream()` that fails at the
//! OPEN handshake still occupies its reserved turn index (no gap in the
//! sequence), a stream error is recorded with the ORIGINAL error's
//! retryable/alarm classification rather than a re-wrapped `Transport`, and the
//! decorator forwards the model-aware `capabilities_for` of whatever it wraps.

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;
use tempfile::tempdir;

use super::*;
use crate::llm::{ChatMessage, FunctionCall, FunctionDefinition, ToolCall, ToolDefinition};

/// Serialises every test that mutates the process-wide [`ENV_VAR`] — this
/// var is exclusive to this module (no other test file touches it). A
/// `tokio::sync::Mutex` (not `std::sync::Mutex`), matching
/// `task::mock_llm::MOCK_LLM_ENV_LOCK`'s exact convention: the guard is held
/// across an `.await` in `from_env_opens_literal_file_path_and_captures`,
/// which clippy's `await_holding_lock` correctly flags for a std mutex.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A scripted `InferenceAdapter` mock: replays a fixed sequence of
/// `Result<ChatResponse, InferenceError>`, panicking (via `InferenceError::MissingConfig`)
/// if called past the end — mirrors `task::mock_llm::EchoLlmClient`'s shape.
struct ScriptedLlm {
    script: Vec<Result<ChatResponse, ()>>,
    cursor: AtomicUsize,
}

impl ScriptedLlm {
    fn new(script: Vec<Result<ChatResponse, ()>>) -> Self {
        Self {
            script,
            cursor: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl InferenceAdapter for ScriptedLlm {
    crate::llm::mock_adapter_identity!("mock-scripted");

    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        match self.script.get(idx) {
            Some(Ok(resp)) => Ok(resp.clone()),
            Some(Err(())) => Err(InferenceError::Api {
                status: 500,
                body: "scripted failure".into(),
            }),
            None => Err(InferenceError::MissingConfig("script exhausted".into())),
        }
    }
}

/// The error a streaming mock raises, in both failure modes. `MissingConfig`
/// is deliberately chosen because its classification is the exact INVERSE of
/// `InferenceError::Transport`'s (`is_alarm` true / `is_retryable` false), so a
/// record that flattened it into `Transport` is detectable from the transcript
/// alone (#4425 finding 2).
const STREAM_FAILURE: &str = "AWS_REGION not set";

/// A mock whose `chat_stream` fails at the stream-OPEN handshake — the stream
/// is never created, so the caller sees `Err` from `chat_stream` itself.
///
/// Why: this is the failure mode #4425 finding 1 is about — the decorator has
/// already reserved a turn index by the time the handshake fails.
struct StreamOpenFailsLlm;

#[async_trait]
impl InferenceAdapter for StreamOpenFailsLlm {
    crate::llm::mock_adapter_identity!("mock-stream-open-fails");

    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        Err(InferenceError::MissingConfig(STREAM_FAILURE.into()))
    }

    async fn chat_stream(&self, _req: &ChatRequest) -> Result<ChatStream, InferenceError> {
        Err(InferenceError::MissingConfig(STREAM_FAILURE.into()))
    }
}

/// A mock whose stream OPENS successfully and then yields one `Err` item.
///
/// Why: the mid-flight failure mode (#4425 finding 2) — the decorator observes
/// a borrowed `&InferenceError` inside the stream's map closure.
struct StreamErrorsMidFlightLlm;

#[async_trait]
impl InferenceAdapter for StreamErrorsMidFlightLlm {
    crate::llm::mock_adapter_identity!("mock-stream-errors");

    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        Err(InferenceError::MissingConfig(STREAM_FAILURE.into()))
    }

    async fn chat_stream(&self, _req: &ChatRequest) -> Result<ChatStream, InferenceError> {
        Ok(Box::pin(futures_util::stream::iter(vec![Err(
            InferenceError::MissingConfig(STREAM_FAILURE.into()),
        )])))
    }
}

/// A mock that ROUTES its capability answer by model slug — the shape
/// `OpenAiCompatClient`/`DispatchingLlmClient` have.
///
/// Why: a transparent decorator must not collapse that routing back to one
/// provider (#4425 finding 3).
struct RoutingCapabilitiesLlm;

#[async_trait]
impl InferenceAdapter for RoutingCapabilitiesLlm {
    fn name(&self) -> &str {
        "mock-routing"
    }

    fn capabilities(&self) -> &trusty_common::inference::ProviderCapabilities {
        trusty_common::inference::capabilities(trusty_common::inference::ProviderId::OpenRouter)
    }

    fn capabilities_for(&self, model: &str) -> &trusty_common::inference::ProviderCapabilities {
        if model.starts_with("fireworks/") {
            trusty_common::inference::capabilities(trusty_common::inference::ProviderId::Fireworks)
        } else {
            trusty_common::inference::capabilities(trusty_common::inference::ProviderId::OpenRouter)
        }
    }

    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        Err(InferenceError::MissingConfig("test double".into()))
    }
}

/// Build a minimal request carrying a system prompt, one tool schema, and a
/// user turn — enough to prove the FULL request (not a summary) round-trips
/// into the capture record.
fn sample_request(user_text: &str) -> ChatRequest {
    ChatRequest {
        model: "openai/gpt-4o-mini".into(),
        messages: vec![
            ChatMessage::system("you are a careful engineer"),
            ChatMessage::user(user_text),
        ],
        temperature: Some(0.0),
        max_tokens: Some(512),
        tools: Some(vec![ToolDefinition::function(FunctionDefinition {
            name: "write_file".into(),
            description: Some("Write a file".into()),
            parameters: Some(serde_json::json!({"type": "object"})),
            cache_control: None,
        })]),
        tool_choice: None,
        stop: None,
        usage: None,
    }
}

/// A response whose assistant turn calls `write_file` with a concrete body —
/// proves the capture records tool-call ARGUMENTS, not just the name.
fn tool_call_response() -> ChatResponse {
    serde_json::from_value(serde_json::json!({
        "id": "resp-1",
        "model": "openai/gpt-4o-mini-2024-07-18",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {
                        "name": "write_file",
                        "arguments": "{\"path\":\"src/lib.rs\",\"content\":\"fn main() {}\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 30, "completion_tokens": 12, "total_tokens": 42}
    }))
    .expect("valid fixture")
}

/// Read every JSONL line in `path` as a parsed `serde_json::Value`.
fn read_records(path: &std::path::Path) -> Vec<Value> {
    let content = std::fs::read_to_string(path).expect("read capture file");
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("valid JSON line"))
        .collect()
}

// ── wrap_with_debug_capture ─────────────────────────────────────────────────

/// With no sink configured, `wrap_with_debug_capture` returns the SAME `Arc`
/// — no wrapper allocated, proving the disabled path is truly zero-overhead
/// (#2264 requirement 1).
#[test]
fn wrap_with_debug_capture_returns_inner_unchanged_when_sink_none() {
    let inner: Arc<dyn InferenceAdapter> = Arc::new(ScriptedLlm::new(vec![]));
    let inner_clone = Arc::clone(&inner);
    let wrapped = wrap_with_debug_capture(inner, "pm", None);
    assert!(
        Arc::ptr_eq(&inner_clone, &wrapped),
        "expected the exact same Arc when sink is None"
    );
}

/// With a sink configured, `wrap_with_debug_capture` returns a NEW wrapper
/// (not the same `Arc`) and every `chat()` call is captured.
#[tokio::test]
async fn wrap_with_debug_capture_wraps_and_records_when_sink_some() {
    let dir = tempdir().expect("tempdir");
    let capture_path = dir.path().join("run.jsonl");
    let sink = Arc::new(DebugCaptureSink::open(&capture_path).expect("open sink"));

    let inner: Arc<dyn InferenceAdapter> =
        Arc::new(ScriptedLlm::new(vec![Ok(tool_call_response())]));
    let inner_clone = Arc::clone(&inner);
    let wrapped = wrap_with_debug_capture(inner, "pm", Some(&sink));
    assert!(
        !Arc::ptr_eq(&inner_clone, &wrapped),
        "expected a distinct wrapper Arc when sink is Some"
    );

    let req = sample_request("write a stub file");
    wrapped.chat(&req).await.expect("chat succeeds");

    let records = read_records(&capture_path);
    assert_eq!(records.len(), 1, "expected exactly one recorded turn");
}

// ── full request/response capture ───────────────────────────────────────────

/// The recorded request carries the FULL messages array (system + user) and
/// the tool schema — not a summary — closing the first `tcode_report.json`
/// gap named in #2264.
#[tokio::test]
async fn record_captures_full_request_messages_and_tool_schema() {
    let dir = tempdir().expect("tempdir");
    let capture_path = dir.path().join("run.jsonl");
    let sink = Arc::new(DebugCaptureSink::open(&capture_path).expect("open sink"));
    let inner: Arc<dyn InferenceAdapter> =
        Arc::new(ScriptedLlm::new(vec![Ok(tool_call_response())]));
    let wrapped = wrap_with_debug_capture(inner, "python-engineer", Some(&sink));

    let req = sample_request("write a stub file");
    wrapped.chat(&req).await.expect("chat succeeds");

    let records = read_records(&capture_path);
    let request = &records[0]["request"];
    assert_eq!(request["messages"][0]["role"], "system");
    assert_eq!(
        request["messages"][0]["content"],
        "you are a careful engineer"
    );
    assert_eq!(request["messages"][1]["role"], "user");
    assert_eq!(request["messages"][1]["content"], "write a stub file");
    assert_eq!(request["tools"][0]["function"]["name"], "write_file");
    assert_eq!(records[0]["role"], "python-engineer");
}

/// The recorded response carries the tool call's full ARGUMENTS, not merely
/// its name — the second `tcode_report.json` gap named in #2264.
#[tokio::test]
async fn record_captures_tool_call_arguments_not_just_names() {
    let dir = tempdir().expect("tempdir");
    let capture_path = dir.path().join("run.jsonl");
    let sink = Arc::new(DebugCaptureSink::open(&capture_path).expect("open sink"));
    let inner: Arc<dyn InferenceAdapter> =
        Arc::new(ScriptedLlm::new(vec![Ok(tool_call_response())]));
    let wrapped = wrap_with_debug_capture(inner, "python-engineer", Some(&sink));

    wrapped
        .chat(&sample_request("write a stub file"))
        .await
        .expect("chat succeeds");

    let records = read_records(&capture_path);
    let call = &records[0]["response"]["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(call["function"]["name"], "write_file");
    let args: Value = serde_json::from_str(call["function"]["arguments"].as_str().unwrap())
        .expect("arguments is valid JSON");
    assert_eq!(args["path"], "src/lib.rs");
    assert_eq!(args["content"], "fn main() {}");
}

/// Tool RESULTS fed back to the model are captured too — not via a separate
/// mechanism, but because they appear verbatim in the NEXT turn's own
/// request message history, exactly as the agent loop actually sends them.
#[tokio::test]
async fn record_captures_tool_results_via_next_turns_message_history() {
    let dir = tempdir().expect("tempdir");
    let capture_path = dir.path().join("run.jsonl");
    let sink = Arc::new(DebugCaptureSink::open(&capture_path).expect("open sink"));
    let inner: Arc<dyn InferenceAdapter> = Arc::new(ScriptedLlm::new(vec![
        Ok(tool_call_response()),
        Ok(tool_call_response()),
    ]));
    let wrapped = wrap_with_debug_capture(inner, "python-engineer", Some(&sink));

    // Turn 1: no tool result yet.
    wrapped
        .chat(&sample_request("write a stub file"))
        .await
        .expect("turn 1");

    // Turn 2: the agent loop has appended the tool's result as a `tool`-role
    // message before asking the model to continue — exactly what a real
    // `AgentLoop::build_request` does after executing a `ToolCall`.
    let mut turn2_req = sample_request("write a stub file");
    turn2_req.messages.push(ChatMessage::tool_result(
        "call-1",
        "write_file",
        "wrote 13 bytes to src/lib.rs",
    ));
    wrapped.chat(&turn2_req).await.expect("turn 2");

    let records = read_records(&capture_path);
    assert_eq!(records.len(), 2);
    let turn2_messages = &records[1]["request"]["messages"];
    let tool_msg = turn2_messages
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .expect("turn 2 request carries the tool-result message");
    assert_eq!(tool_msg["tool_call_id"], "call-1");
    assert_eq!(tool_msg["content"], "wrote 13 bytes to src/lib.rs");
}

/// A failed inner `chat()` call is still recorded: `response` is null and
/// `error` carries the `InferenceError`'s display text — debugging a failure needs
/// to see exactly what request triggered it.
#[tokio::test]
async fn record_captures_error_when_inner_call_fails() {
    let dir = tempdir().expect("tempdir");
    let capture_path = dir.path().join("run.jsonl");
    let sink = Arc::new(DebugCaptureSink::open(&capture_path).expect("open sink"));
    let inner: Arc<dyn InferenceAdapter> = Arc::new(ScriptedLlm::new(vec![Err(())]));
    let wrapped = wrap_with_debug_capture(inner, "pm", Some(&sink));

    let result = wrapped.chat(&sample_request("do something")).await;
    assert!(
        result.is_err(),
        "the error must still propagate to the caller"
    );

    let records = read_records(&capture_path);
    assert!(records[0]["response"].is_null());
    let err_text = records[0]["error"].as_str().expect("error is a string");
    assert!(err_text.contains("500"), "error text: {err_text}");
}

/// A `chat_stream()` that fails at the OPEN handshake still occupies the turn
/// index it reserved — the sequence has NO gap (#4425 finding 1).
///
/// Why: the decorator claims a turn index before calling the inner
/// `chat_stream`, so an early `?` return left that index permanently unused:
/// the transcript lost the failing request entirely AND every later record was
/// offset from a hole no reader could explain. The blocking `chat` path records
/// in all cases; this pins the streaming path to the same contract.
/// What: fail a stream-open, then run one successful blocking turn through a
/// second wrapper on the SAME sink, and assert the recorded `turn_index`
/// sequence is exactly `[0, 1]` — contiguous from zero. Before the fix the file
/// held one record at index 1.
/// Test: this test.
#[tokio::test]
async fn stream_open_failure_records_its_reserved_turn_index() {
    let dir = tempdir().expect("tempdir");
    let capture_path = dir.path().join("run.jsonl");
    let sink = Arc::new(DebugCaptureSink::open(&capture_path).expect("open sink"));

    let failing: Arc<dyn InferenceAdapter> = Arc::new(StreamOpenFailsLlm);
    let succeeding: Arc<dyn InferenceAdapter> =
        Arc::new(ScriptedLlm::new(vec![Ok(tool_call_response())]));
    let streamer = wrap_with_debug_capture(failing, "pm", Some(&sink));
    let blocker = wrap_with_debug_capture(succeeding, "python-engineer", Some(&sink));

    let opened = streamer.chat_stream(&sample_request("stream me")).await;
    assert!(
        opened.is_err(),
        "the stream-open error must still propagate to the caller"
    );
    blocker
        .chat(&sample_request("the next turn"))
        .await
        .expect("blocking turn succeeds");

    let records = read_records(&capture_path);
    let indices: Vec<u64> = records
        .iter()
        .map(|r| r["turn_index"].as_u64().expect("turn_index is a u64"))
        .collect();
    assert_eq!(
        indices,
        vec![0, 1],
        "a failed stream-open must occupy its reserved index — no gap"
    );
    assert_eq!(records[0]["role"], "pm");
    assert!(
        records[0]["response"].is_null(),
        "a failed stream-open records no response"
    );
    assert_eq!(
        records[0]["error"],
        InferenceError::MissingConfig(STREAM_FAILURE.into()).to_string()
    );
    // The failing request itself must be in the record — it is the whole point
    // of capturing the failure.
    assert_eq!(records[0]["request"]["model"], "openai/gpt-4o-mini");
}

/// A stream error is recorded with the ORIGINAL error's classification, not a
/// re-wrapped `Transport` (#4425 finding 2).
///
/// Why: the decorator used to synthesise
/// `InferenceError::Transport(e.to_string())` for the record, which is the one
/// variant that is ALWAYS retryable and NEVER an alarm. A missing-config or
/// auth failure therefore appeared in the transcript as a transient network
/// blip — the exact opposite classification — making `is_retryable`/`is_alarm`
/// underivable from the capture.
/// What: drive a stream that yields `MissingConfig` (alarm, not retryable) and
/// assert the record carries that error's own Display text plus
/// `error_retryable: false` / `error_alarm: true`. Under the old re-wrapping
/// both booleans were inverted and the text was double-prefixed with
/// "inference transport error:".
/// Test: this test.
#[tokio::test]
async fn stream_error_preserves_the_original_error_classification() {
    let dir = tempdir().expect("tempdir");
    let capture_path = dir.path().join("run.jsonl");
    let sink = Arc::new(DebugCaptureSink::open(&capture_path).expect("open sink"));

    let inner: Arc<dyn InferenceAdapter> = Arc::new(StreamErrorsMidFlightLlm);
    let wrapped = wrap_with_debug_capture(inner, "pm", Some(&sink));

    let mut stream = wrapped
        .chat_stream(&sample_request("stream me"))
        .await
        .expect("the stream opens");
    let first = stream.next().await.expect("one event");
    assert!(first.is_err(), "the error must reach the caller unchanged");
    drop(stream);

    let original = InferenceError::MissingConfig(STREAM_FAILURE.into());
    assert!(original.is_alarm() && !original.is_retryable());

    let records = read_records(&capture_path);
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0]["error"],
        original.to_string(),
        "the recorded error must be the original variant, not a Transport wrapper"
    );
    assert_eq!(
        records[0]["error_retryable"], false,
        "MissingConfig is not retryable; Transport would have recorded true"
    );
    assert_eq!(
        records[0]["error_alarm"], true,
        "MissingConfig is an alarm; Transport would have recorded false"
    );
}

/// The decorator forwards the model-aware `capabilities_for`, not just
/// `capabilities()` (#4425 finding 3).
///
/// Why: `wrap_with_debug_capture` sits between the agent loop and a per-request
/// ROUTING adapter on every production path. If the decorator inherited the
/// trait default it would answer every slug with the wrapped adapter's
/// model-free profile, silently undoing that routing for anyone with
/// `TCODE_DEBUG_TRANSCRIPT` set — a capability answer that changes when
/// debugging is on is worse than none.
/// What: wrap an adapter that routes `fireworks/*` to the Fireworks profile;
/// assert the wrapper reports Fireworks for that slug and OpenRouter otherwise.
/// Test: this test.
#[tokio::test]
async fn decorator_forwards_capabilities_for() {
    let dir = tempdir().expect("tempdir");
    let sink = Arc::new(DebugCaptureSink::open(&dir.path().join("run.jsonl")).expect("open sink"));
    let inner: Arc<dyn InferenceAdapter> = Arc::new(RoutingCapabilitiesLlm);
    let wrapped = wrap_with_debug_capture(inner, "pm", Some(&sink));

    assert_eq!(
        wrapped
            .capabilities_for("fireworks/accounts/fireworks/models/llama-v3p1-70b-instruct")
            .id,
        trusty_common::inference::ProviderId::Fireworks,
        "the decorator must forward the wrapped adapter's slug routing"
    );
    assert_eq!(
        wrapped.capabilities_for("openai/gpt-4o-mini").id,
        trusty_common::inference::ProviderId::OpenRouter
    );
}

/// Two wrappers (pm + engineer) sharing ONE sink — the exact shape
/// `run_task::execute_run_task` and `task::executor::run_and_record` build —
/// emit a single globally-ordered `turn_index` sequence spanning both roles.
#[tokio::test]
async fn shared_sink_gives_monotonic_turn_index_across_roles() {
    let dir = tempdir().expect("tempdir");
    let capture_path = dir.path().join("run.jsonl");
    let sink = Arc::new(DebugCaptureSink::open(&capture_path).expect("open sink"));

    let pm_inner: Arc<dyn InferenceAdapter> =
        Arc::new(ScriptedLlm::new(vec![Ok(tool_call_response())]));
    let engineer_inner: Arc<dyn InferenceAdapter> =
        Arc::new(ScriptedLlm::new(vec![Ok(tool_call_response())]));

    let pm = wrap_with_debug_capture(pm_inner, "pm", Some(&sink));
    let engineer = wrap_with_debug_capture(engineer_inner, "python-engineer", Some(&sink));

    pm.chat(&sample_request("delegate")).await.expect("pm turn");
    engineer
        .chat(&sample_request("do the work"))
        .await
        .expect("engineer turn");

    let records = read_records(&capture_path);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["turn_index"], 0);
    assert_eq!(records[0]["role"], "pm");
    assert_eq!(records[1]["turn_index"], 1);
    assert_eq!(records[1]["role"], "python-engineer");
}

// ── resolve_capture_path ─────────────────────────────────────────────────────

/// An existing directory resolves to a fresh, uniquely-named file inside it.
#[test]
fn resolve_capture_path_existing_dir_gets_uuid_file() {
    let dir = tempdir().expect("tempdir");
    let resolved = resolve_capture_path(dir.path());
    assert_eq!(resolved.parent(), Some(dir.path()));
    let name = resolved.file_name().unwrap().to_string_lossy().to_string();
    assert!(name.starts_with("tcode-debug-"), "name: {name}");
    assert!(name.ends_with(".jsonl"), "name: {name}");
}

/// A non-existent path ending in the platform separator is still treated as
/// a directory (lets a caller pre-declare directory semantics before the
/// directory itself exists).
#[test]
fn resolve_capture_path_trailing_separator_treated_as_dir() {
    let dir = tempdir().expect("tempdir");
    let not_yet_created = dir.path().join("captures");
    let mut raw = not_yet_created.to_string_lossy().to_string();
    raw.push(std::path::MAIN_SEPARATOR);
    let resolved = resolve_capture_path(std::path::Path::new(&raw));
    assert_eq!(resolved.parent(), Some(not_yet_created.as_path()));
}

/// A literal (non-directory) path resolves unchanged.
#[test]
fn resolve_capture_path_literal_file_unchanged() {
    let dir = tempdir().expect("tempdir");
    let file_path = dir.path().join("capture.jsonl");
    let resolved = resolve_capture_path(&file_path);
    assert_eq!(resolved, file_path);
}

// ── from_env ─────────────────────────────────────────────────────────────────

/// `from_env` returns `None` — no file, no behaviour change — when the env
/// var is unset (the default, zero-overhead path).
#[tokio::test]
async fn from_env_none_when_unset() {
    let _guard = ENV_LOCK.lock().await;
    // SAFETY: test-only env mutation, serialised by `ENV_LOCK`.
    unsafe {
        std::env::remove_var(ENV_VAR);
    }
    assert!(DebugCaptureSink::from_env().is_none());
}

/// `from_env` opens a literal file path and produces a working sink whose
/// records land in exactly that file.
#[tokio::test]
async fn from_env_opens_literal_file_path_and_captures() {
    let _guard = ENV_LOCK.lock().await;
    let dir = tempdir().expect("tempdir");
    let capture_path = dir.path().join("via-env.jsonl");
    // SAFETY: test-only env mutation, serialised by `ENV_LOCK`.
    unsafe {
        std::env::set_var(ENV_VAR, &capture_path);
    }
    let sink = DebugCaptureSink::from_env().expect("sink built from env");
    unsafe {
        std::env::remove_var(ENV_VAR);
    }

    let inner: Arc<dyn InferenceAdapter> =
        Arc::new(ScriptedLlm::new(vec![Ok(tool_call_response())]));
    let wrapped = wrap_with_debug_capture(inner, "pm", Some(&sink));
    wrapped
        .chat(&sample_request("hi"))
        .await
        .expect("chat succeeds");

    assert_eq!(read_records(&capture_path).len(), 1);
}

/// Sanity: `FunctionCall`/`ToolCall` remain constructible directly (guards
/// against an accidental breaking change to the request-side types this
/// module depends on for its fixtures).
#[test]
fn tool_call_fixture_constructs() {
    let call = ToolCall {
        id: "x".into(),
        kind: "function".into(),
        function: FunctionCall {
            name: "noop".into(),
            arguments: "{}".into(),
        },
    };
    assert_eq!(call.function.name, "noop");
}
