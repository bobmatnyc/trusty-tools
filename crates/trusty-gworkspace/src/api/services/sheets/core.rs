//! Sheets v4 core operations.
//!
//! Why: Get spreadsheet metadata; create spreadsheets; read/write cell values.
//! What: Three tool functions matching the Python service surface (formatting
//! and charts live in sibling `formatting`/`charts` modules).
//! Test: Live only.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::SHEETS_API_BASE;
use crate::api::services::{account_of, opt_str, require_str};

/// Why: Reading sheet metadata + values is the basis of every Sheets workflow.
/// What: GETs `/spreadsheets/{id}` with optional `ranges` and `includeGridData`.
/// Test: Live API.
pub async fn get_spreadsheet(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "spreadsheet_id")?;
    let include_grid = args
        .get("include_grid_data")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let url = format!("{SHEETS_API_BASE}/spreadsheets/{id}?includeGridData={include_grid}");
    client.get(&url, account).await
}

/// Why: Spreadsheet/sheet-level CRUD (create, add/delete sheet, rename) shares one tool.
/// What: Dispatches `create|add_sheet|delete_sheet|rename_sheet` to Sheets v4 batchUpdate.
/// Test: Live API.
pub async fn manage_spreadsheet(client: &BaseClient, args: Value) -> Result<Value> {
    let action = require_str(&args, "action")?;
    let account = account_of(&args);
    match action {
        "create" => {
            let title = opt_str(&args, "title").unwrap_or("Untitled Spreadsheet");
            let body = json!({ "properties": { "title": title } });
            let url = format!("{SHEETS_API_BASE}/spreadsheets");
            client.post(&url, body, account).await
        }
        "add_sheet" => {
            let id = require_str(&args, "spreadsheet_id")?;
            let title = require_str(&args, "title")?;
            let body = json!({
                "requests": [{
                    "addSheet": { "properties": { "title": title } }
                }]
            });
            let url = format!("{SHEETS_API_BASE}/spreadsheets/{id}:batchUpdate");
            client.post(&url, body, account).await
        }
        "delete_sheet" => {
            let id = require_str(&args, "spreadsheet_id")?;
            let sheet_id = args
                .get("sheet_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow!("missing sheet_id"))?;
            let body = json!({
                "requests": [{ "deleteSheet": { "sheetId": sheet_id } }]
            });
            let url = format!("{SHEETS_API_BASE}/spreadsheets/{id}:batchUpdate");
            client.post(&url, body, account).await
        }
        other => Err(anyhow!("unknown action for manage_spreadsheet: {other}")),
    }
}

/// Why: Cell-value writes are the most common Sheets mutation; one tool handles all modes.
/// What: Routes `update|append|clear` to `values:update|append|clear` endpoints.
/// Test: Live API.
pub async fn modify_sheet_values(client: &BaseClient, args: Value) -> Result<Value> {
    let action = opt_str(&args, "action").unwrap_or("write");
    let account = account_of(&args);
    let id = require_str(&args, "spreadsheet_id")?;
    let range = require_str(&args, "range")?;
    match action {
        "read" => {
            let url = format!(
                "{SHEETS_API_BASE}/spreadsheets/{id}/values/{}",
                crate::api::services::drive::files::encode(range)
            );
            client.get(&url, account).await
        }
        "write" | "update" => {
            let values = args
                .get("values")
                .cloned()
                .ok_or_else(|| anyhow!("missing 'values' (2-D array)"))?;
            let body = json!({ "values": values });
            let url = format!(
                "{SHEETS_API_BASE}/spreadsheets/{id}/values/{}?valueInputOption=USER_ENTERED",
                crate::api::services::drive::files::encode(range)
            );
            client.put(&url, body, account).await
        }
        "append" => {
            let values = args
                .get("values")
                .cloned()
                .ok_or_else(|| anyhow!("missing 'values'"))?;
            let body = json!({ "values": values });
            let url = format!(
                "{SHEETS_API_BASE}/spreadsheets/{id}/values/{}:append?valueInputOption=USER_ENTERED",
                crate::api::services::drive::files::encode(range)
            );
            client.post(&url, body, account).await
        }
        "clear" => {
            let url = format!(
                "{SHEETS_API_BASE}/spreadsheets/{id}/values/{}:clear",
                crate::api::services::drive::files::encode(range)
            );
            client.post(&url, json!({}), account).await
        }
        other => Err(anyhow!("unknown action for modify_sheet_values: {other}")),
    }
}
