//! MCP JSON-RPC dispatch + stdio loop for `gworkspace-mcp`.
//!
//! Why: Every trusty-* MCP server uses the same shape — `initialize`,
//! `tools/list`, `tools/call`, with notifications suppressed. Centralising
//! here means tool handlers can focus on Google API specifics.
//! What: `AppState` holds an `Arc<BaseClient>`; `handle_message` is the
//! pure JSON-RPC dispatcher; `run_stdio` wires it into
//! `trusty_mcp_core::run_stdio_loop`.
//! Test: `handle_message_initialize_returns_server_info` covers the
//! handshake; tool dispatch is exercised indirectly by integration tests.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::services;
use trusty_common::mcp::{Request, Response, error_codes, initialize_response, run_stdio_loop};

/// Shared state passed to every dispatcher invocation.
///
/// Why: Tool handlers need the authenticated client; nothing else is
/// currently shared across calls.
/// What: `Clone`-able via `Arc`.
/// Test: smoke construction in `mod tests` below.
#[derive(Clone)]
pub struct AppState {
    pub client: Arc<BaseClient>,
}

/// Dispatch a single tool call by name.
///
/// Why: One match arm per tool keeps the routing table greppable; new
/// tools added to `tools.rs` need a matching arm here.
/// What: Returns the JSON value to wrap inside an MCP `content` envelope.
/// On error, returns `{"error": "<message>"}` so the model gets actionable
/// feedback rather than a JSON-RPC framing-level failure.
/// Test: covered indirectly by `tools/call` tests.
pub async fn handle_tool_call(state: &AppState, name: &str, args: Value) -> Value {
    let result: anyhow::Result<Value> = match name {
        // Accounts
        "list_accounts" => services::accounts::list_accounts(&state.client, args).await,

        // Calendar
        "manage_calendars" => services::calendar::manage_calendars(&state.client, args).await,
        "manage_events" => services::calendar::manage_events(&state.client, args).await,
        "query_free_busy" => services::calendar::query_free_busy(&state.client, args).await,

        // Gmail
        "search_gmail_messages" => {
            services::gmail::messages::search_gmail_messages(&state.client, args).await
        }
        "get_gmail_message_content" => {
            services::gmail::messages::get_gmail_message_content(&state.client, args).await
        }
        "download_gmail_attachment" => {
            services::gmail::messages::download_gmail_attachment(&state.client, args).await
        }
        "list_message_attachments" => {
            services::gmail::messages::list_message_attachments(&state.client, args).await
        }
        "compose_email" => services::gmail::messages::compose_email(&state.client, args).await,
        "modify_gmail_messages" => {
            services::gmail::messages::modify_gmail_messages(&state.client, args).await
        }
        "format_email_content" => {
            services::gmail::settings::format_email_content(&state.client, args).await
        }
        "manage_gmail_labels" => {
            services::gmail::labels::manage_gmail_labels(&state.client, args).await
        }
        "manage_gmail_filters" => {
            services::gmail::organize::manage_gmail_filters(&state.client, args).await
        }
        "manage_gmail_settings" => {
            services::gmail::settings::manage_gmail_settings(&state.client, args).await
        }

        // Drive
        "list_drive_contents" => {
            services::drive::files::list_drive_contents(&state.client, args).await
        }
        "search_drive_files" => {
            services::drive::files::search_drive_files(&state.client, args).await
        }
        "get_drive_file_content" => {
            services::drive::files::get_drive_file_content(&state.client, args).await
        }
        "list_shared_drives" => {
            services::drive::files::list_shared_drives(&state.client, args).await
        }
        "manage_drive_file" => services::drive::files::manage_drive_file(&state.client, args).await,
        "manage_file_permissions" => {
            services::drive::sharing::manage_file_permissions(&state.client, args).await
        }

        // Docs
        "create_document" => services::docs::core::create_document(&state.client, args).await,
        "append_to_document" => services::docs::core::append_to_document(&state.client, args).await,
        "get_document" => services::docs::core::get_document(&state.client, args).await,
        "get_document_structure" => {
            services::docs::core::get_document_structure(&state.client, args).await
        }
        "replace_text_in_document" => {
            services::docs::core::replace_text_in_document(&state.client, args).await
        }
        "insert_text_in_document" => {
            services::docs::core::insert_text_in_document(&state.client, args).await
        }
        "delete_range_in_document" => {
            services::docs::core::delete_range_in_document(&state.client, args).await
        }
        "manage_document_comments" => {
            services::docs::comments::manage_document_comments(&state.client, args).await
        }
        "format_document_range" => {
            services::docs::formatting::format_document_range(&state.client, args).await
        }
        "set_document_style" => {
            services::docs::formatting::set_document_style(&state.client, args).await
        }
        "insert_table_in_document" => {
            services::docs::table_ops::insert_table_in_document(&state.client, args).await
        }
        "find_tables_in_document" => {
            services::docs::table_ops::find_tables_in_document(&state.client, args).await
        }
        "manage_table_structure" => {
            services::docs::table_ops::manage_table_structure(&state.client, args).await
        }

        // Sheets
        "get_spreadsheet" => services::sheets::core::get_spreadsheet(&state.client, args).await,
        "manage_spreadsheet" => {
            services::sheets::core::manage_spreadsheet(&state.client, args).await
        }
        "modify_sheet_values" => {
            services::sheets::core::modify_sheet_values(&state.client, args).await
        }
        "format_sheet" => services::sheets::formatting::format_sheet(&state.client, args).await,
        "create_chart" => services::sheets::charts::create_chart(&state.client, args).await,

        // Slides
        "get_slides" => services::slides::core::get_slides(&state.client, args).await,
        "manage_slides" => services::slides::core::manage_slides(&state.client, args).await,
        "add_slide_content" => {
            services::slides::content::add_slide_content(&state.client, args).await
        }

        // Tasks
        "manage_task_lists" => services::tasks::manage_task_lists(&state.client, args).await,
        "manage_tasks" => services::tasks::manage_tasks(&state.client, args).await,
        "list_tasks" => services::tasks::list_tasks(&state.client, args).await,
        "complete_task" => services::tasks::complete_task(&state.client, args).await,

        other => Err(anyhow::anyhow!("unknown tool: {other}")),
    };

    match result {
        Ok(v) => v,
        Err(e) => json!({ "error": e.to_string() }),
    }
}

