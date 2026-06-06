//! Per-turn CTRL tool-registry assembly and the wiring helpers shared with the
//! PM dispatch path.
//!
//! Why: `ctrl_chat_turn` and `run_pm_task_with_history` both rely on the same
//! set of native tools (memory, projects, sessions, fs, web search, MCP, git,
//! ticketing, tmux). Centralising the build + wire helpers means each call site
//! orders dependencies the same way and any future tool registration only
//! happens in one place.
//! What: `build_ctrl_registry` is the top-level recipe used by ctrl turns;
//! `register_git_tools` and `register_ticketing_tools` are the smaller wiring
//! helpers shared with the PM path.
//! Test: Indirect — covered by the ctrl integration tests and the per-tool unit
//! tests in their respective handler modules.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::tools::ToolRegistry;

use super::super::handlers::{
    AddProjectTool, CreateDirTool, InitiateSelfTaskTool, ListProjectsTool, MemoryRecallTool,
    MemoryStoreTool, MoveFileTool, PmStatusRow, PmStopHandle, RemoveProjectTool, SearchDocsTool,
    SearchSessionsTool, SelfProjectStatusTool, SetActiveProjectTool, StartPmTool, StopTaskTool,
    TaskStatusTool,
};

/// Build the CTRL tool registry for a single LLM turn.
// Why: Each Arc<Mutex<…>> here is wired into a distinct CTRL tool; collapsing
// them into a single context struct would tightly couple unrelated tools and
// fight the per-tool ownership story. Allow locally.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn build_ctrl_registry(
    memory: Arc<Mutex<Vec<String>>>,
    pending_connect: Arc<Mutex<Option<String>>>,
    self_path: Option<PathBuf>,
    pending_self_task: Arc<Mutex<Option<String>>>,
    task_status_snapshot: Vec<PmStatusRow>,
    docs_index: Arc<Mutex<Option<Arc<crate::docs_index::DocsIndex>>>>,
    active_project: Arc<Mutex<Option<PathBuf>>>,
    pending_stop: Arc<Mutex<Option<String>>>,
    stop_snapshot: Vec<PmStopHandle>,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    // Capture self_path before it's moved into the InitiateSelfTaskTool below
    // so TM tools can use it as the canonical state_dir source.
    let self_path_for_tm = self_path.clone();
    registry.register(Arc::new(StartPmTool {
        pending: pending_connect.clone(),
        active_project: active_project.clone(),
    }));
    registry.register(Arc::new(SearchSessionsTool));
    registry.register(Arc::new(ListProjectsTool));
    registry.register(Arc::new(MemoryStoreTool {
        memory: memory.clone(),
    }));
    registry.register(Arc::new(MemoryRecallTool { memory }));
    // #185: Taskmaster needs to inspect PM task state.
    registry.register(Arc::new(TaskStatusTool {
        snapshot: task_status_snapshot,
    }));
    // #182: self-project tools, present (and reporting "no self-project")
    // even when detection failed so the LLM gets a clear error rather than
    // an "unknown tool" surprise.
    registry.register(Arc::new(SelfProjectStatusTool {
        self_path: self_path.clone(),
    }));
    registry.register(Arc::new(InitiateSelfTaskTool {
        self_path,
        pending_connect,
        pending_self_task,
    }));
    // #187: docs search tool — backed by the lazily-built TF-IDF index.
    registry.register(Arc::new(SearchDocsTool { index: docs_index }));
    // #202: project-management + active-project tools.
    registry.register(Arc::new(AddProjectTool));
    registry.register(Arc::new(RemoveProjectTool));
    registry.register(Arc::new(StopTaskTool {
        snapshot: stop_snapshot,
        pending_stop,
    }));
    registry.register(Arc::new(SetActiveProjectTool {
        active_project: active_project.clone(),
    }));
    // CTRL digital-twin: file system manipulation tools.
    registry.register(Arc::new(MoveFileTool));
    registry.register(Arc::new(CreateDirTool));
    // CTRL digital-twin: research tools (web search + project code search).
    registry.register(Arc::new(
        crate::tools::web_search::BraveSearchTool::from_env(),
    ));
    // #374: Same auto-detection as the conversational fast-path registry
    // above. Prefers the running search daemon, falls back to grep.
    {
        let project_root =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let search_tool =
            crate::tools::native_search::SearchCodeTool::new_auto(&project_root).await;
        registry.register(Arc::new(search_tool));
    }
    // #244: Dynamic MCP service management tools (mcp_list/add/remove/enable/disable).
    for tool in crate::tools::mcp_tools::mcp_tool_executors() {
        registry.register(tool);
    }
    // #243: Native ticketing tools (create/get/update/close/list/add_comment +
    // actions_trigger/actions_status). Wired only when the global config has a
    // `[github]` identity that resolves to non-empty token + repo env vars; we
    // silently skip otherwise so unconfigured environments don't error.
    register_ticketing_tools(&mut registry).await;
    // #247: Native git tools (status/log/branches/commit/push/pull/...).
    // Gated by `[git].available_for_roles` in ~/.trusty-agents/config.toml. We
    // resolve the repo root from the active project (when set) or cwd; if
    // discovery fails (not in a repo), we silently skip — the LLM will
    // simply not see git tools.
    register_git_tools(&mut registry, "ctrl", &active_project).await;

    // TM (tmux manager) tools — let ctrl query/control all tmux sessions via
    // natural language. Resolves the state_dir from the active project (when
    // set), the detected self-project, or cwd as a final fallback. When tmux
    // is unavailable, registration silently no-ops so degraded environments
    // (CI, no-tmux dev boxes) still get a working ctrl.
    {
        let active = match active_project.lock() {
            Ok(g) => g.clone(),
            Err(_) => None,
        };
        let project_root = active
            .or(self_path_for_tm)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let state_dir = project_root.join(".trusty-agents").join("state");
        crate::tools::tm_tools::register_tm_tools_for_state_dir(&mut registry, &state_dir);
    }

    registry
}

