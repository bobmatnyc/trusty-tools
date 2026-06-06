//! `finish_task` — terminal tool that ends the agent's tool-calling loop.
//!
//! Why: Issue #57 — pairing this tool with `tool_choice=any` lets an agent
//! signal task completion explicitly instead of relying on the harness to
//! detect "agent stopped emitting tool calls". That makes the control flow
//! deterministic: one tool call = one loop iteration; when the agent wants
//! to stop it calls `finish_task(summary=...)` and the loop exits with the
//! summary as the agent's final text output.
//! What: A `ToolExecutor` implementation whose `execute()` returns the
//! supplied `summary` string as a success result. The actual loop exit
//! happens inside `chat_with_tools_gated`, which detects a tool call named
//! `finish_task` BEFORE dispatching it and returns early. The `execute()`
//! impl exists for completeness (and to keep registry invariants uniform:
//! every registered tool is dispatchable).
//! Test: `finish_task_name_is_stable`, `finish_task_execute_returns_summary`,
//! `finish_task_schema_requires_summary`.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

/// The string constant matched by the loop to short-circuit.
pub const FINISH_TASK_TOOL_NAME: &str = "finish_task";

/// Terminal tool — when the model calls this, the chat loop exits
/// immediately with the provided `summary` as the agent's result.
pub struct FinishTaskTool;

impl FinishTaskTool {
    /// Construct a fresh instance. Stateless.
    pub fn new() -> Self {
        Self
    }
}

impl Default for FinishTaskTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolExecutor for FinishTaskTool {
    fn name(&self) -> &str {
        FINISH_TASK_TOOL_NAME
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": FINISH_TASK_TOOL_NAME,
                "description": "Call this tool when your task is fully complete. \
                                Provide a plain-text summary of what you accomplished. \
                                This immediately ends the task loop.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "summary": {
                            "type": "string",
                            "description": "Plain-text summary of completed work."
                        }
                    },
                    "required": ["summary"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("Task complete.");
        ToolResult::ok(summary.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_task_name_is_stable() {
        assert_eq!(FinishTaskTool.name(), "finish_task");
        assert_eq!(FINISH_TASK_TOOL_NAME, "finish_task");
    }

    #[test]
    fn finish_task_schema_requires_summary() {
        let s = FinishTaskTool.schema();
        assert_eq!(s["function"]["name"], "finish_task");
        let required = s["function"]["parameters"]["required"]
            .as_array()
            .expect("required is array");
        assert!(required.iter().any(|v| v.as_str() == Some("summary")));
    }

    #[tokio::test]
    async fn finish_task_execute_returns_summary() {
        let tool = FinishTaskTool::new();
        let out = tool
            .execute(json!({"summary": "All done — 3 tests green."}))
            .await;
        assert!(!out.is_error());
        assert_eq!(out.content(), "All done — 3 tests green.");
    }

    #[tokio::test]
    async fn finish_task_execute_defaults_when_summary_missing() {
        let tool = FinishTaskTool::new();
        let out = tool.execute(json!({})).await;
        assert!(!out.is_error());
        assert_eq!(out.content(), "Task complete.");
    }
}
