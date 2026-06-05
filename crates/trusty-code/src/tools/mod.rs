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

pub mod delegate;
pub mod registry;
pub mod traits;

// Flat re-exports for `crate::tools::*` convenience.
#[allow(unused_imports)]
pub use delegate::DelegateToAgentTool;
pub use registry::ToolRegistry;
pub use traits::{
    AgentOutput, AgentRunner, HistoryMessage, RunContext, SearchProvider, SearchResult,
    ServiceTier, SkillResolver, ToolExecutor, ToolResult,
};
