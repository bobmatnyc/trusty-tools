use super::*;
use crate::ticketing::types::{Ticket, TicketStatus, UpdateTicketReq};
use crate::ticketing::{CreateTicketReq, TicketFilter, TicketingClient};
use crate::tools::traits::ToolExecutor;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};

/// Mock client that records calls and can be configured to fail.
struct MockClient {
    fail: bool,
}

#[async_trait]
impl TicketingClient for MockClient {
    fn provider_name(&self) -> &str {
        "mock"
    }

    async fn create_ticket(&self, req: CreateTicketReq) -> Result<Ticket> {
        if self.fail {
            return Err(anyhow!("boom"));
        }
        Ok(Ticket {
            id: "1".into(),
            title: req.title,
            body: req.body,
            status: TicketStatus::Open,
            priority: None,
            labels: req.labels,
            assignee: None,
            created_at: None,
            updated_at: None,
            url: Some("http://x/1".into()),
        })
    }

    async fn get_ticket(&self, id: &str) -> Result<Ticket> {
        if self.fail {
            return Err(anyhow!("boom"));
        }
        Ok(Ticket {
            id: id.into(),
            title: "T".into(),
            body: "B".into(),
            status: TicketStatus::Open,
            priority: None,
            labels: vec![],
            assignee: None,
            created_at: None,
            updated_at: None,
            url: None,
        })
    }

    async fn update_ticket(&self, id: &str, _req: UpdateTicketReq) -> Result<Ticket> {
        self.get_ticket(id).await
    }

    async fn close_ticket(&self, _id: &str) -> Result<()> {
        if self.fail {
            Err(anyhow!("boom"))
        } else {
            Ok(())
        }
    }

    async fn list_tickets(&self, _f: TicketFilter) -> Result<Vec<Ticket>> {
        if self.fail {
            return Err(anyhow!("boom"));
        }
        Ok(vec![])
    }

    async fn add_comment(&self, _id: &str, _body: &str) -> Result<()> {
        if self.fail {
            Err(anyhow!("boom"))
        } else {
            Ok(())
        }
    }

    async fn add_tags(&self, id: &str, _tags: &[String]) -> Result<Ticket> {
        if self.fail {
            return Err(anyhow!("boom"));
        }
        self.get_ticket(id).await
    }

    async fn remove_tags(&self, id: &str, _tags: &[String]) -> Result<Ticket> {
        if self.fail {
            return Err(anyhow!("boom"));
        }
        self.get_ticket(id).await
    }

    async fn assign(&self, id: &str, _assignee: &str) -> Result<Ticket> {
        if self.fail {
            return Err(anyhow!("boom"));
        }
        self.get_ticket(id).await
    }

    async fn unassign(&self, id: &str) -> Result<Ticket> {
        if self.fail {
            return Err(anyhow!("boom"));
        }
        self.get_ticket(id).await
    }

    async fn search(&self, _query: &str, _filter: TicketFilter) -> Result<Vec<Ticket>> {
        if self.fail {
            return Err(anyhow!("boom"));
        }
        Ok(vec![])
    }
}

fn ok_client() -> Arc<dyn TicketingClient> {
    Arc::new(MockClient { fail: false })
}

fn fail_client() -> Arc<dyn TicketingClient> {
    Arc::new(MockClient { fail: true })
}

#[tokio::test]
async fn create_ticket_happy_path() {
    let tool = CreateTicketTool(ok_client());
    assert_eq!(tool.name(), "create_ticket");
    let out = tool
        .execute(json!({"title": "T", "body": "B", "labels": ["bug"]}))
        .await;
    assert!(!out.is_error());
    let v: Value = serde_json::from_str(out.content()).unwrap();
    assert_eq!(v["id"], "1");
}

#[tokio::test]
async fn create_ticket_missing_title() {
    let tool = CreateTicketTool(ok_client());
    assert!(tool.execute(json!({"body": "b"})).await.is_error());
}

#[tokio::test]
async fn create_ticket_propagates_error() {
    let tool = CreateTicketTool(fail_client());
    let out = tool.execute(json!({"title": "T", "body": "B"})).await;
    assert!(out.is_error());
}

#[tokio::test]
async fn get_ticket_happy_path() {
    let tool = GetTicketTool(ok_client());
    let out = tool.execute(json!({"id": "42"})).await;
    assert!(!out.is_error());
    let v: Value = serde_json::from_str(out.content()).unwrap();
    assert_eq!(v["id"], "42");
}

#[tokio::test]
async fn get_ticket_missing_id() {
    let tool = GetTicketTool(ok_client());
    assert!(tool.execute(json!({})).await.is_error());
}

#[tokio::test]
async fn close_ticket_happy_path() {
    let tool = CloseTicketTool(ok_client());
    let out = tool.execute(json!({"id": "1"})).await;
    assert!(!out.is_error());
}

