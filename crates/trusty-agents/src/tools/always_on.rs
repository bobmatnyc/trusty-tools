//! Always-on tool execution and live-context assembly (#447).
//!
//! Why: Some tools produce small, deterministic context (current time, recent
//! memory, project status) that the LLM would always benefit from having
//! pre-loaded. Forcing the model to issue a tool call to fetch them burns a
//! turn and inflates latency. Marking such tools `AlwaysOn` lets the harness
//! run them concurrently *before* each LLM call and prepend their output as
//! a `## Live Context` block — the LLM sees only `OnDemand` tools in its
//! tool list and treats live context as part of the prompt.
//! What: `build_live_context` partitions a registry, runs every always-on
//! tool concurrently with a per-tool 5s timeout, and assembles the markdown
//! block. Failing tools are non-fatal: they log at WARN and contribute
//! `"[unavailable]"`. An empty assembled block (all tools failed or no
//! always-on tools registered) returns `None` so callers can omit the
//! injection entirely.
//! Test: See `tests` submodule.

use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
use serde_json::json;

use crate::tools::{ToolExecutionTier, ToolExecutor, ToolResult};

/// Pair of tool vectors returned by `partition_by_tier`: `(always_on, on_demand)`.
pub type PartitionedTools = (Vec<Arc<dyn ToolExecutor>>, Vec<Arc<dyn ToolExecutor>>);

/// Default per-tool timeout for always-on context tools.
///
/// Why: Always-on tools run on every LLM call, so a single slow tool would
/// linearize the whole pipeline. A 5s ceiling caps the worst case while
/// still giving network-backed tools (memory recall, project status) a
/// reasonable budget.
/// What: 5 seconds. Override via `build_live_context_with_timeout`.
/// Test: `slow_tool_times_out` exercises both the success and timeout paths.
pub const DEFAULT_ALWAYS_ON_TIMEOUT: Duration = Duration::from_secs(5);

/// Partition tools into `(always_on, on_demand)` by their `execution_tier()`.
///
/// Why: Callers that build the LLM tool list need only the `on_demand` half;
/// the always-on half is run separately to build live context. Doing the
/// partition once keeps the dispatch site short.
/// What: Returns two `Vec`s of cloned `Arc`s. Preserves iteration order of
/// the input.
/// Test: `partition_separates_tiers`.
pub fn partition_by_tier(tools: &[Arc<dyn ToolExecutor>]) -> PartitionedTools {
    let mut always = Vec::new();
    let mut on_demand = Vec::new();
    for t in tools {
        match t.execution_tier() {
            ToolExecutionTier::AlwaysOn => always.push(t.clone()),
            ToolExecutionTier::OnDemand => on_demand.push(t.clone()),
        }
    }
    (always, on_demand)
}

/// Build a `## Live Context` markdown block from every always-on tool.
///
/// Why: See module docstring. The harness invokes this before each LLM call
/// and prepends the result to the user message (or injects as a system
/// context message).
/// What: Filters `tools` to `AlwaysOn`, runs each with `args` (typically
/// `{}`) concurrently under `DEFAULT_ALWAYS_ON_TIMEOUT`, and assembles a
/// markdown block with one `### <tool_name>` heading per tool. Failures
/// (errors or timeouts) emit `"[unavailable]"` and a WARN log so the rest
/// of the prompt still goes through. Returns `None` when there are no
/// always-on tools OR every always-on tool produced `"[unavailable]"`
/// (so we never inject an empty/useless block).
/// Test: `build_live_context_empty_returns_none`,
/// `build_live_context_shapes_block`, `failing_tool_emits_unavailable`,
/// `slow_tool_times_out`.
pub async fn build_live_context(tools: &[Arc<dyn ToolExecutor>]) -> Option<String> {
    build_live_context_with_timeout(tools, DEFAULT_ALWAYS_ON_TIMEOUT).await
}

