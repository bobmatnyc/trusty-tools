//! Agent-loop wiring guards for the #7948 permission gate.
//!
//! Why: "`deny` never reaches the tool" is a claim about the LOOP, not the
//! gate — `permissions::tests::gate_tests` proves the gate returns `Denied`,
//! and only a loop-level test proves the loop honours it.
//! What: a counting tool plus a `deny` rule, asserting zero executions and a
//! recoverable tool result; an `allow` rule dispatching once; and the ungated
//! negative control.
//! Test: this module.

use super::*;

/// `echo`, but counting its executions — a refused call and a call that ran
/// and errored look identical from the transcript alone.
struct CountingTool {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ToolExecutor for CountingTool {
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        ToolResult::ok("echo: ran")
    }
}

/// A loop over [echo call, stop] with a counting `echo`, optionally gated by
/// a headless gate for `yaml`.
fn counting_loop(yaml: Option<&str>) -> (AgentLoop, Arc<ScriptedLlm>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CountingTool {
        calls: Arc::clone(&calls),
    }));
    let llm = Arc::new(ScriptedLlm::from_json(&[
        tool_call_response("call-1", "hello"),
        stop_response("done"),
    ]));
    let mut agent = make_loop(llm.clone(), Arc::new(registry), AgentLoopConfig::default());
    if let Some(yaml) = yaml {
        let document = format!("---\nname: t\nrole: engineer\n{yaml}---\n\nBody.\n");
        let config = crate::agents::config::AgentConfig {
            permissions: crate::permissions::parse_permissions("test-agent", &document)
                .expect("fixture must parse"),
            ..Default::default()
        };
        let gate = crate::permissions::PermissionContext::headless(
            crate::permissions::PermissionMode::Default,
        )
        .gate_for(Arc::new(config), "agent-1");
        agent = agent.with_permission_gate(Arc::new(gate));
    }
    (agent, llm, calls)
}

/// (#7948 regression) A `deny` rule refuses the call WITHOUT executing the
/// tool, and the model receives a recoverable result naming the rule.
/// Test: this test.
#[tokio::test]
async fn deny_never_dispatches() {
    let (agent, llm, calls) = counting_loop(Some("permissions:\n  echo: deny\n"));
    let out = agent
        .run("system", "task")
        .await
        .expect("the loop must not abort");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a denied tool must never execute"
    );
    let refusal = llm
        .requests()
        .into_iter()
        .flatten()
        .filter_map(|m| m.content.clone())
        .find(|c| c.contains("denied by policy"))
        .expect("the refusal must be fed back to the model as a tool result");
    assert!(refusal.contains("echo"), "must name the rule: {refusal}");
    assert!(
        !out.content.is_empty(),
        "the loop still produced its final turn"
    );
}

/// A headless `ask` is refused without executing, the same way.
/// Test: this test.
#[tokio::test]
async fn headless_ask_never_dispatches() {
    let (agent, _llm, calls) = counting_loop(Some("permissions:\n  echo: ask\n"));
    agent
        .run("system", "task")
        .await
        .expect("the loop must not abort");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// An `allow` rule dispatches normally — the gate is not a blanket refusal.
/// Test: this test.
#[tokio::test]
async fn allow_rule_dispatches() {
    let (agent, _llm, calls) = counting_loop(Some("permissions:\n  echo: allow\n"));
    agent
        .run("system", "task")
        .await
        .expect("the loop must not abort");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// NEGATIVE CONTROL: with no gate the same script dispatches exactly once.
/// Test: this test.
#[tokio::test]
async fn ungated_loop_still_dispatches() {
    let (agent, _llm, calls) = counting_loop(None);
    agent
        .run("system", "task")
        .await
        .expect("the loop must not abort");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
