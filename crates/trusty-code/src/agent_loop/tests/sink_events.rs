//! (UI Phase 1) `ToolEventSink` guards: call ordering, agent attribution, and
//! structured-telemetry forwarding.
//!
//! Why: the UI's core bet is that a change is explained by the agent, the
//! searches, and the memories that produced it. That needs two things this
//! module pins: every tool event must name WHICH agent dispatched it (the PM
//! and a delegated engineer share ONE `ToolEventSink`, so ordering-based
//! inference is not enough), and a tool's structured telemetry must reach the
//! sink ADDITIVELY — the generic tool events must keep firing unchanged so
//! existing consumers are untouched. Split into this focused child module
//! (from `agent_loop::tests`) to keep the parent file under its SLOC cap while
//! reusing its scripted-LLM harness verbatim via `use super::*`.
//! What: owns `RecordingSink` (every sink assertion in the crate's loop tests
//! now lives here) and reuses the parent module's `ScriptedLlm`,
//! `registry_with_echo`, and `make_loop` helpers via `use super::*`.
//! Test: this module is itself the test surface.

use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use futures_util::stream;
use trusty_common::inference::{ChatStream, ChatStreamEvent, StopReason, StreamCompletion, Usage};

use super::*;

/// An `InferenceAdapter` whose `chat_stream` yields text in SEVERAL fragments.
///
/// Why (#4425): every other double in this crate inherits the trait's buffered
/// `chat_stream` default, which replays a finished response as ONE delta — so
/// none of them can tell "streaming works" apart from "the fallback ran". This
/// double emits real multi-fragment output, which is what an OpenAI-dialect SSE
/// turn looks like, and counts BOTH entry points so a test can also prove the
/// sink-less path never opens a stream.
/// What: `chunks` are emitted in order as [`ChatStreamEvent::Delta`]s followed
/// by a terminal `Done` with `finish_reason: stop` (no tool calls, so the loop
/// ends after one turn). `chat` returns the same turn buffered, so the blocking
/// path yields identical content.
/// Test: `native_streaming_transport_emits_incremental_deltas`,
/// `no_sink_uses_the_blocking_chat_path`.
struct ChunkedStreamingLlm {
    chunks: Vec<String>,
    chat_calls: AtomicUsize,
    stream_calls: AtomicUsize,
}

impl ChunkedStreamingLlm {
    fn new(chunks: Vec<&str>) -> Self {
        Self {
            chunks: chunks.into_iter().map(str::to_string).collect(),
            chat_calls: AtomicUsize::new(0),
            stream_calls: AtomicUsize::new(0),
        }
    }

    /// The concatenation of every chunk — the turn's full text.
    fn full_text(&self) -> String {
        self.chunks.concat()
    }

    fn chat_calls(&self) -> usize {
        self.chat_calls.load(AtomicOrdering::SeqCst)
    }

    fn stream_calls(&self) -> usize {
        self.stream_calls.load(AtomicOrdering::SeqCst)
    }
}

#[async_trait]
impl InferenceAdapter for ChunkedStreamingLlm {
    crate::llm::mock_adapter_identity!("mock-chunked-streaming");

    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        self.chat_calls.fetch_add(1, AtomicOrdering::SeqCst);
        let mut assembly = crate::llm::StreamAssembly::new();
        assembly.push(ChatStreamEvent::Delta(self.full_text()));
        assembly.push(ChatStreamEvent::Done(StreamCompletion {
            finish_reason: Some(StopReason::Stop),
            usage: Usage::default(),
        }));
        Ok(assembly.into_response("gen-chunked", "mock/model"))
    }

    async fn chat_stream(&self, _req: &ChatRequest) -> Result<ChatStream, InferenceError> {
        self.stream_calls.fetch_add(1, AtomicOrdering::SeqCst);
        let mut events: Vec<Result<ChatStreamEvent, InferenceError>> = self
            .chunks
            .iter()
            .map(|c| Ok(ChatStreamEvent::Delta(c.clone())))
            .collect();
        events.push(Ok(ChatStreamEvent::Done(StreamCompletion {
            finish_reason: Some(StopReason::Stop),
            usage: Usage::default(),
        })));
        Ok(Box::pin(stream::iter(events)))
    }
}

