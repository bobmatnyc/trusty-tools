//! Google Tasks service.
//!
//! Why: Tasks API has two resources (lists and tasks-within-a-list) with
//! identical CRUD shapes — we expose them as two tools.
//! What: `manage_task_lists` covers list-level CRUD; `manage_tasks` covers
//! per-task CRUD plus "complete" and "move".
//! Test: Live only.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::TASKS_API_BASE;
use crate::api::services::{account_of, opt_str, require_str};

/// Convenience wrapper: list tasks from the default tasklist (`@default`).
///
/// Why: The CTO bot and other agents need a single-shot "what's on my
/// list?" tool without learning the action-style `manage_tasks` dispatcher.
/// What: GETs `lists/{tasklist}/tasks?maxResults={n}&showCompleted={b}` —
/// defaults to the user's default list, `max_results=20`,
/// `show_completed=false`. Projects each item to the small shape
/// `{id, title, due, status, notes}` so agents get predictable fields.
/// Test: Live API only; tool-shape covered by `tool_list_response()` test.
pub async fn list_tasks(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let tasklist = opt_str(&args, "tasklist_id").unwrap_or("@default");
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(20);
    let show_completed = args
        .get("show_completed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let url = format!(
        "{TASKS_API_BASE}/lists/{tasklist}/tasks?maxResults={max_results}&showCompleted={show_completed}"
    );
    let raw = client.get(&url, account).await?;

    let items = raw
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let tasks: Vec<Value> = items
        .into_iter()
        .map(|t| {
            json!({
                "id":     t.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "title":  t.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                "due":    t.get("due").and_then(|v| v.as_str()),
                "status": t.get("status").and_then(|v| v.as_str()).unwrap_or("needsAction"),
                "notes":  t.get("notes").and_then(|v| v.as_str()),
            })
        })
        .collect();
    Ok(json!({ "tasks": tasks }))
}

/// Convenience wrapper: mark a single task complete.
///
/// Why: Agents frequently want to tick exactly one task without learning
/// the full `manage_tasks` action enum.
/// What: PATCHes `lists/{tasklist}/tasks/{id}` with
/// `{"status": "completed"}`. Defaults to the `@default` tasklist.
/// Test: Live API only.
pub async fn complete_task(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let tasklist = opt_str(&args, "tasklist_id").unwrap_or("@default");
    let task_id = require_str(&args, "task_id")?;
    let url = format!("{TASKS_API_BASE}/lists/{tasklist}/tasks/{task_id}");
    client
        .patch(&url, json!({ "status": "completed" }), account)
        .await
}

/// Why: Task list CRUD is small enough to share one tool action enum.
/// What: Routes `list|get|create|update|delete` to `users/@me/lists`.
/// Test: Live API.
pub async fn manage_task_lists(client: &BaseClient, args: Value) -> Result<Value> {
    let action = require_str(&args, "action")?;
    let account = account_of(&args);
    match action {
        "list" => {
            let url = format!("{TASKS_API_BASE}/users/@me/lists");
            client.get(&url, account).await
        }
        "get" => {
            let id = require_str(&args, "tasklist_id")?;
            let url = format!("{TASKS_API_BASE}/users/@me/lists/{id}");
            client.get(&url, account).await
        }
        "create" => {
            let title = require_str(&args, "title")?;
            let url = format!("{TASKS_API_BASE}/users/@me/lists");
            client.post(&url, json!({ "title": title }), account).await
        }
        "update" => {
            let id = require_str(&args, "tasklist_id")?;
            let url = format!("{TASKS_API_BASE}/users/@me/lists/{id}");
            let body = args.get("updates").cloned().unwrap_or_else(|| json!({}));
            client.patch(&url, body, account).await
        }
        "delete" => {
            let id = require_str(&args, "tasklist_id")?;
            let url = format!("{TASKS_API_BASE}/users/@me/lists/{id}");
            client.delete(&url, account).await
        }
        other => Err(anyhow!("unknown action for manage_task_lists: {other}")),
    }
}

