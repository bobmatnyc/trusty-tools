//! GitHub Actions tools (#243).
//!
//! Why: Lets the ticketing agent kick off and monitor CI/release pipelines
//! without shelling out to `gh`.
//! What: Two tools — `actions_trigger`, `actions_status`. Both implement
//! `ToolExecutor` over an `Arc<dyn ActionsClient>`.
//! Test: Schema-validity tests in `super::tests`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::ticketing::actions::ActionsClient;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Tool: `actions_trigger` (#243).
///
/// Why: Lets the ticketing agent kick off a `workflow_dispatch` event so a
/// chat turn can rerun lint/CI/release pipelines without shelling out to
/// `gh`.
/// What: Wraps `GitHubActionsClient::trigger_workflow`. Accepts a workflow
/// filename or numeric ID, optional ref (default "main"), and optional
/// inputs object.
/// Test: `actions_trigger_tool_schema_is_valid`.
pub struct ActionsTriggerTool {
    pub client: Arc<dyn ActionsClient>,
}

#[async_trait]
impl ToolExecutor for ActionsTriggerTool {
    fn name(&self) -> &str {
        "actions_trigger"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "actions_trigger",
                "description": "Trigger a GitHub Actions workflow_dispatch event for the configured repo.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "workflow": {
                            "type": "string",
                            "description": "Workflow filename (e.g. 'ci.yml') or numeric ID."
                        },
                        "ref": {
                            "type": "string",
                            "description": "Branch or tag to run on (default 'main').",
                            "default": "main"
                        },
                        "inputs": {
                            "type": "object",
                            "description": "Optional workflow_dispatch inputs.",
                            "default": {}
                        }
                    },
                    "required": ["workflow"]
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(workflow) = args.get("workflow").and_then(Value::as_str) else {
            return ToolResult::err("actions_trigger: missing 'workflow'");
        };
        let git_ref = args
            .get("ref")
            .and_then(Value::as_str)
            .unwrap_or("main")
            .to_string();
        let inputs = args.get("inputs").cloned().unwrap_or_else(|| json!({}));
        match self
            .client
            .trigger_workflow(workflow, &git_ref, inputs)
            .await
        {
            Ok(()) => ToolResult::ok(
                json!({
                    "triggered": true,
                    "workflow": workflow,
                    "ref": git_ref,
                })
                .to_string(),
            ),
            Err(e) => ToolResult::err(format!("actions_trigger failed: {e:#}")),
        }
    }
}

/// Tool: `actions_status` (#243).
///
/// Why: Pairs with `actions_trigger` — after kicking off a workflow the
/// agent often needs to summarize whether it succeeded, is still running,
/// or failed.
/// What: Wraps `GitHubActionsClient::list_runs` (default limit 5). Returns
/// the parsed runs as JSON for the LLM to summarize.
/// Test: `actions_status_tool_schema_is_valid`.
pub struct ActionsStatusTool {
    pub client: Arc<dyn ActionsClient>,
}

#[async_trait]
impl ToolExecutor for ActionsStatusTool {
    fn name(&self) -> &str {
        "actions_status"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "actions_status",
                "description": "Get recent GitHub Actions workflow run status.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "workflow": {
                            "type": "string",
                            "description": "Workflow filename or numeric ID."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Number of recent runs to return.",
                            "default": 5
                        }
                    },
                    "required": ["workflow"]
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(workflow) = args.get("workflow").and_then(Value::as_str) else {
            return ToolResult::err("actions_status: missing 'workflow'");
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as u32)
            .unwrap_or(5)
            .clamp(1, 100);
        match self.client.list_runs(workflow, limit).await {
            Ok(runs) => match serde_json::to_string(&runs) {
                Ok(s) => ToolResult::ok(s),
                Err(e) => ToolResult::err(format!("actions_status: serialize failed: {e}")),
            },
            Err(e) => ToolResult::err(format!("actions_status failed: {e:#}")),
        }
    }
}