/// A `ToolEventSink` that records every call as a tagged string, in order.
///
/// Why: The sink's whole purpose is call-order + argument fidelity; recording
/// each hook as `"started:agent:tool:call_id"` /
/// `"finished:agent:tool:call_id:success"` / `"error:agent:tool:call_id"` /
/// `"telemetry:agent:tool:call_id:kind"` lets a test assert the exact
/// sequence AND (UI Phase 1) its attribution with one `Vec<String>`
/// comparison.
struct RecordingSink {
    calls: Mutex<Vec<String>>,
    /// (DOC-39 AC-13) `(agent, agent_id)` pairs seen by `tool_started`, in
    /// call order — kept separate from `calls` so every pre-existing
    /// assertion against `calls`'s string format is untouched; only
    /// `sequentially_spawned_same_named_loops_get_distinct_agent_ids` reads
    /// this.
    started_ids: Mutex<Vec<(String, String)>>,
    /// (tcode streaming epic #3696, Gap A, Slice 1) every `agent_message`
    /// call — kept separate from `calls` for the same reason `started_ids`
    /// is: only the delta-emission tests read this.
    messages: Mutex<Vec<RecordedMessage>>,
}

/// One recorded `ToolEventSink::agent_message` call (tcode streaming epic
/// #3696, Gap A, Slice 1). A named struct rather than a tuple so
/// `RecordingSink::messages` doesn't trip clippy's `type_complexity` lint.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedMessage {
    agent: String,
    agent_id: String,
    turn_id: String,
    delta: String,
    done: bool,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            started_ids: Mutex::new(Vec::new()),
            messages: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("lock poisoned").clone()
    }

    fn started_ids(&self) -> Vec<(String, String)> {
        self.started_ids.lock().expect("lock poisoned").clone()
    }

    fn messages(&self) -> Vec<RecordedMessage> {
        self.messages.lock().expect("lock poisoned").clone()
    }
}

#[async_trait]
impl ToolEventSink for RecordingSink {
    async fn tool_started(
        &self,
        agent: &str,
        agent_id: &str,
        call_id: &str,
        tool: &str,
        _args_preview: &str,
    ) {
        self.calls
            .lock()
            .expect("lock poisoned")
            .push(format!("started:{agent}:{tool}:{call_id}"));
        self.started_ids
            .lock()
            .expect("lock poisoned")
            .push((agent.to_string(), agent_id.to_string()));
    }

    async fn tool_finished(
        &self,
        agent: &str,
        _agent_id: &str,
        call_id: &str,
        tool: &str,
        success: bool,
        _result_preview: &str,
    ) {
        self.calls
            .lock()
            .expect("lock poisoned")
            .push(format!("finished:{agent}:{tool}:{call_id}:{success}"));
    }

    async fn tool_error(
        &self,
        agent: &str,
        _agent_id: &str,
        call_id: &str,
        tool: &str,
        _error: &str,
    ) {
        self.calls
            .lock()
            .expect("lock poisoned")
            .push(format!("error:{agent}:{tool}:{call_id}"));
    }

    async fn tool_telemetry(
        &self,
        agent: &str,
        _agent_id: &str,
        call_id: &str,
        tool: &str,
        telemetry: &crate::tools::telemetry::ToolTelemetry,
    ) {
        let kind = match telemetry {
            crate::tools::telemetry::ToolTelemetry::Search(t) => format!("search:{}", t.lane),
            crate::tools::telemetry::ToolTelemetry::Recall(t) => {
                format!("recall:{}", t.results.iter().filter(|r| r.injected).count())
            }
        };
        self.calls
            .lock()
            .expect("lock poisoned")
            .push(format!("telemetry:{agent}:{tool}:{call_id}:{kind}"));
    }

    async fn agent_message(
        &self,
        agent: &str,
        agent_id: &str,
        turn_id: &str,
        delta: &str,
        done: bool,
    ) {
        self.messages
            .lock()
            .expect("lock poisoned")
            .push(RecordedMessage {
                agent: agent.to_string(),
                agent_id: agent_id.to_string(),
                turn_id: turn_id.to_string(),
                delta: delta.to_string(),
                done,
            });
    }
}

