//! Tool registry and definitions for LLM function calling.
//!
//! Why: Replaces hardcoded `if tc.name == "delegate_to_agent"` dispatch with
//! a polymorphic registry so new tools (`web_search`, `load_skill`, etc.) can
//! be plugged in without touching the PM loop. This is the SOA seam.
//! What: `ToolRegistry` holds `Arc<dyn ToolExecutor>` keyed by name; emits a
//! vector of OpenAI-compatible JSON schemas for the LLM request; dispatches
//! named tool calls to the right executor.
//! Test: Unit tests construct a registry with a fake tool and assert that
//! `dispatch("fake", args)` returns the fake's output and `schemas()`
//! contains the fake's schema.

pub mod agent_plugin;
pub mod always_on;
pub mod analysis;
pub mod ast_tools;
pub mod delegate;
pub mod file_filter;
pub mod finish_task;
pub mod format_translator;
pub mod fs_reader;
pub mod git_tools;
pub mod mcp_service_tools;
pub mod mcp_tools;
pub mod memory;
pub mod memory_search;
pub mod native_memory;
pub mod native_search;
pub mod native_ticketing;
pub mod phase_audit;
pub mod registry;
pub mod run_bash;
pub mod shell;
pub mod shell_exec;
pub mod skill_loader;
pub mod timer;
pub mod tm_tools;
pub mod traits;
pub mod web_search;
pub mod write_file;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_openai::types::{ChatCompletionTool, ChatCompletionToolArgs, FunctionObjectArgs};
use serde_json::Value;

#[allow(unused_imports)]
pub use traits::{
    AgentOutput, AgentRunner, RunContext, SearchProvider, SearchResult, SkillResolver,
    ToolExecutionTier, ToolExecutor, ToolResult,
};

/// Registry of tools available to an LLM session.
///
/// Why: Centralizes tool lookup and schema emission so the PM loop and the
/// multi-turn sub-agent loop stay short and test-friendly.
/// What: `HashMap<String, Arc<dyn ToolExecutor>>` with register/dispatch/schemas.
/// Test: See unit tests below.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn ToolExecutor>>,
}

impl ToolRegistry {
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register the three read-only filesystem exploration tools
    /// (`read_file`, `list_dir`, `grep_files`).
    ///
    /// Why: #34 — these tools ship together because they form a coherent
    /// "read-only explorer" capability. Any agent that wants a subset should
    /// rely on its own `[tools].allowed` allowlist.
    /// What: Adds one `Arc<ReadFileTool>` / `ListDirTool` / `GrepFilesTool`
    /// to the registry. Returns `&mut Self` for chaining.
    /// Test: `registers_fs_reader_tools` below.
    #[allow(dead_code)]
    pub fn with_fs_reader_tools(&mut self) -> &mut Self {
        use crate::tools::fs_reader::{GrepFilesTool, ListDirTool, ReadFileTool};
        self.register(Arc::new(ReadFileTool::new()));
        self.register(Arc::new(ListDirTool::new()));
        self.register(Arc::new(GrepFilesTool::new()));
        self
    }

    /// Register a tool by its `name()`.
    ///
    /// Why: Using the tool's own name prevents mismatches between registered
    /// key and schema-declared function name.
    /// What: Inserts into the map. #101 (MIN-5): a `debug_assert!` now
    /// panics in debug builds on duplicate names so collisions surface
    /// during development rather than silently overwriting the first tool.
    /// In release builds the later registration still wins (existing
    /// behavior preserved) to avoid aborting production binaries.
    /// Test: `register` then `dispatch` by the same name and assert success;
    /// see also `register_panics_on_duplicate_name_in_debug`.
    pub fn register(&mut self, tool: Arc<dyn ToolExecutor>) {
        let name = tool.name().to_string();
        debug_assert!(
            !self.tools.contains_key(&name),
            "duplicate tool registration for '{name}' — collisions silently overwrite and cause dispatch bugs (see #101)"
        );
        self.tools.insert(name, tool);
    }

    /// Check whether a tool is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Dispatch a named tool call.
    ///
    /// Why: Single place where missing-tool errors are surfaced as a structured
    /// `ToolResult::Error` so the LLM loop can continue instead of aborting.
    /// What: Looks up `name`, calls `execute(args)`, returns the `ToolResult`.
    /// Test: Dispatch to an unregistered name returns a `ToolResult::Error`;
    /// dispatch to a registered one returns `ToolResult::Success`.
    pub async fn dispatch(&self, name: &str, args: Value) -> ToolResult {
        let Some(tool) = self.tools.get(name) else {
            return ToolResult::err(format!("no tool registered with name '{name}'"));
        };
        tool.execute(args).await
    }