#[tokio::test]
async fn close_ticket_with_comment() {
    let tool = CloseTicketTool(ok_client());
    let out = tool.execute(json!({"id": "1", "comment": "fixed"})).await;
    assert!(!out.is_error());
}

#[tokio::test]
async fn list_tickets_happy_path() {
    let tool = ListTicketsTool(ok_client());
    let out = tool.execute(json!({})).await;
    assert!(!out.is_error());
}

#[tokio::test]
async fn list_tickets_with_filters() {
    let tool = ListTicketsTool(ok_client());
    let out = tool.execute(json!({"status": "open", "limit": 10})).await;
    assert!(!out.is_error());
}

#[tokio::test]
async fn add_comment_happy_path() {
    let tool = AddCommentTool(ok_client());
    let out = tool.execute(json!({"id": "1", "body": "hi"})).await;
    assert!(!out.is_error());
}

#[tokio::test]
async fn add_comment_missing_body() {
    let tool = AddCommentTool(ok_client());
    assert!(tool.execute(json!({"id": "1"})).await.is_error());
}

// ----- #243: UpdateTicketTool + Actions tools + ticketing_tools() -----

#[test]
fn update_ticket_tool_schema_has_required_id() {
    let tool = UpdateTicketTool(ok_client());
    let s = tool.schema();
    assert_eq!(s["function"]["name"], "update_ticket");
    let required = s["function"]["parameters"]["required"]
        .as_array()
        .expect("required is array");
    assert!(required.iter().any(|v| v == "id"));
    // Status enum is locked to canonical values.
    let status_enum = &s["function"]["parameters"]["properties"]["status"]["enum"];
    assert_eq!(status_enum[0], "open");
    assert_eq!(status_enum[1], "in_progress");
}

#[tokio::test]
async fn update_ticket_happy_path() {
    let tool = UpdateTicketTool(ok_client());
    let out = tool
        .execute(json!({"id": "5", "title": "new", "status": "in_progress"}))
        .await;
    assert!(!out.is_error(), "got error: {}", out.content());
    let v: Value = serde_json::from_str(out.content()).unwrap();
    assert_eq!(v["id"], "5");
}

#[tokio::test]
async fn update_ticket_missing_id() {
    let tool = UpdateTicketTool(ok_client());
    assert!(tool.execute(json!({"title": "x"})).await.is_error());
}

fn fake_actions_client() -> Arc<dyn ActionsClient> {
    Arc::new(
        crate::ticketing::actions::GitHubActionsClient::new("fake-token", "owner/repo")
            .expect("fake actions client"),
    )
}

#[test]
fn actions_trigger_tool_schema_is_valid() {
    let tool = ActionsTriggerTool {
        client: fake_actions_client(),
    };
    let s = tool.schema();
    assert_eq!(s["function"]["name"], "actions_trigger");
    let required = s["function"]["parameters"]["required"]
        .as_array()
        .expect("required is array");
    assert!(required.iter().any(|v| v == "workflow"));
    // 'ref' is optional with a default.
    assert_eq!(
        s["function"]["parameters"]["properties"]["ref"]["default"],
        "main"
    );
}

#[test]
fn actions_status_tool_schema_is_valid() {
    let tool = ActionsStatusTool {
        client: fake_actions_client(),
    };
    let s = tool.schema();
    assert_eq!(s["function"]["name"], "actions_status");
    let required = s["function"]["parameters"]["required"]
        .as_array()
        .expect("required is array");
    assert!(required.iter().any(|v| v == "workflow"));
    assert_eq!(
        s["function"]["parameters"]["properties"]["limit"]["default"],
        5
    );
}

#[tokio::test]
async fn actions_trigger_missing_workflow_errors() {
    let tool = ActionsTriggerTool {
        client: fake_actions_client(),
    };
    // No 'workflow' arg — must fail before any network call.
    assert!(tool.execute(json!({})).await.is_error());
}

#[tokio::test]
async fn actions_status_missing_workflow_errors() {
    let tool = ActionsStatusTool {
        client: fake_actions_client(),
    };
    assert!(tool.execute(json!({})).await.is_error());
}

#[test]
fn ticketing_tools_count() {
    // Without actions: 10 tools (6 originals + ticket_tag/assign/transition/search).
    let tools = ticketing_tools(ok_client(), None);
    assert_eq!(tools.len(), 10);
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"create_ticket"));
    assert!(names.contains(&"get_ticket"));
    assert!(names.contains(&"update_ticket"));
    assert!(names.contains(&"close_ticket"));
    assert!(names.contains(&"list_tickets"));
    assert!(names.contains(&"add_comment"));
    assert!(names.contains(&"ticket_tag"));
    assert!(names.contains(&"ticket_assign"));
    assert!(names.contains(&"ticket_transition"));
    assert!(names.contains(&"ticket_search"));

    // With actions: 10 + 2 = 12.
    let tools_full = ticketing_tools(ok_client(), Some(fake_actions_client()));
    assert_eq!(tools_full.len(), 12);
    let names_full: Vec<&str> = tools_full.iter().map(|t| t.name()).collect();
    assert!(names_full.contains(&"actions_trigger"));
    assert!(names_full.contains(&"actions_status"));
}