/// A sink must observe `tool_started` then `tool_finished(success=true)`, in
/// that order, for a successful dispatch.
///
/// Why: This is the exact sequence #2056's daemon-driven task execution relies
/// on to stream live `tool_started`/`tool_finished` events to an attached
/// client — a regression here would silently break that observability.
/// What: Script [tool_call, stop]; attach a `RecordingSink`; assert its call
/// log is exactly `["started:unknown:echo:call-1",
/// "finished:unknown:echo:call-1:true"]` — `unknown` because this loop sets
/// no agent (see `unattributed_loop_emits_unknown_agent`).
/// Test: this test.
#[tokio::test]
async fn sink_receives_started_then_finished_in_order() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call-1", "hi"),
        stop_response("done"),
    ]));
    let registry = registry_with_echo(false);
    let sink = Arc::new(RecordingSink::new());

    let agent =
        make_loop(llm, registry, AgentLoopConfig::default()).with_tool_event_sink(sink.clone());
    agent
        .run("sys", "task")
        .await
        .expect("loop should complete");

    assert_eq!(
        sink.calls(),
        vec![
            "started:unknown:echo:call-1",
            "finished:unknown:echo:call-1:true"
        ]
    );
}

/// A recoverable `ToolResult::Error` must notify `tool_finished(success=false)`,
/// NOT `tool_error` — only a fatal/non-recoverable error is exceptional.
///
/// Why: #2055's taxonomy reserves `tool_error` for exceptional failures (tool
/// crash/timeout); an ordinary recoverable tool error (the model can retry) is
/// still a "the tool finished, unsuccessfully" event, not an exceptional one.
/// What: Script a tool call against the failing echo tool (`EchoTool { fail:
/// true }`, which returns `ToolResult::err` — recoverable); assert the sink saw
/// `finished:...:false`, never `error:...`.
/// Test: this test.
#[tokio::test]
async fn sink_recoverable_error_is_finished_not_error() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call-1", "hi"),
        stop_response("done"),
    ]));
    let registry = registry_with_echo(true);
    let sink = Arc::new(RecordingSink::new());

    let agent =
        make_loop(llm, registry, AgentLoopConfig::default()).with_tool_event_sink(sink.clone());
    agent
        .run("sys", "task")
        .await
        .expect("loop should complete");

    assert_eq!(
        sink.calls(),
        vec![
            "started:unknown:echo:call-1",
            "finished:unknown:echo:call-1:false"
        ]
    );
}

/// A tool whose result carries structured telemetry, standing in for
/// `search_code`/`recall_session` without needing a live daemon.
///
/// Why: the loop's forwarding contract (telemetry reaches `tool_telemetry`,
/// attributed, AFTER the generic hook) is independent of which real tool
/// produced it, so it is tested here against a stub and separately against
/// each real tool's own telemetry-shaping tests.
/// What: returns a fixed `SearchTelemetry` on every call.
/// Test: `sink_receives_tool_telemetry_with_agent`.
struct TelemetryTool;

#[async_trait]
impl ToolExecutor for TelemetryTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "echo",
                "description": "Echo the provided text back.",
                "parameters": {
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }
            }
        })
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        ToolResult::ok("searched").with_telemetry(crate::tools::telemetry::ToolTelemetry::Search(
            crate::tools::telemetry::SearchTelemetry {
                lane: "semantic".to_string(),
                query: "q".to_string(),
                hit_count: Some(4),
                hits: vec![],
                latency_ms: 3,
            },
        ))
    }
}

/// Every tool event must be attributed to the agent the loop was told it is
/// running as (UI Phase 1).
///
/// Why: THE keystone of the UI's Phase-1 API — without it a client cannot say
/// which agent drove a change except by fragile `AgentSpawned`/`AgentDone`
/// stream-ordering inference. A PM and a delegated engineer share one sink,
/// so the name must ride on each call.
/// What: run a loop declared as `pm`; assert every recorded hook carries `pm`.
/// Test: this test.
#[tokio::test]
async fn sink_events_are_attributed_to_the_agent() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call-1", "hi"),
        stop_response("done"),
    ]));
    let registry = registry_with_echo(false);
    let sink = Arc::new(RecordingSink::new());

    make_loop(llm, registry, AgentLoopConfig::default())
        .with_tool_event_sink(sink.clone())
        .with_agent("pm")
        .run("sys", "task")
        .await
        .expect("loop should complete");

    assert_eq!(
        sink.calls(),
        vec!["started:pm:echo:call-1", "finished:pm:echo:call-1:true"]
    );
}

