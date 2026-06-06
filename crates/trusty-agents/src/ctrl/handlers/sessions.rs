//! Session / PM-lifecycle CTRL tools.
//!
//! Why: Once CTRL is connected to one or more projects, the LLM needs to drive
//! their lifecycle — spawn new PMs (`start_pm`), report status
//! (`task_status`), stop runaway tasks (`stop_task`), and search past sessions
//! (`search_sessions`). All four tools are pure (no `&mut Ctrl`) and queue any
//! side effects via Arc-shared slots drained by `ctrl_chat_turn`.
//! What: `StartPmTool`, `SearchSessionsTool`, `TaskStatusTool`, `StopTaskTool`,
//! plus the row aliases `PmStatusRow` and `PmStopHandle`.
//! Test: `stop_task_tool_*`, `task_status_returns_known_pm_state`,
//! `start_pm_*` in `ctrl::tests`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::events::{self, Event};
use crate::session_record;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::super::state::PmMsg;

/// One row of (project_name, status, last_message) shared with `TaskStatusTool`. (#185)
pub(crate) type PmStatusRow = (String, Arc<Mutex<String>>, Arc<Mutex<String>>);

/// One stoppable PM, keyed by `name` (matching `task_status` output) and
/// `project_path` string. (#202)
pub(crate) type PmStopHandle = (
    String, // name (matches task_status `project` field)
    String, // canonical project path
    mpsc::Sender<PmMsg>,
);

/// `start_pm(project_path)` — requested project path is captured in a shared
/// Option; the REPL loop drains it after the LLM turn and actually spawns the
/// PM via `Ctrl::connect`. This indirection keeps the tool pure (no &mut Ctrl
/// references) while still achieving the user-visible effect.
///
/// (#202) When `project_path` is missing or empty, falls back to the
/// `active_project` slot populated by `SetActiveProjectTool`, so a user who
/// already called `set_active_project(...)` can say "start a PM" without
/// re-typing the path.
pub(crate) struct StartPmTool {
    pub(crate) pending: Arc<Mutex<Option<String>>>,
    /// (#202) Default project to use when the LLM omits `project_path`.
    pub(crate) active_project: Arc<Mutex<Option<PathBuf>>>,
}

#[async_trait]
impl ToolExecutor for StartPmTool {
    fn name(&self) -> &str {
        "start_pm"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "start_pm",
                "description": "Spawn a project-scoped PM for the given absolute path. If 'project_path' is omitted, falls back to the active project (set via set_active_project).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "project_path": { "type": "string", "description": "Absolute filesystem path of the project." }
                    },
                    "required": [],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        // 1. Prefer an explicit, non-empty arg.
        let arg_path = args
            .get("project_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // 2. Fall back to the active-project slot.
        let path = match arg_path {
            Some(p) => p,
            None => {
                let active = match self.active_project.lock() {
                    Ok(g) => g.clone(),
                    Err(e) => {
                        return ToolResult::err(format!(
                            "start_pm: active_project lock poisoned: {e}"
                        ));
                    }
                };
                match active {
                    Some(p) => p.display().to_string(),
                    None => {
                        return ToolResult::err(
                            "start_pm: no 'project_path' provided and no active project set (use set_active_project first)",
                        );
                    }
                }
            }
        };

        match self.pending.lock() {
            Ok(mut slot) => {
                *slot = Some(path.clone());
                ToolResult::ok(format!("queued start_pm for {path}"))
            }
            Err(e) => ToolResult::err(format!("start_pm: pending lock poisoned: {e}")),
        }
    }
}

/// `search_sessions(query)` — grep ~/.trusty-agents/sessions/runs.jsonl.
pub(crate) struct SearchSessionsTool;

#[async_trait]
impl ToolExecutor for SearchSessionsTool {
    fn name(&self) -> &str {
        "search_sessions"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "search_sessions",
                "description": "Search past workflow runs (cross-project). Empty query returns all.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let query = args.get("query").and_then(Value::as_str).unwrap_or("");
        match session_record::search(query).await {
            Ok(hits) => {
                let limited: Vec<_> = hits.into_iter().take(20).collect();
                match serde_json::to_string(&limited) {
                    Ok(s) => ToolResult::ok(s),
                    Err(e) => ToolResult::err(format!("search_sessions: serialize failed: {e}")),
                }
            }
            Err(e) => ToolResult::err(format!("search_sessions: {e:#}")),
        }
    }
}

