//! CRUD ticketing tools (#132/#133/#243).
//!
//! Why: Expose `TicketingClient` create/read/update/list operations as
//! strongly-typed LLM tools so agents don't have to shell out to `gh` / JIRA
//! CLI / Linear CLI.
//! What: Six tools — `create_ticket`, `get_ticket`, `close_ticket`,
//! `list_tickets`, `add_comment`, `update_ticket`. All implement `ToolExecutor`.
//! Test: Construction and error-path tests in `super::tests`, using a mock
//! `TicketingClient` implementation.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::ticketing::{
    CreateTicketReq, Priority, TicketFilter, TicketStatus, TicketingClient, UpdateTicketReq,
};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Tool: `create_ticket`.
pub struct CreateTicketTool(pub Arc<dyn TicketingClient>);

#[async_trait]
impl ToolExecutor for CreateTicketTool {
    fn name(&self) -> &str {
        "create_ticket"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "create_ticket",
                "description": "Create a new ticket in the configured ticketing provider (GitHub / JIRA / Linear).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "title": {"type": "string"},
                        "body": {"type": "string"},
                        "labels": {"type": "array", "items": {"type": "string"}},
                        "priority": {"type": "string", "enum": ["low", "medium", "high", "critical"]},
                        "assignee": {"type": "string"}
                    },
                    "required": ["title", "body"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(title) = args.get("title").and_then(Value::as_str) else {
            return ToolResult::err("create_ticket: missing 'title'");
        };
        let Some(body) = args.get("body").and_then(Value::as_str) else {
            return ToolResult::err("create_ticket: missing 'body'");
        };
        let labels: Vec<String> = args
            .get("labels")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let priority = args
            .get("priority")
            .and_then(Value::as_str)
            .and_then(|s| match s {
                "low" => Some(Priority::Low),
                "medium" => Some(Priority::Medium),
                "high" => Some(Priority::High),
                "critical" => Some(Priority::Critical),
                _ => None,
            });
        let assignee = args
            .get("assignee")
            .and_then(Value::as_str)
            .map(String::from);

        let req = CreateTicketReq {
            title: title.to_string(),
            body: body.to_string(),
            labels,
            priority,
            assignee,
        };
        match self.0.create_ticket(req).await {
            Ok(t) => {
                let out = json!({
                    "id": t.id,
                    "title": t.title,
                    "url": t.url,
                });
                ToolResult::ok(out.to_string())
            }
            Err(e) => ToolResult::err(format!("create_ticket failed: {e:#}")),
        }
    }
}

/// Tool: `get_ticket`.
pub struct GetTicketTool(pub Arc<dyn TicketingClient>);

#[async_trait]
impl ToolExecutor for GetTicketTool {
    fn name(&self) -> &str {
        "get_ticket"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "get_ticket",
                "description": "Fetch a ticket by its provider-native id.",
                "parameters": {
                    "type": "object",
                    "properties": {"id": {"type": "string"}},
                    "required": ["id"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(id) = args.get("id").and_then(Value::as_str) else {
            return ToolResult::err("get_ticket: missing 'id'");
        };
        match self.0.get_ticket(id).await {
            Ok(t) => match serde_json::to_string(&t) {
                Ok(s) => ToolResult::ok(s),
                Err(e) => ToolResult::err(format!("get_ticket: serialize failed: {e}")),
            },
            Err(e) => ToolResult::err(format!("get_ticket failed: {e:#}")),
        }
    }
}

/// Tool: `close_ticket`.
pub struct CloseTicketTool(pub Arc<dyn TicketingClient>);

#[async_trait]
impl ToolExecutor for CloseTicketTool {
    fn name(&self) -> &str {
        "close_ticket"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "close_ticket",
                "description": "Close / complete a ticket. Optional comment is posted before close.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "comment": {"type": "string"}
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(id) = args.get("id").and_then(Value::as_str) else {
            return ToolResult::err("close_ticket: missing 'id'");
        };
        if let Some(comment) = args.get("comment").and_then(Value::as_str)
            && !comment.is_empty()
            && let Err(e) = self.0.add_comment(id, comment).await
        {
            return ToolResult::err(format!("close_ticket: comment failed: {e:#}"));
        }
        match self.0.close_ticket(id).await {
            Ok(()) => ToolResult::ok(json!({"closed": true, "id": id}).to_string()),
            Err(e) => ToolResult::err(format!("close_ticket failed: {e:#}")),
        }
    }
}

/// Tool: `list_tickets`.
pub struct ListTicketsTool(pub Arc<dyn TicketingClient>);

#[async_trait]
impl ToolExecutor for ListTicketsTool {
    fn name(&self) -> &str {
        "list_tickets"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "list_tickets",
                "description": "List tickets, optionally filtered by status/labels/limit.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "status": {"type": "string", "enum": ["open", "in_progress", "done", "closed"]},
                        "labels": {"type": "array", "items": {"type": "string"}},
                        "limit": {"type": "integer"}
                    },
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let status = args
            .get("status")
            .and_then(Value::as_str)
            .and_then(|s| match s {
                "open" => Some(TicketStatus::Open),
                "in_progress" => Some(TicketStatus::InProgress),
                "done" => Some(TicketStatus::Done),
                "closed" => Some(TicketStatus::Closed),
                _ => None,
            });
        let labels: Vec<String> = args
            .get("labels")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize);

        let filter = TicketFilter {
            status,
            labels,
            assignee: None,
            limit,
        };
        match self.0.list_tickets(filter).await {
            Ok(ts) => match serde_json::to_string(&ts) {
                Ok(s) => ToolResult::ok(s),
                Err(e) => ToolResult::err(format!("list_tickets: serialize failed: {e}")),
            },
            Err(e) => ToolResult::err(format!("list_tickets failed: {e:#}")),
        }
    }
}