/// A text assistant turn publishes its content deltas with `done: false` and
/// exactly one terminal `done: true`; a tool-only turn publishes none
/// (tcode streaming epic #3696, Gap B — #4425).
///
/// Why: this is the contract #4425 replaced Gap A (Slice 1)'s single
/// `done: true` call with. A subscriber must be able to append every
/// `done: false` delta as it arrives and treat the `done: true` call purely as
/// "the bubble is complete" — so the terminal call must carry NO text, or a
/// UI that appends every delta would duplicate the whole turn. A tool-only
/// turn still emits nothing: there is no bubble to render.
/// What: script [tool_call_response, stop_response("final answer")]. The
/// scripted mock has no native streaming, so the shared adapter's buffered
/// fallback replays the turn as one delta — giving exactly one `done: false`
/// call carrying the text plus one empty `done: true`.
/// Test: this test.
#[tokio::test]
async fn sink_receives_agent_message_delta_for_text_turn_only() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call-1", "hi"),
        stop_response("final answer"),
    ]));
    let registry = registry_with_echo(false);
    let sink = Arc::new(RecordingSink::new());

    make_loop(llm, registry, AgentLoopConfig::default())
        .with_tool_event_sink(sink.clone())
        .with_agent("pm")
        .with_agent_id("pm-1")
        .run("sys", "task")
        .await
        .expect("loop should complete");

    let messages = sink.messages();
    assert_eq!(
        messages.len(),
        2,
        "a tool-only turn must not emit a delta; expected one content delta \
         plus one terminal delta for the final text turn, got {messages:?}"
    );
    let content = &messages[0];
    assert_eq!(content.agent, "pm");
    assert_eq!(content.agent_id, "pm-1");
    assert!(
        !content.turn_id.is_empty(),
        "turn_id must be minted, not empty"
    );
    assert_eq!(content.delta, "final answer");
    assert!(!content.done, "content deltas must carry done: false");

    let terminal = &messages[1];
    assert_eq!(
        terminal.turn_id, content.turn_id,
        "every delta of one turn shares its turn_id"
    );
    assert_eq!(
        terminal.delta, "",
        "the terminal delta must carry no text — a UI appends every delta"
    );
    assert!(terminal.done, "the last delta of a turn is done: true");
}

/// A natively-streaming transport reaches the sink as SEPARATE `done: false`
/// deltas, in order (#4425).
///
/// Why: the previous test cannot distinguish "streaming works" from "the
/// buffered fallback emits one delta" — both produce a single content call.
/// This one drives a transport whose `chat_stream` yields several fragments,
/// which is what an OpenAI-dialect SSE turn actually looks like, and is the
/// evidence that token-level output reaches a subscriber incrementally rather
/// than as one paste.
/// What: a mock overriding `chat_stream` with four text fragments and a
/// terminal `Done`; assert the sink saw each fragment separately with
/// `done: false`, that concatenating them reproduces the turn, and that the
/// loop still received the assembled text as its final answer.
/// Test: this test.
#[tokio::test]
async fn native_streaming_transport_emits_incremental_deltas() {
    let llm = Arc::new(ChunkedStreamingLlm::new(vec![
        "The ", "quick ", "brown ", "fox",
    ]));
    let registry = registry_with_echo(false);
    let sink = Arc::new(RecordingSink::new());

    let output = AgentLoop::new(AgentLoopConfig::default(), llm, registry)
        .with_tool_event_sink(sink.clone())
        .with_agent("pm")
        .with_agent_id("pm-1")
        .run("sys", "task")
        .await
        .expect("loop should complete");

    let messages = sink.messages();
    let content: Vec<&RecordedMessage> = messages.iter().filter(|m| !m.done).collect();
    assert_eq!(
        content.len(),
        4,
        "each streamed fragment must reach the sink separately, got {messages:?}"
    );
    assert_eq!(
        content
            .iter()
            .map(|m| m.delta.as_str())
            .collect::<Vec<_>>()
            .join(""),
        "The quick brown fox"
    );
    assert!(
        content.iter().all(|m| m.turn_id == content[0].turn_id),
        "all deltas of one turn share a turn_id"
    );

    let terminal: Vec<&RecordedMessage> = messages.iter().filter(|m| m.done).collect();
    assert_eq!(terminal.len(), 1, "exactly one terminal delta per turn");

    // The loop itself must still see the whole turn — streaming is a transport
    // detail, not a change to what the agent produced.
    assert_eq!(output.content, "The quick brown fox");
}