/// `task_status()` — list all PM handles with current state. (#185)
///
/// Why: The Taskmaster persona must be able to report what's running, idle,
/// or in error to drive tasks proactively to completion. Mirrors the side-
/// effect-free pattern used by other CTRL tools by reading from a snapshot
/// captured when the registry is built per-turn.
/// What: Returns a JSON array of `{project, status, last_message}`.
/// Test: `task_status_returns_known_pm_state`.
pub(crate) struct TaskStatusTool {
    /// Snapshot of (project_name, status_arc, last_message_arc) captured
    /// when the registry is built.
    pub(crate) snapshot: Vec<PmStatusRow>,
}

#[async_trait]
impl ToolExecutor for TaskStatusTool {
    fn name(&self) -> &str {
        "task_status"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "task_status",
                "description": "List all active and recently completed PM tasks with their current status",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": [],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        let mut rows: Vec<Value> = Vec::new();
        for (project, status_arc, last_arc) in &self.snapshot {
            let status = status_arc
                .lock()
                .map(|s| s.clone())
                .unwrap_or_else(|_| "unknown".to_string());
            let last = last_arc
                .lock()
                .map(|m| m.clone())
                .unwrap_or_else(|_| String::new());
            rows.push(json!({
                "project": project,
                "status": status,
                "last_message": last,
            }));
        }
        match serde_json::to_string(&rows) {
            Ok(s) => ToolResult::ok(s),
            Err(e) => ToolResult::err(format!("task_status: serialize: {e}")),
        }
    }
}

/// `stop_task(session_id)` — request shutdown of a running PM session.
/// (#202)
///
/// Why: The Taskmaster persona must be able to abort a runaway task without
/// killing the entire CTRL process. CTRL tracks PMs by project name (which is
/// what `task_status` returns as `project`), so we accept either the project
/// name or its canonical path here and match against the snapshot.
/// What: Looks up the matching handle, queues the `name` in `pending_stop`
/// for the REPL loop to drain (since the tool can't take `&mut Ctrl`), and
/// publishes `Event::SessionCancelled` immediately so SSE subscribers update.
/// The actual `PmMsg::Shutdown` send + handle removal happens in
/// `ctrl_chat_turn` after the turn completes.
/// Test: `stop_task_tool_records_pending_stop`.
pub(crate) struct StopTaskTool {
    pub(crate) snapshot: Vec<PmStopHandle>,
    pub(crate) pending_stop: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl ToolExecutor for StopTaskTool {
    fn name(&self) -> &str {
        "stop_task"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "stop_task",
                "description": "Stop a running PM task. Pass the project name or path returned by task_status() / list_projects().",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "session_id": {
                            "type": "string",
                            "description": "Session identifier — accepts the project name or canonical path of the PM to stop."
                        }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(sid) = args.get("session_id").and_then(Value::as_str) else {
            return ToolResult::err("stop_task: missing 'session_id'");
        };
        let sid_trim = sid.trim();
        // Match against either the short name or the canonical path so the
        // LLM can pass whichever is more convenient.
        let found = self
            .snapshot
            .iter()
            .find(|(name, path, _)| name == sid_trim || path == sid_trim);
        let Some((name, _path, _tx)) = found else {
            return ToolResult::ok(format!("Task not found: {sid_trim}"));
        };

        // Publish cancellation event immediately so any SSE subscribers see
        // the stop signal before the REPL drains the queue.
        events::publish(Event::SessionCancelled {
            session_id: name.clone(),
        });

        match self.pending_stop.lock() {
            Ok(mut slot) => {
                *slot = Some(name.clone());
                ToolResult::ok(format!("Task {name} stopped"))
            }
            Err(e) => ToolResult::err(format!("stop_task: pending lock poisoned: {e}")),
        }
    }
}
