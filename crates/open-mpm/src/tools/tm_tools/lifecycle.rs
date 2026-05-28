//! TM session lifecycle tools: create and kill sessions.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::tm::manager::TmManager;
use crate::tm::project::AdapterType;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::helpers::required_str;

/// Create a new tmux session for a project.
pub struct TmNewSessionTool {
    pub tm: Arc<Mutex<TmManager>>,
}

#[async_trait]
impl ToolExecutor for TmNewSessionTool {
    fn name(&self) -> &str {
        "tm_new_session"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "tm_new_session",
                "description": "Create a new tmux session for a project directory. Optionally specify which AI harness adapter to use.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "session_name": {
                            "type": "string",
                            "description": "Friendly session name (will be uniquified if it collides with existing tmux sessions)."
                        },
                        "project_path": {
                            "type": "string",
                            "description": "Absolute path to the project root directory."
                        },
                        "adapter": {
                            "type": "string",
                            "description": "Optional adapter type: claude-mpm, claude-code, codex, augment, gemini, shell. Defaults to shell."
                        }
                    },
                    "required": ["session_name", "project_path"],
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
        let project_path = match required_str(&args, "project_path") {
            Ok(s) => PathBuf::from(s),
            Err(e) => return e,
        };
        let adapter = args
            .get("adapter")
            .and_then(Value::as_str)
            .map(AdapterType::from_id);

        let mgr = self.tm.lock().await;
        match mgr.new_session(&session_name, &project_path, adapter).await {
            Ok(session) => ToolResult::ok(
                serde_json::to_string_pretty(&json!({
                    "name": session.name,
                    "tmux_session_name": session.tmux_session_name,
                    "adapter": session.adapter_type.as_str(),
                    "status": session.status.to_string(),
                    "project_path": session.project_path.to_string_lossy(),
                    "id": session.id,
                }))
                .unwrap_or_default(),
            ),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

/// Kill a tmux session.
pub struct TmKillSessionTool {
    pub tm: Arc<Mutex<TmManager>>,
}

#[async_trait]
impl ToolExecutor for TmKillSessionTool {
    fn name(&self) -> &str {
        "tm_kill_session"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "tm_kill_session",
                "description": "Kill a tmux session and mark it Stopped. This is destructive — confirm with the user before invoking unless the user explicitly asked.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "session_name": {
                            "type": "string",
                            "description": "The session name (or id) to kill."
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
        let mgr = self.tm.lock().await;
        match mgr.kill_session(&session_name).await {
            Ok(()) => ToolResult::ok(format!("Killed session '{session_name}'.")),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}