    /// Dispatch with an optional per-agent allowlist (see `ToolsConfig`).
    ///
    /// Why: Per-agent tool gating prevents, e.g., the plan-agent from shelling
    /// out or the research-agent from delegating. Allowlist is checked
    /// centrally so every call site inherits the same policy.
    /// What: If `allowed` is `Some` and does not contain `name`, returns a
    /// recoverable `ToolResult::Error` naming what is permitted. Otherwise
    /// delegates to `dispatch`.
    /// Test: See `ToolRegistry::dispatch_gated_rejects_disallowed` below.
    pub async fn dispatch_gated(
        &self,
        name: &str,
        args: Value,
        allowed: Option<&[String]>,
    ) -> ToolResult {
        if let Some(list) = allowed
            && !list.iter().any(|a| a == name)
        {
            return ToolResult::err(format!(
                "Tool '{name}' is not permitted for this agent. Allowed: {}",
                list.join(", ")
            ));
        }
        self.dispatch(name, args).await
    }

    /// Emit all registered tool schemas as raw JSON values.
    ///
    /// Why: The sub-agent tool-calling loop needs raw JSON schemas to attach
    /// to requests; the OpenAI-compatible schema is produced by each tool.
    /// What: Collects `tool.schema()` for every registered tool.
    /// Test: Registering two tools returns a vector of length 2.
    pub fn schemas(&self) -> Vec<Value> {
        self.tools.values().map(|t| t.schema()).collect()
    }

    /// Dispatch a tool call honoring a `UserIdentity`'s `ServiceTier` (#445).
    ///
    /// Why: Some tools must be denied to less-trusted transports (Slack guest
    /// users, unauthenticated HTTP) even if the LLM tries to call them.
    /// Enforcing the tier check at the dispatch boundary means transport
    /// authors can't accidentally bypass RBAC by forgetting to pre-filter
    /// the tool list before passing it to the LLM.
    /// What: If the tool is registered AND `user.can_access_tier(tool.restricted_tiers())`
    /// is `false`, returns a `ToolResult::Error` with a stable, user-facing
    /// message ("This tool is not available for your access tier."). Otherwise
    /// delegates to `dispatch_gated` so the per-agent allowlist still applies.
    /// Test: `dispatch_for_user_*` below.
    pub async fn dispatch_for_user(
        &self,
        name: &str,
        args: Value,
        allowed: Option<&[String]>,
        user: &crate::rbac::UserIdentity,
    ) -> ToolResult {
        if let Some(tool) = self.tools.get(name)
            && !user.can_access_tier(tool.restricted_tiers())
        {
            return ToolResult::err("This tool is not available for your access tier.");
        }
        self.dispatch_gated(name, args, allowed).await
    }

    /// Return the subset of registered tools that `user` may invoke (#445).
    ///
    /// Why: Schema emission for the LLM request must reflect what the user
    /// can actually call, otherwise the model will hallucinate denied calls
    /// and burn turns. Filtering here keeps the LLM's tool list and dispatch
    /// enforcement in sync.
    /// What: Returns cloned `Arc`s for tools where
    /// `user.can_access_tier(tool.restricted_tiers())` is true. Order is
    /// unspecified (HashMap iteration).
    /// Test: `filter_tools_for_user_*` below.
    pub fn filter_tools_for_user(
        &self,
        user: &crate::rbac::UserIdentity,
    ) -> Vec<Arc<dyn ToolExecutor>> {
        self.tools
            .values()
            .filter(|t| user.can_access_tier(t.restricted_tiers()))
            .cloned()
            .collect()
    }

    /// Convert registered tool schemas into `ChatCompletionTool` values
    /// suitable for the async-openai builder.
    ///
    /// Why: The PM loop uses async-openai's typed builder; this bridges the
    /// raw JSON schemas back into that representation without duplicating
    /// them at call sites.
    /// What: Re-parses the `function` object into `FunctionObjectArgs` and
    /// wraps with `ChatCompletionToolArgs`.
    /// Test: Registering `delegate_to_agent` and calling this yields one
    /// `ChatCompletionTool` with the expected name.
    pub fn openai_tools(&self) -> Result<Vec<ChatCompletionTool>> {
        let mut out = Vec::with_capacity(self.tools.len());
        for schema in self.schemas() {
            let function = schema
                .get("function")
                .cloned()
                .context("tool schema missing 'function' object")?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .context("tool schema missing function.name")?
                .to_string();
            let description = function
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let parameters = function
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type":"object","properties":{}}));

            let func = FunctionObjectArgs::default()
                .name(name)
                .description(description)
                .parameters(parameters)
                .build()
                .context("failed to build FunctionObject from schema")?;
            let tool = ChatCompletionToolArgs::default()
                .function(func)
                .build()
                .context("failed to build ChatCompletionTool from schema")?;
            out.push(tool);
        }
        Ok(out)
    }
}

/// Optional backends for the native search + memory tools (#137).
///
/// Why: `native_tool_registry` needs to know which of search_code /
/// search_memory / store_memory / retrieve_memory / list_memory_keys /
/// search_skills should be given a real backend vs. left in graceful-
/// degradation mode. Passing them in a single struct keeps the call site
/// readable as the backend list grows.
/// What: Each field is an `Option<Arc<_>>`; `None` leaves the corresponding
/// tool in "unavailable" mode so callers that don't wire it don't crash.
/// Test: `native_tool_registry_*` cases exercise both wired and default
/// paths.
#[derive(Default, Clone)]
#[allow(dead_code)]
pub struct NativeToolBackends {
    pub code_indexer: Option<Arc<crate::search::indexer::CodeIndexer>>,
    pub memory_graph: Option<Arc<crate::memory::graph::MemoryGraph>>,
    pub skill_resolver: Option<Arc<dyn SkillResolver>>,
    pub memory_backend: Option<crate::tools::native_memory::MemoryBackend>,
}

