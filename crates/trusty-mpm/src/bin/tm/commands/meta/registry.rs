//! Tool registry assembly for the standalone metaharness (#1045, WI-2).
//!
//! Why: the M1 metaharness drives PM → sub-agent delegation in-process via
//! trusty-code (Seam A). Before the orchestrator (WI-4) and live LLM inference
//! can run, `meta run` needs a populated [`ToolRegistry`] so the (future) PM
//! loop has the fs/bash/delegate capabilities it will offer the model. WI-2
//! stands up that registry — the wiring is real, but the delegation runner is a
//! stub: actual sub-agent execution arrives in WI-4. Keeping the assembly in its
//! own submodule keeps `meta/mod.rs` focused on the CLI verb and stays well
//! under the SLOC cap.
//! What: [`NoopAgentRunner`] is a placeholder [`AgentRunner`] whose `run`
//! returns a fixed stub message (no real delegation yet);
//! [`build_meta_registry`] constructs a [`ToolRegistry`] populated with the
//! trusty-code fs tools (`read_file`, `write_file`, `edit`), the `bash` tool,
//! and the `delegate_to_agent` tool backed by the stub runner.
//! Test: `registry_contains_expected_tools` and `noop_runner_returns_stub` in
//! this module's `tests` block.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use trusty_code::tools::{
    AgentOutput, AgentRunner, BashTool, DelegateToAgentTool, EditTool, ReadFileTool, ToolRegistry,
    WriteFileTool,
};

/// Placeholder text returned by [`NoopAgentRunner::run`] until WI-4 lands.
///
/// Why: the `delegate_to_agent` tool requires a concrete [`AgentRunner`], but
/// WI-2's scope stops short of real sub-agent execution. Centralising the stub
/// string keeps the producer and the unit-test assertion in lockstep.
/// What: the literal message the stub runner echoes back as its agent output.
/// Test: `noop_runner_returns_stub` asserts the emitted value.
pub(crate) const NOOP_RUNNER_STUB: &str = "stub: WI-4 will implement real delegation";

/// Stub [`AgentRunner`] for the WI-2 metaharness registry.
///
/// Why: [`DelegateToAgentTool`] needs an injected [`AgentRunner`], yet WI-2 must
/// not perform live delegation (that is WI-4). This no-op runner lets the
/// delegate tool register and be exercised structurally without spawning agents
/// or calling an LLM.
/// What: a zero-sized type implementing [`AgentRunner`]; `run` ignores its
/// arguments and returns a fixed-content [`AgentOutput`].
/// Test: `noop_runner_returns_stub` calls `run` and asserts the stub content;
/// `registry_contains_expected_tools` proves it wires into the registry.
pub(crate) struct NoopAgentRunner;

#[async_trait]
impl AgentRunner for NoopAgentRunner {
    /// Return the fixed WI-2 stub output instead of running a sub-agent.
    ///
    /// Why: WI-2 wires the delegate tool without live delegation; a deterministic
    /// stub keeps the path exercisable and unit-testable.
    /// What: ignores `agent_name`/`task` and returns
    /// `AgentOutput::from_content(NOOP_RUNNER_STUB)`.
    /// Test: `noop_runner_returns_stub`.
    async fn run(&self, _agent_name: &str, _task: &str) -> Result<AgentOutput> {
        Ok(AgentOutput::from_content(NOOP_RUNNER_STUB))
    }
}

/// Build the metaharness [`ToolRegistry`] scoped to `project`.
///
/// Why: the metaharness PM loop (WI-4) offers the model a fixed capability set —
/// file read/write/edit, shell, and sub-agent delegation. Assembling that set in
/// one place gives later work items a single, tested entry point and lets WI-2
/// verify the wiring before any orchestration exists.
/// What: constructs a [`ToolRegistry`] and registers [`ReadFileTool`],
/// [`WriteFileTool`], and [`EditTool`] (each scoped to `project`), a default
/// [`BashTool`], and a [`DelegateToAgentTool`] backed by [`NoopAgentRunner`].
/// The fs tools are scoped to `project` so file operations stay inside the
/// run's working directory.
/// Test: `registry_contains_expected_tools` asserts every expected tool name is
/// registered.
pub(crate) fn build_meta_registry(project: &Path) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadFileTool::new(project)));
    registry.register(Arc::new(WriteFileTool::new(project)));
    registry.register(Arc::new(EditTool::new(project)));
    registry.register(Arc::new(BashTool::default_config()));
    registry.register(Arc::new(DelegateToAgentTool::new(Arc::new(
        NoopAgentRunner,
    ))));
    registry
}

/// Sorted list of tool names registered by [`build_meta_registry`].
///
/// Why: both the structured run summary (stdout JSON) and the info-level log
/// line need a stable, deterministic ordering of the registered tools; the
/// registry's underlying `HashMap` does not guarantee order, so callers must
/// sort. Centralising the projection keeps the summary and the log consistent.
/// What: returns the registered schema function names sorted lexicographically.
/// Test: `registry_tool_names_are_sorted_and_complete`.
pub(crate) fn registry_tool_names(registry: &ToolRegistry) -> Vec<String> {
    let mut names: Vec<String> = registry
        .schemas()
        .into_iter()
        .filter_map(|s| {
            s.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `build_meta_registry` registers every expected metaharness tool.
    ///
    /// Why: the registry is the contract WI-4 builds on; a missing tool would
    /// silently strip a capability from the PM loop.
    /// What: builds the registry over a temp project dir and asserts each tool
    /// name (`bash`, `read_file`, `write_file`, `edit`, `delegate_to_agent`) is
    /// registered via `contains`.
    /// Test: this test.
    #[test]
    fn registry_contains_expected_tools() {
        let project = std::env::temp_dir();
        let registry = build_meta_registry(&project);
        assert!(registry.contains("bash"), "bash must be registered");
        assert!(
            registry.contains("read_file"),
            "read_file must be registered"
        );
        assert!(
            registry.contains("write_file"),
            "write_file must be registered"
        );
        assert!(registry.contains("edit"), "edit must be registered");
        assert!(
            registry.contains("delegate_to_agent"),
            "delegate_to_agent must be registered"
        );
    }

    /// `registry_tool_names` returns the complete tool set in sorted order.
    ///
    /// Why: the summary JSON and log line depend on a deterministic ordering;
    /// this guards both the contents and the sort.
    /// What: asserts the projected names equal the expected sorted list.
    /// Test: this test.
    #[test]
    fn registry_tool_names_are_sorted_and_complete() {
        let project = std::env::temp_dir();
        let registry = build_meta_registry(&project);
        let names = registry_tool_names(&registry);
        assert_eq!(
            names,
            vec![
                "bash".to_string(),
                "delegate_to_agent".to_string(),
                "edit".to_string(),
                "read_file".to_string(),
                "write_file".to_string(),
            ]
        );
    }

    /// `NoopAgentRunner::run` returns the fixed WI-2 stub output.
    ///
    /// Why: confirms the placeholder runner behaves deterministically and does
    /// not accidentally perform real work before WI-4.
    /// What: calls `run` with arbitrary args; asserts content equals the stub.
    /// Test: this test.
    #[tokio::test]
    async fn noop_runner_returns_stub() {
        let out = NoopAgentRunner
            .run("engineer", "write a file")
            .await
            .expect("noop runner never errors");
        assert_eq!(out.content, NOOP_RUNNER_STUB);
    }
}
