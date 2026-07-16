//! Tests for the per-turn working-context budget surfaced out of the cadence
//! compressor (epic #2343). Split out of `tests.rs` per the crate's
//! `_tests.rs` sibling-file convention (see `cadence_tests.rs` precedent) to
//! keep that file under the 1500-SLOC test cap. Declared as a CHILD of
//! `agent_loop::tests` so it reuses that module's scripted-LLM/echo-tool
//! harness (`ScriptedLlm`, `make_loop`, `registry_with_echo`, …) via
//! `use super::*` rather than duplicating it.

use super::*;

/// A `ToolEventSink` that captures every `context_budget` snapshot it is
/// handed, ignoring the tool hooks.
///
/// Why: the whole point of surfacing `CadenceOutcome` is that a subscriber can
/// SEE the per-turn budget; a sink that records the snapshots is the smallest
/// thing that proves the value now escapes the loop instead of being dropped.
struct BudgetSink {
    snapshots: Mutex<Vec<ContextBudgetSnapshot>>,
}

impl BudgetSink {
    fn new() -> Self {
        Self {
            snapshots: Mutex::new(Vec::new()),
        }
    }

    fn snapshots(&self) -> Vec<ContextBudgetSnapshot> {
        self.snapshots.lock().expect("lock poisoned").clone()
    }
}

#[async_trait]
impl ToolEventSink for BudgetSink {
    async fn tool_started(&self, _call_id: &str, _tool: &str, _args_preview: &str) {}
    async fn tool_finished(&self, _c: &str, _t: &str, _s: bool, _r: &str) {}
    async fn tool_error(&self, _call_id: &str, _tool: &str, _error: &str) {}

    async fn context_budget(&self, snapshot: &ContextBudgetSnapshot) {
        self.snapshots
            .lock()
            .expect("lock poisoned")
            .push(*snapshot);
    }
}

/// Build the 3-tool-call + stop script the cadence tests share.
fn cadence_script() -> Vec<Value> {
    let mut fixtures: Vec<Value> = (0..3)
        .map(|i| tool_call_response(&format!("call_{i}"), &format!("turn-{i}")))
        .collect();
    fixtures.push(stop_response("all done"));
    fixtures
}

/// A cadence-enabled loop must hand its sink one budget snapshot per turn,
/// carrying REAL measured values (epic #2343).
///
/// Why: `CadenceOutcome` was computed every turn and dropped on the floor —
/// its own doc comment said it existed "for observability/tests" while
/// observability consumed nothing. This asserts the value now reaches a
/// subscriber AND that it is a genuine measurement, not a placeholder: a
/// snapshot whose `overhead_tokens` were always 0 would satisfy a naive
/// "an event fired" test while rendering a meaningless budget meter.
/// What: run a 4-turn scripted loop with `cadence: Some(_)` under
/// `DailyDriver`; assert one snapshot per turn, each with a non-zero context
/// window, a non-zero measured overhead, a cap matching the epic's 40%
/// arithmetic, and percentages that sum to exactly 100.
/// Test: this test.
#[tokio::test]
async fn cadence_emits_context_budget_snapshot() {
    let llm = Arc::new(ScriptedLlm::from_json(&cadence_script()));
    let registry = registry_with_echo(false);
    let sink = Arc::new(BudgetSink::new());

    let agent = make_loop(
        llm,
        registry,
        AgentLoopConfig {
            cadence: Some(CadenceConfig::default()),
            ..AgentLoopConfig::default()
        },
    )
    .with_tool_event_sink(sink.clone());
    agent
        .run("system prompt", "the task")
        .await
        .expect("run completes");

    let snapshots = sink.snapshots();
    assert_eq!(
        snapshots.len(),
        4,
        "one budget snapshot per turn boundary, got {snapshots:?}"
    );
    for s in &snapshots {
        assert!(s.context_window > 0, "window must be resolved: {s:?}");
        assert!(
            s.overhead_tokens > 0,
            "a seeded transcript always costs SOME tokens — a 0 here means the \
             snapshot is a placeholder, not a measurement: {s:?}"
        );
        assert_eq!(
            s.overhead_cap_tokens,
            s.context_window * 40 / 100,
            "cap must be the epic's 40% arithmetic: {s:?}"
        );
        assert_eq!(
            s.working_context_pct as usize + s.overhead_pct as usize,
            100,
            "the two percentages a budget meter renders must sum to 100: {s:?}"
        );
        assert!(
            s.within_budget,
            "a tiny test transcript is far under cap: {s:?}"
        );
    }
}

/// A loop with cadence DISABLED must never emit a budget snapshot — this is
/// the PM-only gate (#2346 scope).
///
/// Why: cadence is PM-only by construction (`AgentLoopConfig.cadence` defaults
/// to `None`; only `task::executor` sets `Some`). A delegated ENGINEER loop
/// shares the very same `Arc<dyn ToolEventSink>` as its PM, so emitting from
/// an un-gated path would publish engineer-loop budget events onto the PM's
/// session stream and corrupt the UI's budget indicator with a second,
/// unrelated context. Asserting the default config emits NOTHING is what pins
/// that gate.
/// What: identical script to `cadence_emits_context_budget_snapshot` but with
/// the default `cadence: None` config (exactly what `build_engineer_runner`
/// leaves in place); assert the sink saw zero snapshots.
/// Test: this test.
#[tokio::test]
async fn no_context_budget_event_when_cadence_disabled() {
    let llm = Arc::new(ScriptedLlm::from_json(&cadence_script()));
    let registry = registry_with_echo(false);
    let sink = Arc::new(BudgetSink::new());

    let agent =
        make_loop(llm, registry, AgentLoopConfig::default()).with_tool_event_sink(sink.clone());
    agent
        .run("system prompt", "the task")
        .await
        .expect("run completes");

    assert!(
        sink.snapshots().is_empty(),
        "cadence: None (the delegated engineer's config) must emit no budget \
         events, got {:?}",
        sink.snapshots()
    );
}

/// `Parity` mode must not emit budget snapshots even with cadence attached.
///
/// Why: mirrors `cadence_never_fires_in_parity_mode` — the byte-identical
/// benchmark guarantee means cadence never runs under Parity, so there is no
/// measurement to report and emitting one would imply work that did not happen.
/// What: `mode: Parity` + an aggressive `cadence: Some(_)`; assert no snapshots.
/// Test: this test.
#[tokio::test]
async fn no_context_budget_event_in_parity_mode() {
    let llm = Arc::new(ScriptedLlm::from_json(&cadence_script()));
    let registry = registry_with_echo(false);
    let sink = Arc::new(BudgetSink::new());

    let agent = make_loop(
        llm,
        registry,
        AgentLoopConfig {
            mode: crate::mode::HarnessMode::Parity,
            cadence: Some(CadenceConfig {
                cadence_turns: 1,
                ..CadenceConfig::default()
            }),
            ..AgentLoopConfig::default()
        },
    )
    .with_tool_event_sink(sink.clone());
    agent
        .run("system prompt", "the task")
        .await
        .expect("run completes");

    assert!(
        sink.snapshots().is_empty(),
        "Parity must never emit a budget event"
    );
}