/// Variant of `build_live_context` with a caller-supplied timeout.
///
/// Why: Tests need to exercise the timeout path without waiting the full
/// 5s default. Production callers should use `build_live_context`.
/// What: Same contract as `build_live_context`, parameterized timeout.
/// Test: `slow_tool_times_out` passes a 50ms timeout.
pub async fn build_live_context_with_timeout(
    tools: &[Arc<dyn ToolExecutor>],
    timeout: Duration,
) -> Option<String> {
    let always: Vec<_> = tools
        .iter()
        .filter(|t| t.execution_tier() == ToolExecutionTier::AlwaysOn)
        .cloned()
        .collect();
    if always.is_empty() {
        return None;
    }

    let futures = always.iter().map(|tool| {
        let tool = tool.clone();
        async move {
            let name = tool.name().to_string();
            let exec = tool.execute(json!({}));
            let outcome = match tokio::time::timeout(timeout, exec).await {
                Ok(ToolResult::Success(s)) => s,
                Ok(ToolResult::Error { message, .. }) => {
                    tracing::warn!(
                        tool = %name,
                        error = %message,
                        "always-on tool errored; injecting [unavailable]"
                    );
                    "[unavailable]".to_string()
                }
                Err(_) => {
                    tracing::warn!(
                        tool = %name,
                        timeout_ms = timeout.as_millis() as u64,
                        "always-on tool timed out; injecting [unavailable]"
                    );
                    "[unavailable]".to_string()
                }
            };
            (name, outcome)
        }
    });

    let results: Vec<(String, String)> = join_all(futures).await;

    // If every result is "[unavailable]", suppress the block entirely so we
    // don't pollute the prompt with a useless section.
    if results.iter().all(|(_, body)| body == "[unavailable]") {
        return None;
    }

    let mut out = String::from("## Live Context\n");
    for (name, body) in results {
        out.push_str(&format!("\n### {name}\n{body}\n"));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::Value;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StaticTool {
        name: &'static str,
        tier: ToolExecutionTier,
        body: &'static str,
        calls: Arc<AtomicUsize>,
    }

    impl StaticTool {
        fn new(name: &'static str, tier: ToolExecutionTier, body: &'static str) -> Arc<Self> {
            Arc::new(Self {
                name,
                tier,
                body,
                calls: Arc::new(AtomicUsize::new(0)),
            })
        }
    }

    #[async_trait]
    impl ToolExecutor for StaticTool {
        fn name(&self) -> &str {
            self.name
        }
        fn schema(&self) -> Value {
            json!({"type":"function","function":{"name":self.name,"description":"","parameters":{"type":"object","properties":{}}}})
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            ToolResult::ok(self.body)
        }
        fn execution_tier(&self) -> ToolExecutionTier {
            self.tier
        }
    }

    struct FailingAlwaysOn;
    #[async_trait]
    impl ToolExecutor for FailingAlwaysOn {
        fn name(&self) -> &str {
            "failing"
        }
        fn schema(&self) -> Value {
            json!({"type":"function","function":{"name":"failing","description":"","parameters":{"type":"object","properties":{}}}})
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::err("kaboom")
        }
        fn execution_tier(&self) -> ToolExecutionTier {
            ToolExecutionTier::AlwaysOn
        }
    }

    struct SlowAlwaysOn;
    #[async_trait]
    impl ToolExecutor for SlowAlwaysOn {
        fn name(&self) -> &str {
            "slow"
        }
        fn schema(&self) -> Value {
            json!({"type":"function","function":{"name":"slow","description":"","parameters":{"type":"object","properties":{}}}})
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            tokio::time::sleep(Duration::from_secs(60)).await;
            ToolResult::ok("never")
        }
        fn execution_tier(&self) -> ToolExecutionTier {
            ToolExecutionTier::AlwaysOn
        }
    }

    #[test]
    fn partition_separates_tiers() {
        let tools: Vec<Arc<dyn ToolExecutor>> = vec![
            StaticTool::new("a", ToolExecutionTier::AlwaysOn, "x"),
            StaticTool::new("b", ToolExecutionTier::OnDemand, "y"),
            StaticTool::new("c", ToolExecutionTier::AlwaysOn, "z"),
        ];
        let (always, on_demand) = partition_by_tier(&tools);
        assert_eq!(always.len(), 2);
        assert_eq!(on_demand.len(), 1);
        assert_eq!(on_demand[0].name(), "b");
    }

    #[tokio::test]
    async fn build_live_context_empty_returns_none() {
        let tools: Vec<Arc<dyn ToolExecutor>> = vec![StaticTool::new(
            "only_on_demand",
            ToolExecutionTier::OnDemand,
            "x",
        )];
        let ctx = build_live_context(&tools).await;
        assert!(ctx.is_none(), "no always-on tools -> no context block");
    }

    #[tokio::test]
    async fn build_live_context_shapes_block() {
        let tools: Vec<Arc<dyn ToolExecutor>> = vec![
            StaticTool::new("time", ToolExecutionTier::AlwaysOn, "12:34"),
            StaticTool::new("status", ToolExecutionTier::AlwaysOn, "green"),
            StaticTool::new("ignored", ToolExecutionTier::OnDemand, "skip me"),
        ];
        let ctx = build_live_context(&tools).await.expect("block produced");
        assert!(ctx.starts_with("## Live Context\n"));
        assert!(ctx.contains("### time\n12:34"));
        assert!(ctx.contains("### status\ngreen"));
        // OnDemand tools must NOT appear.
        assert!(!ctx.contains("ignored"));
        assert!(!ctx.contains("skip me"));
    }

    #[tokio::test]
    async fn failing_tool_emits_unavailable() {
        let tools: Vec<Arc<dyn ToolExecutor>> = vec![
            Arc::new(FailingAlwaysOn),
            StaticTool::new("ok", ToolExecutionTier::AlwaysOn, "live"),
        ];
        let ctx = build_live_context(&tools).await.expect("block produced");
        assert!(ctx.contains("### failing\n[unavailable]"));
        assert!(ctx.contains("### ok\nlive"));
    }

    #[tokio::test]
    async fn all_failing_returns_none() {
        // When every always-on tool fails, the block would carry zero useful
        // info — suppress it rather than pollute the prompt.
        let tools: Vec<Arc<dyn ToolExecutor>> = vec![Arc::new(FailingAlwaysOn)];
        assert!(build_live_context(&tools).await.is_none());
    }

    #[tokio::test]
    async fn slow_tool_times_out() {
        // Tight timeout (50ms) hits the timeout branch without waiting 5s.
        let tools: Vec<Arc<dyn ToolExecutor>> = vec![
            Arc::new(SlowAlwaysOn),
            StaticTool::new("fast", ToolExecutionTier::AlwaysOn, "quick"),
        ];
        let ctx = build_live_context_with_timeout(&tools, Duration::from_millis(50))
            .await
            .expect("block produced");
        assert!(
            ctx.contains("### slow\n[unavailable]"),
            "slow tool must time out, got: {ctx}"
        );
        assert!(ctx.contains("### fast\nquick"));
    }
}
