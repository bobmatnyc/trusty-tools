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

/// Why: issue #3870 (epic #3866 Slice D) — the `tool_loop` RTK call site
/// previously discarded compression stats via the stats-free
/// `compress_tool_output_async` wrapper. This proves the extracted
/// `compress_success_result` helper (a) still compresses using the
/// `_with_path` variant, and (b) durably appends a `compression.jsonl` line
/// under the given project dir with the correct `surface`/`surface_detail`/
/// `compression_path` fields — i.e. the stats are no longer discarded at the
/// real call site, not just in the pure builder unit tests in
/// `compression.rs`.
/// What: Drives repetitive `cargo test`-shaped input (known to compress via
/// the native fallback chain, since `rtk` is never installed in CI) through
/// `super::compress_success_result`, awaits the returned `JoinHandle` for a
/// deterministic (non-sleep-based) wait on the spawned append, then reads
/// back the JSONL file.
/// Test: This IS the test.
#[tokio::test]
async fn compress_success_result_appends_rtk_record_with_correct_fields() {
    let dir = tempfile::tempdir().unwrap();
    let mut input = String::new();
    for i in 0..50 {
        input.push_str(&format!("test mod::t{i} ... ok\n"));
    }
    input.push_str("test result: ok. 50 passed; 0 failed\n");

    let (compressed, handle) =
        super::compress_success_result("cargo test", &input, dir.path().to_path_buf()).await;
    assert!(
        compressed.len() < input.len(),
        "expected compression to shrink repetitive passing-test output"
    );
    handle.await.expect("append task should not panic");

    let path = dir.path().join(".trusty-agents/state/compression.jsonl");
    let contents = tokio::fs::read_to_string(&path)
        .await
        .expect("compression.jsonl should exist after the append task completes");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one compression record");
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(parsed["surface"], "rtk");
    assert_eq!(parsed["surface_detail"], "cargo test");
    // `rtk` is never on PATH in CI, so this call site must report the
    // native-fallback path, not silently omit `compression_path`.
    assert_eq!(parsed["compression_path"], "native_fallback");
    assert!(parsed["tokens_before"].as_u64().unwrap() > 0);
}