/// Why: Task CRUD inside a list is the bulk of the Tasks API surface, plus a
/// cross-list search agents frequently need ("find the task about X").
/// What: Routes `list|get|create|update|delete|complete|move|search` to
/// `lists/{id}/tasks`; `search` fans out across every tasklist.
/// Test: `search` filter is unit-tested via `task_matches`; live 200 deferred.
pub async fn manage_tasks(client: &BaseClient, args: Value) -> Result<Value> {
    let action = require_str(&args, "action")?;
    let account = account_of(&args);
    let tasklist = opt_str(&args, "tasklist_id").unwrap_or("@default");
    match action {
        "list" => {
            let url = format!("{TASKS_API_BASE}/lists/{tasklist}/tasks");
            client.get(&url, account).await
        }
        "get" => {
            let id = require_str(&args, "task_id")?;
            let url = format!("{TASKS_API_BASE}/lists/{tasklist}/tasks/{id}");
            client.get(&url, account).await
        }
        "search" => search_tasks(client, account, &args).await,
        "create" => {
            let body = args
                .get("task")
                .cloned()
                .ok_or_else(|| anyhow!("missing 'task' object"))?;
            let url = format!("{TASKS_API_BASE}/lists/{tasklist}/tasks");
            client.post(&url, body, account).await
        }
        "update" => {
            let id = require_str(&args, "task_id")?;
            let body = args.get("updates").cloned().unwrap_or_else(|| json!({}));
            let url = format!("{TASKS_API_BASE}/lists/{tasklist}/tasks/{id}");
            client.patch(&url, body, account).await
        }
        "delete" => {
            let id = require_str(&args, "task_id")?;
            let url = format!("{TASKS_API_BASE}/lists/{tasklist}/tasks/{id}");
            client.delete(&url, account).await
        }
        "complete" => {
            let id = require_str(&args, "task_id")?;
            let body = json!({ "status": "completed" });
            let url = format!("{TASKS_API_BASE}/lists/{tasklist}/tasks/{id}");
            client.patch(&url, body, account).await
        }
        "move" => {
            let id = require_str(&args, "task_id")?;
            let mut url = format!("{TASKS_API_BASE}/lists/{tasklist}/tasks/{id}/move");
            let mut params = Vec::<String>::new();
            if let Some(parent) = opt_str(&args, "parent") {
                params.push(format!("parent={parent}"));
            }
            if let Some(prev) = opt_str(&args, "previous") {
                params.push(format!("previous={prev}"));
            }
            if !params.is_empty() {
                url = format!("{url}?{}", params.join("&"));
            }
            client.post(&url, json!({}), account).await
        }
        other => Err(anyhow!("unknown action for manage_tasks: {other}")),
    }
}

/// Why: The Tasks API has no cross-list search; agents need one call to find a
/// task by keyword regardless of which list holds it.
/// What: Lists every tasklist (fully paginated), then lists each list's tasks
/// (also fully paginated) and keeps those whose title or notes contain
/// `query` (case-insensitive). Each hit is annotated with its owning
/// `tasklist_id`/`tasklist_title`.
/// Test: The per-task predicate is unit-tested via `task_matches`; the
/// pagination termination condition via `next_page_token_present_and_absent`.
async fn search_tasks(client: &BaseClient, account: Option<&str>, args: &Value) -> Result<Value> {
    let query = require_str(args, "query")?;
    let needle = query.to_ascii_lowercase();
    let show_completed = args
        .get("show_completed")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let list_items = fetch_all_items(
        client,
        account,
        &format!("{TASKS_API_BASE}/users/@me/lists?maxResults=100"),
    )
    .await?;

    // Sequential fan-out: a user has few tasklists (typically < 10), so this
    // keeps the `&client` borrow trivial and avoids a `futures` dependency
    // while remaining well within any practical latency budget.
    let mut results = Vec::<Value>::new();
    for list in &list_items {
        let Some(list_id) = list.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let list_title = list.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let tasks_url = format!(
            "{TASKS_API_BASE}/lists/{list_id}/tasks?showCompleted={show_completed}&showHidden=true&maxResults=100"
        );
        let items = fetch_all_items(client, account, &tasks_url).await?;
        for task in items {
            if task_matches(&task, &needle) {
                results.push(json!({
                    "tasklist_id": list_id,
                    "tasklist_title": list_title,
                    "id": task.get("id"),
                    "title": task.get("title"),
                    "notes": task.get("notes"),
                    "due": task.get("due"),
                    "status": task.get("status"),
                }));
            }
        }
    }
    Ok(json!({ "query": query, "count": results.len(), "results": results }))
}