/// With NO sink attached, the loop takes the blocking `chat` path and never
/// calls `chat_stream` (#4425).
///
/// Why: `run_task`'s CLI path and every scripted test attach no sink; opening
/// a stream for them would change the wire request (`stream: true`) and the
/// failure modes of a path that has no consumer for deltas. This pins that
/// the non-streaming callers did not regress.
/// What: a mock that counts both entry points; run without a sink and assert
/// `chat` was used and `chat_stream` never was.
/// Test: this test.
#[tokio::test]
async fn no_sink_uses_the_blocking_chat_path() {
    let llm = Arc::new(ChunkedStreamingLlm::new(vec!["done"]));
    let registry = registry_with_echo(false);

    AgentLoop::new(AgentLoopConfig::default(), llm.clone(), registry)
        .with_agent("pm")
        .run("sys", "task")
        .await
        .expect("loop should complete");

    assert_eq!(llm.chat_calls(), 1, "the blocking path must be used");
    assert_eq!(
        llm.stream_calls(),
        0,
        "a sink-less loop must never open a stream"
    );
}

/// Two loops sharing ONE sink must attribute their calls to their OWN agents
/// (UI Phase 1).
///
/// Why: this is the exact production topology — `task::executor` clones one
/// `Arc<dyn ToolEventSink>` into the PM's loop and the delegated engineer's
/// runner. The requirement is that a UI can tell the two apart on the merged
/// stream; this test is that requirement, stated directly.
/// What: drive a `pm`-declared loop and a `python-engineer`-declared loop
/// against the same sink; assert the merged log distinguishes them.
/// Test: this test.
#[tokio::test]
async fn pm_and_delegated_engineer_are_distinguishable_on_one_sink() {
    let sink = Arc::new(RecordingSink::new());

    for agent in ["pm", "python-engineer"] {
        let llm = Arc::new(ScriptedLlm::from_json(&[
            tool_call_response("call-1", "hi"),
            stop_response("done"),
        ]));
        make_loop(llm, registry_with_echo(false), AgentLoopConfig::default())
            .with_tool_event_sink(sink.clone())
            .with_agent(agent)
            .run("sys", "task")
            .await
            .expect("loop should complete");
    }

    assert_eq!(
        sink.calls(),
        vec![
            "started:pm:echo:call-1",
            "finished:pm:echo:call-1:true",
            "started:python-engineer:echo:call-1",
            "finished:python-engineer:echo:call-1:true",
        ],
        "the PM's and the engineer's calls must be distinguishable even though \
         they share one sink AND reuse the same call_id"
    );
}

/// A loop with no declared agent must emit the documented sentinel, never an
/// empty string (UI Phase 1).
///
/// Why: a blank agent chip in a UI is indistinguishable from a genuine empty
/// name; an explicit `unknown` is a visible wiring bug instead of a silent one.
/// Test: this test.
#[tokio::test]
async fn unattributed_loop_emits_unknown_agent() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call-1", "hi"),
        stop_response("done"),
    ]));
    let sink = Arc::new(RecordingSink::new());

    make_loop(llm, registry_with_echo(false), AgentLoopConfig::default())
        .with_tool_event_sink(sink.clone())
        .run("sys", "task")
        .await
        .expect("loop should complete");

    assert!(
        sink.calls()
            .iter()
            .all(|c| c.contains(crate::events::UNATTRIBUTED_AGENT)),
        "unattributed loops must emit the sentinel: {:?}",
        sink.calls()
    );
    assert!(
        sink.started_ids()
            .iter()
            .all(|(_, id)| id == crate::events::UNATTRIBUTED_AGENT_ID),
        "a loop with no declared agent_id must emit the documented sentinel \
         (DOC-39 AC-13), never an empty string: {:?}",
        sink.started_ids()
    );
}

