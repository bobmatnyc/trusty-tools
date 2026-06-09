//! Tool system for tcode — traits, registry, and the delegate tool.
//!
//! Why: The PM loop and sub-agent loops need a polymorphic tool dispatch layer
//! so new capabilities (web search, file ops, memory, etc.) plug in without
//! touching the core orchestration code. This module is the assembly point.
//! What: Re-exports `ToolExecutor`, `AgentRunner`, `RunContext`, `AgentOutput`,
//! `SearchProvider`, `SearchResult`, `SkillResolver`, and `ToolResult` from
//! `traits`; `ToolRegistry` from `registry`; and `DelegateToAgentTool` from
//! `delegate`.
//! Test: Unit tests live in each submodule; integration tests use
//! `DelegateToAgentTool` with a `MockAgentRunner`.

pub mod bash;
pub mod delegate;
pub mod fs;
pub mod registry;
pub mod traits;

// Flat re-exports for `crate::tools::*` convenience.
#[allow(unused_imports)]
pub use bash::BashTool;
#[allow(unused_imports)]
pub use delegate::DelegateToAgentTool;
pub use fs::{EditTool, ReadFileTool, WriteFileTool};
pub use registry::ToolRegistry;
pub use traits::{
    AgentOutput, AgentRunner, HistoryMessage, RunContext, SearchProvider, SearchResult,
    ServiceTier, SkillResolver, ToolExecutor, ToolResult,
};

// ── Registry integration tests for the FS tools ──────────────────────────────

#[cfg(test)]
mod fs_registry_tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::{EditTool, ReadFileTool, ToolRegistry, WriteFileTool};

    /// All three FS tools register and appear in `schemas()`.
    ///
    /// Why: Verifies the complete tool plug-in lifecycle — register, schema emit,
    /// dispatch — for the fs tool set as a group.
    /// What: Registers `read_file`, `write_file`, and `edit`; asserts all three
    /// schema names and that `dispatch_gated` routes to each.
    /// Test: This test.
    #[tokio::test]
    async fn fs_tools_register_and_appear_in_schemas() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::new(tmp.path())));
        reg.register(Arc::new(WriteFileTool::new(tmp.path())));
        reg.register(Arc::new(EditTool::new(tmp.path())));

        assert!(reg.contains("read_file"), "read_file must be registered");
        assert!(reg.contains("write_file"), "write_file must be registered");
        assert!(reg.contains("edit"), "edit must be registered");

        let schemas = reg.schemas();
        assert_eq!(schemas.len(), 3, "exactly three tools");

        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"edit"));
    }

    /// `dispatch_gated` routes `write_file` correctly.
    ///
    /// Why: Verifies the dispatch path end-to-end for an fs tool.
    /// What: Register `write_file`, dispatch with valid args, assert file written.
    /// Test: This test.
    #[tokio::test]
    async fn dispatch_gated_routes_write_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(WriteFileTool::new(tmp.path())));

        let result = reg
            .dispatch_gated(
                "write_file",
                json!({"path": "via_registry.txt", "content": "registry dispatch test"}),
                None,
            )
            .await;

        assert!(
            !result.is_error(),
            "dispatch should succeed: {}",
            result.content()
        );
        let content = std::fs::read_to_string(tmp.path().join("via_registry.txt")).expect("read");
        assert_eq!(content, "registry dispatch test");
    }

    /// `dispatch_gated` routes `read_file` and returns content.
    ///
    /// Why: Verifies the read dispatch path end-to-end.
    /// What: Write a file, register `read_file`, dispatch, assert content in result.
    /// Test: This test.
    #[tokio::test]
    async fn dispatch_gated_routes_read_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("test.txt"), "hello from registry").expect("seed");

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::new(tmp.path())));

        let result = reg
            .dispatch_gated("read_file", json!({"path": "test.txt"}), None)
            .await;

        assert!(
            !result.is_error(),
            "dispatch should succeed: {}",
            result.content()
        );
        assert!(result.content().contains("hello from registry"));
    }

    /// `dispatch_gated` routes `edit` and the file is modified.
    ///
    /// Why: Verifies the edit dispatch path end-to-end.
    /// What: Write `old`, register `edit`, dispatch replace, assert `new` on disk.
    /// Test: This test.
    #[tokio::test]
    async fn dispatch_gated_routes_edit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("target.py"), "x = 0\n").expect("seed");

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EditTool::new(tmp.path())));

        let result = reg
            .dispatch_gated(
                "edit",
                json!({"path": "target.py", "old_string": "x = 0", "new_string": "x = 99"}),
                None,
            )
            .await;

        assert!(
            !result.is_error(),
            "dispatch should succeed: {}",
            result.content()
        );
        let content = std::fs::read_to_string(tmp.path().join("target.py")).expect("read");
        assert!(content.contains("x = 99"));
    }
}