/// Translate a parsed JSON-RPC request into a JSON-RPC response payload.
///
/// Why: Same shape used across the trusty-* family — handle init, ping,
/// notifications, tools/list, tools/call.
/// What: Returns `Value::Null` for notifications (suppressed); returns an
/// `{"error": {...}}` map for unknown methods.
/// Test: `handle_message_initialize_returns_server_info`.
pub async fn handle_message(state: AppState, req: Value) -> Value {
    let method = req["method"].as_str().unwrap_or("");
    match method {
        "initialize" => initialize_response("gworkspace-mcp", env!("CARGO_PKG_VERSION"), None),
        "notifications/initialized" | "notifications/cancelled" => Value::Null,
        "ping" => json!({}),
        "rpc.discover" => crate::openrpc::discover_response(),
        "tools/list" => crate::tools::tool_list_response(),
        "tools/call" => {
            let params = &req["params"];
            let name = params["name"].as_str().unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            let args = if args.is_null() { json!({}) } else { args };
            let result = handle_tool_call(&state, name, args).await;
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
/// Why: Single entry point for `bin/gworkspace-mcp.rs`.
/// What: Forwards every parsed `Request` to `handle_message` and wraps the
/// JSON result back into a `Response`.
/// Test: Manual — driven from Claude Code.
pub async fn run_stdio(state: AppState) -> anyhow::Result<()> {
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

    fn make_state() -> AppState {
        // BaseClient::new() reads no env vars when unset and returns Ok,
        // so this works in CI without Google credentials.
        let client = BaseClient::new().expect("construct base client");
        AppState {
            client: Arc::new(client),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_message_initialize_returns_server_info() {
        let state = make_state();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });
        let resp = handle_message(state, req).await;
        assert_eq!(resp["serverInfo"]["name"], "gworkspace-mcp");
        assert!(resp["capabilities"]["tools"].is_object());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_message_tools_list_returns_tools() {
        let state = make_state();
        let req = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let resp = handle_message(state, req).await;
        assert!(resp["tools"].is_array());
        assert!(resp["tools"].as_array().unwrap().len() >= 40);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handle_message_rpc_discover_returns_openrpc_document() {
        let state = make_state();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "rpc.discover",
            "params": {}
        });
        let resp = handle_message(state, req).await;
        assert_eq!(resp["openrpc"], "1.3.2");
        assert!(resp["info"]["title"].is_string());
        let methods = resp["methods"].as_array().expect("methods array");
        assert!(!methods.is_empty(), "methods array must not be empty");
        for m in methods {
            assert!(m["name"].is_string());
            assert!(m["params"].is_array());
            assert!(m["x-google-scopes"].is_array());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_method_returns_error_payload() {
        let state = make_state();
        let req = json!({ "jsonrpc": "2.0", "id": 3, "method": "no/such/method" });
        let resp = handle_message(state, req).await;
        assert_eq!(resp["error"]["code"], error_codes::METHOD_NOT_FOUND);
    }
}