/// Two loops declared with the SAME `agent` name but DIFFERENT `agent_id`s
/// must stay distinguishable on one shared sink (DOC-39 AC-13.1/13.2).
///
/// Why: this is the direct regression proof for the gap #2862 left open —
/// `agent` alone cannot distinguish two delegated sub-agents of the same
/// kind (e.g. two `python-engineer` delegations); `agent_id` is the stable
/// per-spawn correlation key production mints once per delegation
/// (`runner::in_process::InProcessAgentRunner::run_pipeline`). This test
/// pins that same contract at the `AgentLoop`/sink layer directly, without
/// needing the full runner.
/// What: run two loops both declared `with_agent("python-engineer")` but with
/// distinct `with_agent_id(...)` values against one shared sink; assert both
/// recorded `(agent, agent_id)` pairs share `agent` but differ in `agent_id`.
/// NOTE (code-critic MEDIUM): the two loops here run SEQUENTIALLY (this test
/// awaits the first `.run()` to completion before starting the second) — the
/// name deliberately says "sequentially", not "concurrently", to be honest
/// about that. The genuinely-concurrent claim (two spawns racing under
/// `tokio::join!` against one shared sink) is proven separately by
/// `runner::tests::concurrently_delegated_same_named_agents_get_distinct_ids_under_tokio_join`.
/// Test: this test.
#[tokio::test]
async fn sequentially_spawned_same_named_loops_get_distinct_agent_ids() {
    let sink = Arc::new(RecordingSink::new());

    for agent_id in ["spawn-a", "spawn-b"] {
        let llm = Arc::new(ScriptedLlm::from_json(&[
            tool_call_response("call-1", "echo"),
            stop_response("done"),
        ]));
        make_loop(llm, registry_with_echo(false), AgentLoopConfig::default())
            .with_tool_event_sink(sink.clone())
            .with_agent("python-engineer")
            .with_agent_id(agent_id)
            .run("sys", "task")
            .await
            .expect("loop should complete");
    }

    let ids = sink.started_ids();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0].0, "python-engineer");
    assert_eq!(ids[1].0, "python-engineer");
    assert_eq!(ids[0].1, "spawn-a");
    assert_eq!(ids[1].1, "spawn-b");
    assert_ne!(
        ids[0].1, ids[1].1,
        "two same-named delegated loops must carry DISTINCT agent_ids \
         (DOC-39 AC-13)"
    );
}

/// A tool's structured telemetry must reach `tool_telemetry`, attributed, and
/// ONLY IN ADDITION to the generic tool events (UI Phase 1).
///
/// Why: the structured events are additive by design — existing consumers of
/// `tool_started`/`tool_finished` must see the exact same stream they always
/// did, so a UI can adopt the new events incrementally.
/// What: dispatch a tool returning `SearchTelemetry`; assert the sink saw
/// started -> finished -> telemetry, in that order, all attributed.
/// Test: this test.
#[tokio::test]
async fn sink_receives_tool_telemetry_with_agent() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call-1", "hi"),
        stop_response("done"),
    ]));
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(TelemetryTool));
    let sink = Arc::new(RecordingSink::new());

    make_loop(llm, Arc::new(reg), AgentLoopConfig::default())
        .with_tool_event_sink(sink.clone())
        .with_agent("python-engineer")
        .run("sys", "task")
        .await
        .expect("loop should complete");

    assert_eq!(
        sink.calls(),
        vec![
            "started:python-engineer:echo:call-1",
            "finished:python-engineer:echo:call-1:true",
            "telemetry:python-engineer:echo:call-1:search:semantic",
        ],
        "telemetry must be ADDITIVE — the generic events must still fire, \
         unchanged, before it"
    );
}

/// A tool that reports no telemetry must not trigger the hook at all
/// (UI Phase 1).
///
/// Why: guards the "costs nothing when absent" property — every non-retrieval
/// tool must leave the stream exactly as it was pre-ticket.
/// Test: this test.
#[tokio::test]
async fn tools_without_telemetry_emit_no_telemetry_event() {
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call-1", "hi"),
        stop_response("done"),
    ]));
    let sink = Arc::new(RecordingSink::new());

    make_loop(llm, registry_with_echo(false), AgentLoopConfig::default())
        .with_tool_event_sink(sink.clone())
        .with_agent("pm")
        .run("sys", "task")
        .await
        .expect("loop should complete");

    assert!(
        !sink.calls().iter().any(|c| c.starts_with("telemetry:")),
        "a tool with no telemetry must not emit a structured event: {:?}",
        sink.calls()
    );
}
