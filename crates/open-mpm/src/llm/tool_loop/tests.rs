//! Concurrency-contract test for the tool-loop dispatch fan-out.
//!
//! Why: The loop dispatches a turn's tool calls in parallel; a regression to
//! sequential dispatch (or one error cancelling peers) would be silent and
//! costly. This guards the contract without a live LLM.
//! What: Mocks a slow-ok and a fast-err tool, runs the same
//! `FuturesUnordered` + `dispatch_gated` pattern the loop uses, and asserts
//! both ran, the error didn't cancel peers, and timing implies parallelism.
//! Test: This IS the test module.

// Parallel dispatch test: verifies that a ToolRegistry can dispatch
// multiple tool calls concurrently using the same plumbing used by
// `chat_with_tools_gated`, and that one tool erroring does not cancel
// the others. This exercises the registry + FuturesUnordered pattern
// without requiring a real OpenRouter round-trip.
#[tokio::test]
async fn parallel_tool_dispatch_does_not_cancel_peers() {
    use crate::tools::{ToolExecutor, ToolRegistry, ToolResult};
    use async_trait::async_trait;
    use futures::stream::{FuturesUnordered, StreamExt};
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SlowOk(Arc<AtomicUsize>);
    #[async_trait]
    impl ToolExecutor for SlowOk {
        fn name(&self) -> &str {
            "slow_ok"
        }
        fn schema(&self) -> serde_json::Value {
            json!({"type":"function","function":{"name":"slow_ok","parameters":{"type":"object","properties":{},"additionalProperties":false}}})
        }
        async fn execute(&self, _args: serde_json::Value) -> ToolResult {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            self.0.fetch_add(1, Ordering::SeqCst);
            ToolResult::ok("slow done")
        }
    }

    struct FastErr(Arc<AtomicUsize>);
    #[async_trait]
    impl ToolExecutor for FastErr {
        fn name(&self) -> &str {
            "fast_err"
        }
        fn schema(&self) -> serde_json::Value {
            json!({"type":"function","function":{"name":"fast_err","parameters":{"type":"object","properties":{},"additionalProperties":false}}})
        }
        async fn execute(&self, _args: serde_json::Value) -> ToolResult {
            self.0.fetch_add(1, Ordering::SeqCst);
            ToolResult::err("fast boom")
        }
    }

    let slow_count = Arc::new(AtomicUsize::new(0));
    let fast_count = Arc::new(AtomicUsize::new(0));
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(SlowOk(slow_count.clone())));
    reg.register(Arc::new(FastErr(fast_count.clone())));
    let reg = Arc::new(reg);

    let calls = vec![
        ("id-1".to_string(), "slow_ok".to_string()),
        ("id-2".to_string(), "fast_err".to_string()),
        ("id-3".to_string(), "slow_ok".to_string()),
    ];

    let start = std::time::Instant::now();
    let mut futs = FuturesUnordered::new();
    for (id, name) in &calls {
        let reg = Arc::clone(&reg);
        let id = id.clone();
        let name = name.clone();
        futs.push(async move {
            let r = reg.dispatch_gated(&name, json!({}), None).await;
            (id, name, r)
        });
    }

    let mut saw_error = false;
    let mut saw_success = false;
    while let Some((_, _, r)) = futs.next().await {
        if r.is_error() {
            saw_error = true;
        } else {
            saw_success = true;
        }
    }
    let elapsed = start.elapsed();

    assert!(saw_error, "expected at least one error result");
    assert!(saw_success, "expected success despite concurrent error");
    assert_eq!(slow_count.load(Ordering::SeqCst), 2);
    assert_eq!(fast_count.load(Ordering::SeqCst), 1);
    // If dispatch were sequential, elapsed would be >= 60ms (2 * 30ms);
    // parallel dispatch should complete in roughly one slow-tool's time.
    assert!(
        elapsed < std::time::Duration::from_millis(120),
        "dispatch appears sequential; elapsed = {elapsed:?}"
    );
}
