//! Core trait abstractions for tool execution, agent running, search, and skills.
//!
//! Why: DI through traits lets us swap concrete implementations (subprocess
//! runner vs. in-process mock; Brave vs. Tavily search; fs skill store vs.
//! embedded) without changing call sites. This is the SOA seam that keeps
//! the workflow engine and PM loop testable.
//! What: Defines `ToolExecutor`, `AgentRunner`, `SearchProvider`, and
//! `SkillResolver`. All are object-safe (`dyn`-able) and `Send + Sync`.
//!
//! `ToolExecutor`, `ToolResult`, and `ToolExecutionTier` live in
//! `trusty-agents-common` (moved in the original extraction). `AgentRunner`,
//! `RunContext`, and `AgentOutput` were moved to `trusty-agents-common::runner`
//! in Wave 2 (issue #867, refs #830/#832). All are re-exported here so every
//! existing `crate::tools::traits::X` import resolves unchanged.
//!
//! Test: Mock impls of each trait are constructed in unit tests for
//! `ToolRegistry`, `WorkflowEngine`, and related code.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// Why: `ToolExecutor`, `ToolResult`, and `ToolExecutionTier` were extracted
//      to `trusty-agents-common` so external agent crates (e.g. `cto-assistant`)
//      can implement them without a cargo cycle through `trusty-agents`. Re-exports
//      here preserve every existing `crate::tools::traits::ToolExecutor`
//      import in the workspace — internal call sites are unchanged.
// What: Re-exports the trait + result + tier from the shared crate.
// Test: All existing tool tests still resolve these names and pass.
pub use trusty_agents_common::{ToolExecutionTier, ToolExecutor, ToolResult};

// Why: `AgentRunner`, `RunContext`, `AgentOutput`, and `HistoryMessage` were
//      extracted to `trusty-agents-common::runner` in Wave 2 (issue #867,
//      refs #830/#832) so external crates can implement or mock the runner
//      seam without depending on the full `trusty-agents` binary crate.
//      Re-exports here preserve every existing `crate::tools::traits::AgentRunner`,
//      `RunContext`, `AgentOutput` reference inside `trusty-agents`.
// What: Explicit re-exports from the shared runner module.
// Test: All existing AgentRunner tests and workflow tests still resolve and pass.
pub use trusty_agents_common::runner::{AgentOutput, AgentRunner, RunContext};

/// A search hit returned by a `SearchProvider`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Web search backend abstraction.
///
/// Why: Swap Brave for Tavily / DuckDuckGo / a test stub by changing the
/// registered impl — no callers change.
/// What: `search(query, n)` returns up to `n` results.
/// Test: A stub `SearchProvider` returns a fixed vector in tests.
#[async_trait]
pub trait SearchProvider: Send + Sync {
    async fn search(&self, query: &str, n: usize) -> Result<Vec<SearchResult>>;
}

/// Resolves named skills to their Markdown content.
///
/// Why: Skill sources vary (project `.claude/`, user `~/.claude/`, bundled
/// `config/skills/`); callers should not care which.
/// What: `resolve(name)` returns `Some(content)` if found. `list()` returns
/// all discoverable names.
/// Test: `FsSkillResolver` unit test places a file in a tempdir and asserts
/// `resolve()` returns its contents.
pub trait SkillResolver: Send + Sync {
    fn resolve(&self, name: &str) -> Option<String>;
    fn list(&self) -> Vec<String>;
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use anyhow::Result;
    use async_trait::async_trait;

    use super::{AgentOutput, AgentRunner, RunContext};
    use crate::perf::TokenUsage;

    /// Mock runner that records the RunContext it receives in `run_with_context`.
    ///
    /// Why: We need to assert that the trait default `run_with_history` impl
    /// actually forwards `ctx` to `run_with_context` (bug #122 regression guard).
    /// What: Stores each ctx snapshot; `run` is unused (test calls only
    /// `run_with_history`).
    /// Test: `test_run_with_history_forwards_ctx` constructs one of these,
    /// calls `run_with_history` with a specific `RunContext`, and asserts the
    /// recorded snapshot matches.
    struct CtxCapture {
        /// Every RunContext received by run_with_context, in call order.
        captured: Arc<Mutex<Vec<RunContext>>>,
    }

    #[async_trait]
    impl AgentRunner for CtxCapture {
        async fn run(&self, _agent_name: &str, _task: &str) -> Result<AgentOutput> {
            Ok(AgentOutput {
                content: "run".into(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }

        async fn run_with_context(
            &self,
            _agent_name: &str,
            _task: &str,
            ctx: &RunContext,
        ) -> Result<AgentOutput> {
            self.captured.lock().unwrap().push(ctx.clone());
            Ok(AgentOutput {
                content: "run_with_context".into(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
        // Note: run_with_history is intentionally NOT overridden — we test the
        // default impl behaviour defined in the trait.
    }

    /// Verifies that the trait default `run_with_history` forwards `ctx` to
    /// `run_with_context` rather than dropping it (bug #122 regression guard).
    ///
    /// Why: Before the fix, the default called `self.run()` which ignores ctx,
    /// meaning persistent-session agents lost working_dir and model.
    /// What: Calls `run_with_history` with specific working_dir and
    /// model, then asserts `run_with_context` received those values.
    /// Test: Run with `cargo test test_run_with_history_forwards_ctx`.
    #[tokio::test]
    async fn test_run_with_history_forwards_ctx() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let runner = CtxCapture {
            captured: captured.clone(),
        };

        let ctx = RunContext {
            working_dir: Some(PathBuf::from("/tmp/test-wd")),
            model: Some("anthropic/claude-opus-4-5".to_string()),
            max_turns_override: Some(5),
            assigned_file: None,
        };

        let out = runner
            .run_with_history("test-agent", "do something", &[], &ctx)
            .await
            .expect("run_with_history should succeed");

        // The default impl must delegate to run_with_context, not run().
        assert_eq!(
            out.content, "run_with_context",
            "default run_with_history must call run_with_context, not run()"
        );

        let snaps = captured.lock().unwrap();
        assert_eq!(
            snaps.len(),
            1,
            "run_with_context should be called exactly once"
        );

        let recorded = &snaps[0];
        assert_eq!(
            recorded.working_dir,
            Some(PathBuf::from("/tmp/test-wd")),
            "working_dir must be forwarded through run_with_history"
        );
        assert_eq!(
            recorded.model,
            Some("anthropic/claude-opus-4-5".to_string()),
            "model must be forwarded through run_with_history"
        );
        assert_eq!(
            recorded.max_turns_override,
            Some(5),
            "max_turns_override must be forwarded through run_with_history"
        );
    }
}
