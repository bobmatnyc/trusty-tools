//! Ticket workflow tools (#246).
//!
//! Why: Surface canonical workflow operations (tag, assign, transition,
//! search) so agents drive triage without provider-specific status strings.
//! What: Four tools — `ticket_tag`, `ticket_assign`, `ticket_transition`,
//! `ticket_search`. All implement `ToolExecutor`.
//! Test: Schema + happy-path tests in `super::tests`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::ticketing::{TicketFilter, TicketStatus, TicketingClient};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Tool: `ticket_tag` (#246).
///
/// Why: Lets agents add or remove labels without overwriting the rest of
/// a ticket's label set (the prior `update_ticket` semantic). Tags drive
/// triage workflows ("needs-review", "blocked", "good-first-issue").
/// What: Accepts an `id` plus optional `add` / `remove` arrays. Calls
/// `add_tags` then `remove_tags` (in that order) on the underlying
/// client and returns the updated ticket summary.
/// Test: `ticket_tag_tool_schema_requires_id`,
/// `ticket_tag_happy_path`.
pub struct TicketTagTool(pub Arc<dyn TicketingClient>);

#[async_trait]
impl ToolExecutor for TicketTagTool {
    fn name(&self) -> &str {
        "ticket_tag"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "ticket_tag",
                "description": "Add or remove tags/labels on a ticket without replacing the existing set.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "add": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Labels to add to the ticket."
                        },
                        "remove": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Labels to remove from the ticket."
                        }
                    },
                    "required": ["id"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(id) = args.get("id").and_then(Value::as_str) else {
            return ToolResult::err("ticket_tag: missing 'id'");
        };
        let add: Vec<String> = args
            .get("add")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let remove: Vec<String> = args
            .get("remove")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if add.is_empty() && remove.is_empty() {
            return ToolResult::err("ticket_tag: must provide at least one of 'add' or 'remove'");
        }
        if !add.is_empty()
            && let Err(e) = self.0.add_tags(id, &add).await
        {
            return ToolResult::err(format!("ticket_tag add failed: {e:#}"));
        }
        if !remove.is_empty()
            && let Err(e) = self.0.remove_tags(id, &remove).await
        {
            return ToolResult::err(format!("ticket_tag remove failed: {e:#}"));
        }
        match self.0.get_ticket(id).await {
            Ok(t) => ToolResult::ok(
                json!({
                    "id": t.id,
                    "labels": t.labels,
                    "url": t.url,
                })
                .to_string(),
            ),
            Err(e) => ToolResult::err(format!("ticket_tag: refetch failed: {e:#}")),
        }
    }
}

/// Tool: `ticket_assign` (#246).
///
/// Why: Ownership transfer is a common workflow event; pass `""` to
/// `unassign` rather than introducing a separate tool.
/// What: Accepts `id` and `assignee`. Empty string => `unassign`,
/// otherwise => `assign`.
/// Test: `ticket_assign_tool_schema_requires_id_and_assignee`.
pub struct TicketAssignTool(pub Arc<dyn TicketingClient>);

#[async_trait]
impl ToolExecutor for TicketAssignTool {
    fn name(&self) -> &str {
        "ticket_assign"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "ticket_assign",
                "description": "Assign or unassign a ticket. Pass empty string for assignee to unassign.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "assignee": {
                            "type": "string",
                            "description": "GitHub username, or empty string to unassign"
                        }
                    },
                    "required": ["id", "assignee"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(id) = args.get("id").and_then(Value::as_str) else {
            return ToolResult::err("ticket_assign: missing 'id'");
        };
        let Some(assignee) = args.get("assignee").and_then(Value::as_str) else {
            return ToolResult::err("ticket_assign: missing 'assignee'");
        };
        let result = if assignee.is_empty() {
            self.0.unassign(id).await
        } else {
            self.0.assign(id, assignee).await
        };
        match result {
            Ok(t) => match serde_json::to_string(&t) {
                Ok(s) => ToolResult::ok(s),
                Err(e) => ToolResult::err(format!("ticket_assign: serialize failed: {e}")),
            },
            Err(e) => ToolResult::err(format!("ticket_assign failed: {e:#}")),
        }
    }
}

