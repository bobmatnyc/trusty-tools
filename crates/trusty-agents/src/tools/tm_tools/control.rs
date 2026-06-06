//! TM session control tools: pause, resume, and send-message.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::tm::manager::TmManager;
use crate::tools::traits::{ToolExecutor, ToolResult};

use super::helpers::required_str;

/// Pause a tmux session (sends adapter pause command).
pub struct TmPauseSessionTool {
    pub tm: Arc<Mutex<TmManager>>,
}

#[async_trait]
impl ToolExecutor for TmPauseSessionTool {
    fn name(&self) -> &str {
        "tm_pause_session"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "tm_pause_session",
                "description": "Pause a tmux session by sending the adapter's pause command (e.g., /mpm-session-pause for claude-mpm). Marks the session as Paused.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "session_name": {
                            "type": "string",
                            "description": "The session name (or id) to pause."
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
        match mgr.pause_session(&session_name).await {
            Ok(()) => ToolResult::ok(format!("Paused session '{session_name}'.")),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

/// Resume a paused tmux session.
pub struct TmResumeSessionTool {
    pub tm: Arc<Mutex<TmManager>>,
}

#[async_trait]
impl ToolExecutor for TmResumeSessionTool {
    fn name(&self) -> &str {
        "tm_resume_session"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "tm_resume_session",
                "description": "Resume a previously-paused tmux session by sending the adapter's resume command. Marks the session as Running.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "session_name": {
                            "type": "string",
                            "description": "The session name (or id) to resume."
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
        match mgr.resume_session(&session_name).await {
            Ok(()) => ToolResult::ok(format!("Resumed session '{session_name}'.")),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

/// Send a message to a session's harness (e.g., a prompt to claude-mpm).
pub struct TmSendMessageTool {
    pub tm: Arc<Mutex<TmManager>>,
}

#[async_trait]
impl ToolExecutor for TmSendMessageTool {
    fn name(&self) -> &str {
        "tm_send_message"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "tm_send_message",
                "description": "Send a message (prompt or command) to the AI harness running in a tmux session. The message is formatted by the session's adapter before being typed into the pane.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "session_name": {
                            "type": "string",
                            "description": "The session name (or id) to receive the message."
                        },
                        "message": {
                            "type": "string",
                            "description": "The message text to send."
                        }
                    },
                    "required": ["session_name", "message"],
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
        let message = match required_str(&args, "message") {
            Ok(s) => s.to_string(),
            Err(e) => return e,
        };
        let mgr = self.tm.lock().await;
        match mgr.send_message(&session_name, &message).await {
            Ok(()) => ToolResult::ok(format!(
                "Sent {} chars to session '{session_name}'.",
                message.len()
            )),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}
