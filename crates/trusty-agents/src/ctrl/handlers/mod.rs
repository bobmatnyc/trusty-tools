//! CTRL tool handlers, registry assembly, and runtime context helpers.
//!
//! Why: Each ToolExecutor that ctrl/PM expose to the LLM gets its own focused
//! file so this slice of the codebase stays under the 500-line cap and lints
//! cleanly per-module.
//! What: One submodule per logical group (memory, projects, sessions, fs, etc.)
//! plus `registry` for the per-turn assembly and `tm_context` for the live TM
//! session block that gets injected into system prompts.
//! Test: Tool-level unit tests live alongside each handler; registry assembly
//! is exercised end-to-end by the ctrl integration tests.

pub(crate) mod docs;
pub(crate) mod fs;
pub(crate) mod memory;
pub(crate) mod projects;
pub(crate) mod registry;
pub(crate) mod self_project;
pub(crate) mod sessions;
pub(crate) mod tm_context;

pub(crate) use docs::SearchDocsTool;
pub(crate) use fs::{CreateDirTool, MoveFileTool};
pub(crate) use memory::{MemoryRecallTool, MemoryStoreTool};
pub(crate) use projects::{
    AddProjectTool, ListProjectsTool, RemoveProjectTool, SetActiveProjectTool,
};
pub(crate) use registry::{build_ctrl_registry, register_ticketing_tools};
pub(crate) use self_project::{InitiateSelfTaskTool, SelfProjectStatusTool};
pub(crate) use sessions::{
    PmStatusRow, PmStopHandle, SearchSessionsTool, StartPmTool, StopTaskTool, TaskStatusTool,
};
pub(crate) use tm_context::build_tm_context_block;