/// Build a list of native (non-shell) tools (#133, #137).
///
/// Why: Agents upgrading from `shell_exec`-backed workflows need a single
/// call to get the new typed search/memory tools registered. Ticketing is
/// opt-in because it requires a configured `TicketingClient`; search/memory
/// backends are opt-in because callers may not have them initialized yet.
/// What: Returns native search + memory tools always. When a backend is
/// provided in `backends`, the corresponding tool is wired through it;
/// otherwise the tool is registered in graceful-degradation mode. When
/// `ticketing` is `Some(client)`, also returns the five ticketing tools.
/// Test: See `native_tool_registry_returns_expected_names` in unit tests.
#[allow(dead_code)]
pub fn native_tool_registry(
    ticketing: Option<Arc<dyn crate::ticketing::TicketingClient>>,
    backends: NativeToolBackends,
) -> Vec<Arc<dyn ToolExecutor>> {
    use crate::tools::native_memory::{ListMemoryKeysTool, RetrieveMemoryTool, StoreMemoryTool};
    use crate::tools::native_search::{SearchCodeTool, SearchMemoryTool, SearchSkillsTool};
    use crate::tools::native_ticketing::{
        AddCommentTool, CloseTicketTool, CreateTicketTool, GetTicketTool, ListTicketsTool,
    };

    let search_code: Arc<dyn ToolExecutor> = match backends.code_indexer {
        Some(idx) => Arc::new(SearchCodeTool::with_indexer(idx)),
        None => Arc::new(SearchCodeTool::new()),
    };
    let search_memory: Arc<dyn ToolExecutor> = match backends.memory_graph {
        Some(g) => Arc::new(SearchMemoryTool::with_graph(g)),
        None => Arc::new(SearchMemoryTool::new()),
    };
    let search_skills: Arc<dyn ToolExecutor> = match backends.skill_resolver {
        Some(r) => Arc::new(SearchSkillsTool::with_resolver(r)),
        None => Arc::new(SearchSkillsTool::new()),
    };
    let (store_mem, retrieve_mem, list_mem): (
        Arc<dyn ToolExecutor>,
        Arc<dyn ToolExecutor>,
        Arc<dyn ToolExecutor>,
    ) = match backends.memory_backend {
        Some(backend) => (
            Arc::new(StoreMemoryTool::with_backend(backend.clone())),
            Arc::new(RetrieveMemoryTool::with_backend(backend.clone())),
            Arc::new(ListMemoryKeysTool::with_backend(backend)),
        ),
        None => (
            Arc::new(StoreMemoryTool::new()),
            Arc::new(RetrieveMemoryTool::new()),
            Arc::new(ListMemoryKeysTool::new()),
        ),
    };

    let mut out: Vec<Arc<dyn ToolExecutor>> = vec![
        search_code,
        search_memory,
        search_skills,
        store_mem,
        retrieve_mem,
        list_mem,
    ];
    if let Some(client) = ticketing {
        out.push(Arc::new(CreateTicketTool(client.clone())));
        out.push(Arc::new(GetTicketTool(client.clone())));
        out.push(Arc::new(CloseTicketTool(client.clone())));
        out.push(Arc::new(ListTicketsTool(client.clone())));
        out.push(Arc::new(AddCommentTool(client)));
    }
    out
}

/// Legacy helper for the `delegate_to_agent` tool schema.
///
/// Why: Kept for backward-compatibility with the existing PM loop before it
/// migrates to the registry fully; also used by the unit test for schema
/// correctness.
/// What: Builds a `ChatCompletionTool` with the hardcoded delegate schema.
/// Test: Unit test below.
#[allow(dead_code)]
pub fn delegate_to_agent_tool() -> Result<ChatCompletionTool> {
    let params = serde_json::json!({
        "type": "object",
        "properties": {
            "agent_name": {
                "type": "string",
                "description": "The short name of the sub-agent to delegate to (e.g. 'python-engineer')."
            },
            "task": {
                "type": "string",
                "description": "The concrete task description to send to the sub-agent."
            }
        },
        "required": ["agent_name", "task"],
        "additionalProperties": false
    });

    let function = FunctionObjectArgs::default()
        .name("delegate_to_agent")
        .description("Delegate a task to a specialized sub-agent. Use this for any implementation work (writing code, running analysis, etc.). The sub-agent will be spawned as a subprocess and its result returned to you.")
        .parameters(params)
        .build()
        .context("failed to build delegate_to_agent function object")?;

    let tool = ChatCompletionToolArgs::default()
        .function(function)
        .build()
        .context("failed to build delegate_to_agent ChatCompletionTool")?;

    Ok(tool)
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