/// Tool: `add_comment`.
pub struct AddCommentTool(pub Arc<dyn TicketingClient>);

#[async_trait]
impl ToolExecutor for AddCommentTool {
    fn name(&self) -> &str {
        "add_comment"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "add_comment",
                "description": "Add a comment to an existing ticket.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "body": {"type": "string"}
                    },
                    "required": ["id", "body"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(id) = args.get("id").and_then(Value::as_str) else {
            return ToolResult::err("add_comment: missing 'id'");
        };
        let Some(body) = args.get("body").and_then(Value::as_str) else {
            return ToolResult::err("add_comment: missing 'body'");
        };
        match self.0.add_comment(id, body).await {
            Ok(()) => ToolResult::ok(json!({"added": true}).to_string()),
            Err(e) => ToolResult::err(format!("add_comment failed: {e:#}")),
        }
    }
}

/// Tool: `update_ticket` (#243).
///
/// Why: Closing the loop — agents often need to amend a ticket's title/body
/// or set status mid-workflow without going through close+recreate.
/// What: Accepts `id` plus any subset of `title`/`body`/`status`/`labels`/
/// `assignee` and forwards a partial `UpdateTicketReq` to the client.
/// Test: `update_ticket_tool_schema_has_required_id`,
/// `update_ticket_happy_path`.
pub struct UpdateTicketTool(pub Arc<dyn TicketingClient>);

#[async_trait]
impl ToolExecutor for UpdateTicketTool {
    fn name(&self) -> &str {
        "update_ticket"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "update_ticket",
                "description": "Update an existing ticket. Pass any subset of title/body/status/labels/assignee; unspecified fields are left unchanged.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "title": {"type": "string"},
                        "body": {"type": "string"},
                        "status": {"type": "string", "enum": ["open", "in_progress", "done", "closed"]},
                        "labels": {"type": "array", "items": {"type": "string"}},
                        "assignee": {"type": "string"}
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(id) = args.get("id").and_then(Value::as_str) else {
            return ToolResult::err("update_ticket: missing 'id'");
        };
        let title = args.get("title").and_then(Value::as_str).map(String::from);
        let body = args.get("body").and_then(Value::as_str).map(String::from);
        let status = args
            .get("status")
            .and_then(Value::as_str)
            .and_then(|s| match s {
                "open" => Some(TicketStatus::Open),
                "in_progress" => Some(TicketStatus::InProgress),
                "done" => Some(TicketStatus::Done),
                "closed" => Some(TicketStatus::Closed),
                _ => None,
            });
        let labels = args.get("labels").and_then(Value::as_array).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        });
        let assignee = args
            .get("assignee")
            .and_then(Value::as_str)
            .map(String::from);

        let req = UpdateTicketReq {
            title,
            body,
            status,
            labels,
            assignee,
            ..Default::default()
        };
        match self.0.update_ticket(id, req).await {
            Ok(t) => match serde_json::to_string(&t) {
                Ok(s) => ToolResult::ok(s),
                Err(e) => ToolResult::err(format!("update_ticket: serialize failed: {e}")),
            },
            Err(e) => ToolResult::err(format!("update_ticket failed: {e:#}")),
        }
    }
}