// ----- #246: ticket_tag / ticket_assign / ticket_transition / ticket_search -----

#[test]
fn ticket_tag_tool_schema_requires_id() {
    let tool = TicketTagTool(ok_client());
    let s = tool.schema();
    assert_eq!(s["function"]["name"], "ticket_tag");
    let required = s["function"]["parameters"]["required"]
        .as_array()
        .expect("required is array");
    assert_eq!(required.len(), 1);
    assert_eq!(required[0], "id");
    // Both add and remove arrays of strings.
    assert_eq!(
        s["function"]["parameters"]["properties"]["add"]["type"],
        "array"
    );
    assert_eq!(
        s["function"]["parameters"]["properties"]["remove"]["type"],
        "array"
    );
}

#[tokio::test]
async fn ticket_tag_requires_at_least_one_action() {
    let tool = TicketTagTool(ok_client());
    let out = tool.execute(json!({"id": "1"})).await;
    assert!(out.is_error());
}

#[tokio::test]
async fn ticket_tag_happy_path_add() {
    let tool = TicketTagTool(ok_client());
    let out = tool.execute(json!({"id": "1", "add": ["bug"]})).await;
    assert!(!out.is_error(), "got error: {}", out.content());
}

#[test]
fn ticket_assign_tool_schema_requires_id_and_assignee() {
    let tool = TicketAssignTool(ok_client());
    let s = tool.schema();
    assert_eq!(s["function"]["name"], "ticket_assign");
    let required = s["function"]["parameters"]["required"]
        .as_array()
        .expect("required is array");
    assert!(required.iter().any(|v| v == "id"));
    assert!(required.iter().any(|v| v == "assignee"));
    assert_eq!(required.len(), 2);
}

#[tokio::test]
async fn ticket_assign_with_user() {
    let tool = TicketAssignTool(ok_client());
    let out = tool.execute(json!({"id": "1", "assignee": "alice"})).await;
    assert!(!out.is_error(), "got error: {}", out.content());
}

#[tokio::test]
async fn ticket_assign_empty_string_unassigns() {
    let tool = TicketAssignTool(ok_client());
    let out = tool.execute(json!({"id": "1", "assignee": ""})).await;
    assert!(!out.is_error(), "got error: {}", out.content());
}

#[test]
fn ticket_transition_tool_schema_has_status_enum() {
    let tool = TicketTransitionTool(ok_client());
    let s = tool.schema();
    assert_eq!(s["function"]["name"], "ticket_transition");
    let required = s["function"]["parameters"]["required"]
        .as_array()
        .expect("required is array");
    assert!(required.iter().any(|v| v == "id"));
    assert!(required.iter().any(|v| v == "status"));
    let status_enum = s["function"]["parameters"]["properties"]["status"]["enum"]
        .as_array()
        .expect("status enum is array");
    let names: Vec<&str> = status_enum.iter().filter_map(|v| v.as_str()).collect();
    assert!(names.contains(&"open"));
    assert!(names.contains(&"in_progress"));
    assert!(names.contains(&"in_review"));
    assert!(names.contains(&"done"));
    assert!(names.contains(&"closed"));
    assert!(names.contains(&"blocked"));
    assert!(names.contains(&"cancelled"));
}

#[tokio::test]
async fn ticket_transition_unknown_status_errors() {
    let tool = TicketTransitionTool(ok_client());
    let out = tool.execute(json!({"id": "1", "status": "garbage"})).await;
    assert!(out.is_error());
}

#[test]
fn ticket_search_tool_schema_requires_query() {
    let tool = TicketSearchTool(ok_client());
    let s = tool.schema();
    assert_eq!(s["function"]["name"], "ticket_search");
    let required = s["function"]["parameters"]["required"]
        .as_array()
        .expect("required is array");
    assert_eq!(required.len(), 1);
    assert_eq!(required[0], "query");
    // state has a default of "open".
    assert_eq!(
        s["function"]["parameters"]["properties"]["state"]["default"],
        "open"
    );
    // limit has default 10.
    assert_eq!(
        s["function"]["parameters"]["properties"]["limit"]["default"],
        10
    );
}

#[tokio::test]
async fn ticket_search_missing_query_errors() {
    let tool = TicketSearchTool(ok_client());
    let out = tool.execute(json!({})).await;
    assert!(out.is_error());
}

#[tokio::test]
async fn ticket_search_happy_path() {
    let tool = TicketSearchTool(ok_client());
    let out = tool.execute(json!({"query": "cors"})).await;
    assert!(!out.is_error(), "got error: {}", out.content());
}

#[test]
fn ticketing_tools_count_is_12() {
    // Total 12 tools when actions client is provided (10 ticketing + 2 actions).
    let tools = ticketing_tools(ok_client(), Some(fake_actions_client()));
    assert_eq!(tools.len(), 12);
}