/// Tool: `ticket_transition` (#246).
///
/// Why: Surface the canonical workflow transition (open / in_progress /
/// in_review / done / closed / blocked / cancelled) so the agent doesn't
/// have to fall back to provider-specific status strings.
/// What: Accepts `id` and `status`. Maps the string to a `TicketStatus`
/// and calls `transition`.
/// Test: `ticket_transition_tool_schema_has_status_enum`.
pub struct TicketTransitionTool(pub Arc<dyn TicketingClient>);

#[async_trait]
impl ToolExecutor for TicketTransitionTool {
    fn name(&self) -> &str {
        "ticket_transition"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "ticket_transition",
                "description": "Move a ticket to a new status (canonical workflow state).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "status": {
                            "type": "string",
                            "enum": [
                                "open", "in_progress", "in_review",
                                "done", "closed", "blocked", "cancelled"
                            ]
                        }
                    },
                    "required": ["id", "status"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(id) = args.get("id").and_then(Value::as_str) else {
            return ToolResult::err("ticket_transition: missing 'id'");
        };
        let Some(status_str) = args.get("status").and_then(Value::as_str) else {
            return ToolResult::err("ticket_transition: missing 'status'");
        };
        let status = match status_str {
            "open" => TicketStatus::Open,
            "in_progress" => TicketStatus::InProgress,
            "in_review" => TicketStatus::InReview,
            "done" => TicketStatus::Done,
            "closed" => TicketStatus::Closed,
            "blocked" => TicketStatus::Blocked,
            "cancelled" => TicketStatus::Cancelled,
            other => {
                return ToolResult::err(format!("ticket_transition: unknown status '{other}'"));
            }
        };
        match self.0.transition(id, status).await {
            Ok(t) => match serde_json::to_string(&t) {
                Ok(s) => ToolResult::ok(s),
                Err(e) => ToolResult::err(format!("ticket_transition: serialize failed: {e}")),
            },
            Err(e) => ToolResult::err(format!("ticket_transition failed: {e:#}")),
        }
    }
}

/// Tool: `ticket_search` (#246).
///
/// Why: `list_tickets` only filters by structured fields; `search` lets
/// the agent find tickets by free-text phrase ("cors bug").
/// What: Accepts `query`, optional `state` (open/closed/all, default
/// open) and optional `limit` (default 10).
/// Test: `ticket_search_tool_schema_requires_query`.
pub struct TicketSearchTool(pub Arc<dyn TicketingClient>);

#[async_trait]
impl ToolExecutor for TicketSearchTool {
    fn name(&self) -> &str {
        "ticket_search"
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "ticket_search",
                "description": "Full-text search across tickets in the configured provider.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "state": {
                            "type": "string",
                            "enum": ["open", "closed", "all"],
                            "default": "open"
                        },
                        "limit": {"type": "integer", "default": 10}
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return ToolResult::err("ticket_search: missing 'query'");
        };
        let state = args.get("state").and_then(Value::as_str).unwrap_or("open");
        let status = match state {
            "closed" => Some(TicketStatus::Closed),
            "all" => None,
            _ => Some(TicketStatus::Open),
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(10);
        let filter = TicketFilter {
            status,
            labels: vec![],
            assignee: None,
            limit: Some(limit),
        };
        match self.0.search(query, filter).await {
            Ok(ts) => match serde_json::to_string(&ts) {
                Ok(s) => ToolResult::ok(s),
                Err(e) => ToolResult::err(format!("ticket_search: serialize failed: {e}")),
            },
            Err(e) => ToolResult::err(format!("ticket_search failed: {e:#}")),
        }
    }
}
