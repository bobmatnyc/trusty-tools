//! MCP JSON-RPC dispatch + stdio loop for `tickets-mcp`.
//!
//! Why: Follows the same shape as every other trusty-* MCP server —
//! `initialize`, `tools/list`, `tools/call`, notifications suppressed.
//! What: `AppState` holds an `Arc<BackendClient>`. Each tool resolves a
//! backend and calls one trait method.
//! Test: `handle_message_initialize_returns_server_info` covers handshake;
//! `tools/list` length is checked; unknown methods return INVALID_REQUEST.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::mcp::{Request, Response, error_codes, initialize_response, run_stdio_loop};
use crate::tickets::api::backends::{
    Backend, CreateIssueParams, CreateMilestoneParams, ListIssuesParams, SearchIssuesParams,
    UpdateIssueParams,
};
use crate::tickets::api::client::BackendClient;

/// Shared state passed to every dispatcher invocation.
///
/// Why: Tool handlers need the configured backend set.
/// What: `Clone`-able via `Arc`.
/// Test: constructed in module tests.
#[derive(Clone)]
pub struct AppState {
    pub client: Arc<BackendClient>,
}

// ---- helpers to extract args ----

fn str_arg(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow!("missing required string arg: {key}"))
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(String::from)
}

fn opt_strings(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn opt_u32(args: &Value, key: &str, default: u32) -> u32 {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .unwrap_or(default)
}

/// Resolve the backend from the request args + dispatcher.
fn backend_for(state: &AppState, args: &Value) -> Result<Arc<dyn Backend>> {
    let name = args.get("backend").and_then(|v| v.as_str());
    state.client.resolve(name)
}

/// Convert anything serializable into JSON.
fn to_json<T: serde::Serialize>(t: T) -> Result<Value> {
    serde_json::to_value(t).map_err(Into::into)
}

/// Dispatch one tool call.
///
/// Why: One arm per tool keeps the routing table grep-able.
/// What: Each branch builds the param struct, picks a backend, and
/// serialises the result.
/// Test: indirect — covered by integration testing.
pub async fn handle_tool_call(state: &AppState, name: &str, args: Value) -> Value {
    let result: Result<Value> = dispatch(state, name, args).await;
    match result {
        Ok(v) => v,
        Err(e) => json!({ "error": e.to_string() }),
    }
}

async fn dispatch(state: &AppState, name: &str, args: Value) -> Result<Value> {
    match name {
        // ----- Issues -----
        "create_issue" => {
            let b = backend_for(state, &args)?;
            let p = CreateIssueParams {
                title: str_arg(&args, "title")?,
                description: opt_str(&args, "description"),
                priority: opt_str(&args, "priority"),
                assignee: opt_str(&args, "assignee"),
                labels: opt_strings(&args, "labels"),
                milestone_id: opt_str(&args, "milestone_id"),
                project_id: opt_str(&args, "project_id"),
                parent_id: opt_str(&args, "parent_id"),
                issue_type: opt_str(&args, "issue_type"),
            };
            to_json(b.create_issue(p).await?)
        }
        "get_issue" => {
            let b = backend_for(state, &args)?;
            let id = str_arg(&args, "issue_id")?;
            to_json(b.get_issue(&id).await?)
        }
        "update_issue" => {
            let b = backend_for(state, &args)?;
            let id = str_arg(&args, "issue_id")?;
            let p = UpdateIssueParams {
                title: opt_str(&args, "title"),
                description: opt_str(&args, "description"),
                priority: opt_str(&args, "priority"),
                assignee: opt_str(&args, "assignee"),
                labels: if args.get("labels").is_some() {
                    Some(opt_strings(&args, "labels"))
                } else {
                    None
                },
                milestone_id: opt_str(&args, "milestone_id"),
                state: opt_str(&args, "state"),
            };
            to_json(b.update_issue(&id, p).await?)
        }
        "close_issue" => {
            let b = backend_for(state, &args)?;
            let id = str_arg(&args, "issue_id")?;
            let comment = opt_str(&args, "comment");
            to_json(b.close_issue(&id, comment.as_deref()).await?)
        }
        "reopen_issue" => {
            let b = backend_for(state, &args)?;
            let id = str_arg(&args, "issue_id")?;
            to_json(b.reopen_issue(&id).await?)
        }
        "list_issues" => {
            let b = backend_for(state, &args)?;
            let p = ListIssuesParams {
                project_id: opt_str(&args, "project_id"),
                state: opt_str(&args, "state"),
                assignee: opt_str(&args, "assignee"),
                labels: opt_strings(&args, "labels"),
                limit: opt_u32(&args, "limit", 20),
                offset: opt_u32(&args, "offset", 0),
            };
            to_json(b.list_issues(p).await?)
        }
        "search_issues" => {
            let b = backend_for(state, &args)?;
            let p = SearchIssuesParams {
                query: opt_str(&args, "query"),
                state: opt_str(&args, "state"),
                priority: opt_str(&args, "priority"),
                labels: opt_strings(&args, "labels"),
                assignee: opt_str(&args, "assignee"),
                project_id: opt_str(&args, "project_id"),
                milestone_id: opt_str(&args, "milestone_id"),
                limit: opt_u32(&args, "limit", 10),
                offset: opt_u32(&args, "offset", 0),
            };
            to_json(b.search_issues(p).await?)
        }

        // ----- Comments -----
        "add_comment" => {
            let b = backend_for(state, &args)?;
            to_json(
                b.add_comment(&str_arg(&args, "issue_id")?, &str_arg(&args, "body")?)
                    .await?,
            )
        }
        "list_comments" => {
            let b = backend_for(state, &args)?;
            to_json(b.list_comments(&str_arg(&args, "issue_id")?).await?)
        }
        "update_comment" => {
            let b = backend_for(state, &args)?;
            to_json(
                b.update_comment(
                    &str_arg(&args, "issue_id")?,
                    &str_arg(&args, "comment_id")?,
                    &str_arg(&args, "body")?,
                )
                .await?,
            )
        }
        "delete_comment" => {
            let b = backend_for(state, &args)?;
            b.delete_comment(&str_arg(&args, "issue_id")?, &str_arg(&args, "comment_id")?)
                .await?;
            Ok(json!({ "deleted": true }))
        }

        // ----- Labels -----
        "list_labels" => {
            let b = backend_for(state, &args)?;
            to_json(b.list_labels().await?)
        }
        "create_label" => {
            let b = backend_for(state, &args)?;
            to_json(
                b.create_label(
                    &str_arg(&args, "name")?,
                    opt_str(&args, "color").as_deref(),
                    opt_str(&args, "description").as_deref(),
                )
                .await?,
            )
        }
        "add_labels" => {
            let b = backend_for(state, &args)?;
            let labels = opt_strings(&args, "labels");
            b.add_labels(&str_arg(&args, "issue_id")?, &labels).await?;
            Ok(json!({ "added": labels }))
        }
        "remove_labels" => {
            let b = backend_for(state, &args)?;
            let labels = opt_strings(&args, "labels");
            b.remove_labels(&str_arg(&args, "issue_id")?, &labels)
                .await?;
            Ok(json!({ "removed": labels }))
        }

        // ----- Milestones -----
        "list_milestones" => {
            let b = backend_for(state, &args)?;
            to_json(b.list_milestones().await?)
        }
        "create_milestone" => {
            let b = backend_for(state, &args)?;
            let p = CreateMilestoneParams {
                name: str_arg(&args, "name")?,
                description: opt_str(&args, "description"),
                due_date: opt_str(&args, "due_date"),
            };
            to_json(b.create_milestone(p).await?)
        }
        "close_milestone" => {
            let b = backend_for(state, &args)?;
            to_json(b.close_milestone(&str_arg(&args, "milestone_id")?).await?)
        }
        "get_milestone_issues" => {
            let b = backend_for(state, &args)?;
            to_json(
                b.get_milestone_issues(&str_arg(&args, "milestone_id")?)
                    .await?,
            )
        }

        // ----- Projects / Epics -----
        "list_projects" => {
            let b = backend_for(state, &args)?;
            to_json(b.list_projects().await?)
        }
        "get_project" => {
            let b = backend_for(state, &args)?;
            to_json(b.get_project(&str_arg(&args, "project_id")?).await?)
        }
        "list_epics" => {
            let b = backend_for(state, &args)?;
            to_json(b.list_epics().await?)
        }
        "get_epic_issues" => {
            let b = backend_for(state, &args)?;
            to_json(b.get_epic_issues(&str_arg(&args, "epic_id")?).await?)
        }
        "create_project_update" => {
            let b = backend_for(state, &args)?;
            to_json(
                b.create_project_update(
                    &str_arg(&args, "project_id")?,
                    &str_arg(&args, "body")?,
                    opt_str(&args, "health").as_deref(),
                )
                .await?,
            )
        }
        "list_project_updates" => {
            let b = backend_for(state, &args)?;
            to_json(
                b.list_project_updates(&str_arg(&args, "project_id")?)
                    .await?,
            )
        }

        // ----- Workflow -----
        "list_states" => {
            let b = backend_for(state, &args)?;
            to_json(b.list_states().await?)
        }
        "transition_issue" => {
            let b = backend_for(state, &args)?;
            to_json(
                b.transition_issue(&str_arg(&args, "issue_id")?, &str_arg(&args, "state")?)
                    .await?,
            )
        }
        "assign_issue" => {
            let b = backend_for(state, &args)?;
            to_json(
                b.assign_issue(&str_arg(&args, "issue_id")?, &str_arg(&args, "assignee")?)
                    .await?,
            )
        }

        // ----- Meta -----
        "list_backends" => Ok(json!({
            "backends": state.client.list_backends(),
            "default": state.client.default_backend(),
        })),
        "list_teams" => {
            // Best effort: equivalent to list_projects for now.
            let b = backend_for(state, &args)?;
            to_json(b.list_projects().await?)
        }

        other => Err(anyhow!("unknown tool: {other}")),
    }
}

/// Translate a parsed JSON-RPC request into a JSON-RPC response payload.
///
/// Why: Same shape used across the trusty-* family.
/// What: Notifications return `Value::Null`; unknown methods return an
/// error map.
/// Test: `handle_message_initialize_returns_server_info`.
pub async fn handle_message(state: AppState, req: Value) -> Value {
    let method = req["method"].as_str().unwrap_or("");
    match method {
        "initialize" => initialize_response("tickets-mcp", env!("CARGO_PKG_VERSION"), None),
        "notifications/initialized" | "notifications/cancelled" => Value::Null,
        "ping" => json!({}),
        "tools/list" => crate::tickets::tools::tool_list_response(),
        "tools/call" => {
            let params = &req["params"];
            let tool_name = params["name"].as_str().unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            let args = if args.is_null() { json!({}) } else { args };
            let result = handle_tool_call(&state, tool_name, args).await;
            let text = serde_json::to_string(&result).unwrap_or_default();
            json!({ "content": [{ "type": "text", "text": text }] })
        }
        _ => json!({
            "error": {
                "code": error_codes::METHOD_NOT_FOUND,
                "message": format!("Method not found: {method}"),
            }
        }),
    }
}

/// Run the stdio MCP loop wired to this server.
///
/// Why: Single entry point for `bin/tickets-mcp.rs`.
/// What: Forwards each parsed `Request` to `handle_message`.
/// Test: manual via Claude Code.
pub async fn run_stdio(state: AppState) -> Result<()> {
    run_stdio_loop(move |req: Request| {
        let state = state.clone();
        async move {
            let id = req.id.clone();
            let raw = serde_json::to_value(&req).unwrap_or(Value::Null);
            let resp = handle_message(state, raw).await;
            if resp.is_null() {
                return Response::suppressed();
            }
            if let Some(err) = resp.get("error") {
                let code =
                    err.get("code")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(error_codes::INTERNAL_ERROR as i64) as i32;
                let message = err
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Internal error")
                    .to_string();
                return Response::err(id, code, message);
            }
            Response::ok(id, resp)
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tickets::api::config::Config;

    async fn make_state() -> AppState {
        let client = BackendClient::from_config(Config::default()).await.unwrap();
        AppState {
            client: Arc::new(client),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_message_initialize_returns_server_info() {
        let state = make_state().await;
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });
        let resp = handle_message(state, req).await;
        assert_eq!(resp["serverInfo"]["name"], "tickets-mcp");
        assert!(resp["capabilities"]["tools"].is_object());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_message_tools_list_returns_tools() {
        let state = make_state().await;
        let req = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let resp = handle_message(state, req).await;
        let tools = resp["tools"].as_array().unwrap();
        assert!(
            tools.len() >= 30,
            "expected >= 30 tools, got {}",
            tools.len()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_method_returns_error_payload() {
        let state = make_state().await;
        let req = json!({ "jsonrpc": "2.0", "id": 3, "method": "no/such/method" });
        let resp = handle_message(state, req).await;
        assert_eq!(resp["error"]["code"], error_codes::METHOD_NOT_FOUND);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn list_backends_works_with_empty_config() {
        let state = make_state().await;
        let v = handle_tool_call(&state, "list_backends", json!({})).await;
        assert!(v["backends"].is_array());
    }
}