/// Fetch every page of a paginated Tasks API list endpoint, concatenating
/// each page's `items` array.
///
/// Why: Both `/users/@me/lists` and `lists/{id}/tasks` paginate via
/// `nextPageToken`; a single unpaged GET silently truncates results once an
/// account has many tasklists, or a list has many tasks — a false negative
/// that defeats the point of a cross-list `search`.
/// What: GETs `base_url` (which already carries its own query string),
/// collects `items`, and follows `nextPageToken` by appending
/// `&pageToken=...` until the API stops returning one.
/// Test: The termination predicate is unit-tested via `next_page_token`;
/// the loop itself is live-only (network).
async fn fetch_all_items(
    client: &BaseClient,
    account: Option<&str>,
    base_url: &str,
) -> Result<Vec<Value>> {
    let mut items = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let url = match &page_token {
            Some(tok) => format!("{base_url}&pageToken={tok}"),
            None => base_url.to_string(),
        };
        let page = client.get(&url, account).await?;
        if let Some(arr) = page.get("items").and_then(|v| v.as_array()) {
            items.extend(arr.iter().cloned());
        }
        page_token = next_page_token(&page);
        if page_token.is_none() {
            break;
        }
    }
    Ok(items)
}

/// Extract the next-page continuation token from a Tasks API list response.
///
/// Why: Isolating the termination condition as a pure function lets the
/// pagination loop's stopping behavior be unit-tested without a live HTTP
/// round-trip.
/// What: Returns `Some(token)` when `nextPageToken` is present and non-empty,
/// else `None`.
/// Test: `next_page_token_present_and_absent` below.
fn next_page_token(page: &Value) -> Option<String> {
    page.get("nextPageToken")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Why: The cross-list search filter must be pure to be unit-testable offline.
/// What: Returns true when the (already-lowercased) `needle` is a substring of
/// the task's title or notes.
/// Test: `task_matches_title_and_notes` below.
fn task_matches(task: &Value, needle_lower: &str) -> bool {
    let title = task.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let notes = task.get("notes").and_then(|v| v.as_str()).unwrap_or("");
    title.to_ascii_lowercase().contains(needle_lower)
        || notes.to_ascii_lowercase().contains(needle_lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_matches_title_and_notes() {
        let t = json!({ "title": "Ship the Release", "notes": "coordinate with QA" });
        // Case-insensitive substring on the title.
        assert!(task_matches(&t, "release"));
        // Match in the notes field.
        assert!(task_matches(&t, "qa"));
        // No match.
        assert!(!task_matches(&t, "invoice"));
        // Missing fields must not panic and must not match.
        let empty = json!({});
        assert!(!task_matches(&empty, "anything"));
    }

    #[test]
    fn next_page_token_present_and_absent() {
        let with_token = json!({ "items": [], "nextPageToken": "abc123" });
        assert_eq!(next_page_token(&with_token).as_deref(), Some("abc123"));

        let without_token = json!({ "items": [] });
        assert_eq!(next_page_token(&without_token), None);

        // An empty-string token must be treated as "no more pages", not as
        // a token to loop forever on.
        let empty_token = json!({ "items": [], "nextPageToken": "" });
        assert_eq!(next_page_token(&empty_token), None);
    }
}
