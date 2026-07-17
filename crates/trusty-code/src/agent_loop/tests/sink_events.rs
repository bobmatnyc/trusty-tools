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

use super::*;

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
    /// `concurrently_spawned_same_named_loops_get_distinct_agent_ids` reads
    /// this.
    started_ids: Mutex<Vec<(String, String)>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            started_ids: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("lock poisoned").clone()
    }

    fn started_ids(&self) -> Vec<(String, String)> {
        self.started_ids.lock().expect("lock poisoned").clone()
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
/// `agent` alone cannot distinguish two concurrently-delegated sub-agents of
/// the same kind (e.g. two `python-engineer` delegations); `agent_id` is the
/// stable per-spawn correlation key production mints once per delegation
/// (`runner::in_process::InProcessAgentRunner::run_pipeline`). This test
/// pins that same contract at the `AgentLoop`/sink layer directly, without
/// needing the full runner.
/// What: run two loops both declared `with_agent("python-engineer")` but with
/// distinct `with_agent_id(...)` values against one shared sink; assert both
/// recorded `(agent, agent_id)` pairs share `agent` but differ in `agent_id`.
/// Test: this test.
#[tokio::test]
async fn concurrently_spawned_same_named_loops_get_distinct_agent_ids() {
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
