//! Read-only TM tools: list sessions, list projects, capture pane, reconcile.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::tm::manager::TmManager;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::helpers::required_str;

/// List all TM-managed tmux sessions with project/adapter/status info.
pub struct TmListSessionsTool {
    pub tm: Arc<Mutex<TmManager>>,
}

#[async_trait]
impl ToolExecutor for TmListSessionsTool {
    fn name(&self) -> &str {
        "tm_list_sessions"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "tm_list_sessions",
                "description": "List all tmux sessions managed by TM, with adapter type, status, project path, and last-active time. Use this to answer questions like 'what sessions are running?' or 'what is X working on?'.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        let mgr = self.tm.lock().await;
        match mgr.list_sessions().await {
            Ok(sessions) => {
                let rows: Vec<Value> = sessions
                    .iter()
                    .map(|s| {
                        json!({
                            "name": s.name,
                            "tmux_session_name": s.tmux_session_name,
                            "adapter": s.adapter_type.as_str(),
                            "status": s.status.to_string(),
                            "project_path": s.project_path.to_string_lossy(),
                            "project_name": s.project_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("?"),
                            "last_active": s.last_active_ago(),
                        })
                    })
                    .collect();
                ToolResult::ok(serde_json::to_string_pretty(&rows).unwrap_or_default())
            }
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

/// List all TM projects with framework and session count.
pub struct TmListProjectsTool {
    pub tm: Arc<Mutex<TmManager>>,
}

#[async_trait]
impl ToolExecutor for TmListProjectsTool {
    fn name(&self) -> &str {
        "tm_list_projects"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "tm_list_projects",
                "description": "List all TM projects with detected language/framework and number of associated tmux sessions.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        let mgr = self.tm.lock().await;
        match mgr.list_projects().await {
            Ok(projects) => {
                let rows: Vec<Value> = projects
                    .iter()
                    .map(|p| {
                        json!({
                            "name": p.name,
                            "path": p.path.to_string_lossy(),
                            "framework": p.framework.display(),
                            "session_count": p.session_ids.len(),
                        })
                    })
                    .collect();
                ToolResult::ok(serde_json::to_string_pretty(&rows).unwrap_or_default())
            }
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

/// Capture pane output from a session.
pub struct TmCapturePaneTool {
    pub tm: Arc<Mutex<TmManager>>,
}

#[async_trait]
impl ToolExecutor for TmCapturePaneTool {
    fn name(&self) -> &str {
        "tm_capture_pane"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "tm_capture_pane",
                "description": "Capture the most recent lines of pane output from a tmux session. Use this to check what a session is currently doing or showing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "session_name": {
                            "type": "string",
                            "description": "The session name (or id) whose pane to capture."
                        },
                        "lines": {
                            "type": "number",
                            "description": "Number of trailing lines to capture (default 50)."
                        }
                    },
                    "required": ["session_name"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let session_name = match required_str(&args, "session_name") {
            Ok(s) => s.to_string(),
            Err(e) => return e,
        };
        let lines = args
            .get("lines")
            .and_then(Value::as_u64)
            .map(|n| n.min(u32::MAX as u64) as u32)
            .unwrap_or(50);
        let mgr = self.tm.lock().await;
        match mgr.capture_pane(&session_name, lines).await {
            Ok(output) => ToolResult::ok(output),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

/// Reconcile registry with live tmux sessions (discovers new, marks orphaned).
pub struct TmReconcileTool {
    pub tm: Arc<Mutex<TmManager>>,
}

#[async_trait]
impl ToolExecutor for TmReconcileTool {
    fn name(&self) -> &str {
        "tm_reconcile"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "tm_reconcile",
                "description": "Reconcile TM's registry with live tmux. Discovers new tmux sessions not yet tracked, and marks vanished registry sessions as Orphaned. Returns counts of added and orphaned sessions.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        let mgr = self.tm.lock().await;
        match mgr.reconcile().await {
            Ok(report) => {
                let added: Vec<&str> = report.added.iter().map(|s| s.name.as_str()).collect();
                let payload = json!({
                    "added": added,
                    "orphaned": report.orphaned,
                    "added_count": report.added.len(),
                    "orphaned_count": report.orphaned.len(),
                });
                ToolResult::ok(serde_json::to_string_pretty(&payload).unwrap_or_default())
            }
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}