/// Wire the 12 native git tools into a `ToolRegistry` if configured (#247).
///
/// Why: Both ctrl and PM call sites need the same wiring logic; factoring
/// it out keeps them aligned and gives a single place to evolve role-gating
/// or write-confirmation behavior. Discovery failures (not a git repo) are
/// non-fatal — the agent simply runs without git tools.
/// What: Loads `GlobalConfig`, checks `git.available_for_roles` for `role`,
/// resolves a repo root (active project or cwd), opens it via `GitRepo`,
/// and registers all 12 tools from `git_tools(root)`.
/// Test: Indirect — covered by `git_tools_count_is_12` in `git_tools.rs`
/// and by the ctrl integration tests.
pub(crate) async fn register_git_tools(
    registry: &mut ToolRegistry,
    role: &str,
    active_project: &Arc<Mutex<Option<PathBuf>>>,
) {
    let cfg = crate::mcp::config::GlobalConfig::load().await;
    if !cfg.git.available_for_roles.iter().any(|r| r == role) {
        return;
    }
    let candidate = {
        let guard = match active_project.lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::warn!(
                    "register_git_tools: active_project mutex poisoned; skipping git tool registration"
                );
                return;
            }
        };
        guard.clone()
    }
    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let repo = match crate::git::GitRepo::open(&candidate) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, role = role, "no git repo discovered; skipping git tools");
            return;
        }
    };
    for tool in crate::tools::git_tools::git_tools(repo.root.clone()) {
        registry.register(tool);
    }
}

/// Wire the ticketing tools into a `ToolRegistry` if configured (#243).
///
/// Why: Both ctrl and PM call sites need the same wiring logic, so factoring
/// it out keeps them aligned. Failure to load config or build a client is
/// non-fatal — the agent simply runs without ticketing tools.
/// What: Reads `~/.trusty-agents/config.toml`, resolves the default GitHub
/// identity, builds a `TicketingClient` plus a `GitHubActionsClient`, and
/// registers all tools from `ticketing_tools()`.
/// Test: Indirectly via `ticketing_tools_count` in `native_ticketing` tests
/// and integration tests that snapshot the registered tool set.
pub(crate) async fn register_ticketing_tools(registry: &mut ToolRegistry) {
    let cfg = crate::mcp::config::GlobalConfig::load().await;
    let Some(identity) = cfg.github_identity(None) else {
        return;
    };
    let Some(tk_cfg) = identity.to_ticketing_config() else {
        tracing::debug!(
            identity = %identity.name,
            "ticketing identity present but env vars not set; skipping ticketing tools"
        );
        return;
    };
    let client: Arc<dyn crate::ticketing::TicketingClient> = match tk_cfg.build_client().await {
        Ok(c) => Arc::from(c),
        Err(e) => {
            tracing::warn!(error = %e, "failed to build ticketing client; skipping ticketing tools");
            return;
        }
    };
    // Actions client uses the same token/repo as the issues client (or `gh`
    // CLI fallback when token is missing).
    let actions = crate::ticketing::actions::build_actions_client(
        identity.token().as_deref(),
        identity.repo().as_deref(),
    )
    .await;
    for tool in crate::tools::native_ticketing::ticketing_tools(client, actions) {
        registry.register(tool);
    }
}
