//! Google Tasks tool definitions.
//!
//! Why: Groups task-list and task CRUD/complete tools.
//! What: Appends the Tasks tool group to the shared registry vector.
//! Test: Covered via `tool_list_response()` in `tools::tests`.

use super::schema::{account_schema, action_enum, tool};
use serde_json::{Value, json};

/// Append the Tasks tool group to the registry.
///
/// Why: Keeps Tasks-related tools colocated.
/// What: Pushes the task-list/task manage, list, and complete tools.
/// Test: Covered via `tool_list_response()` in `tools::tests`.
pub(super) fn append(tools: &mut Vec<Value>) {
    tools.push(tool(
        "manage_task_lists",
        "CRUD Google Tasks lists (list, get one by id, create, update, delete).",
        json!({
            "account": account_schema(),
            "action": action_enum(&["list", "get", "create", "update", "delete"]),
            "tasklist_id": { "type": "string", "description": "Required for get/update/delete." },
            "title": { "type": "string" },
            "updates": { "type": "object" },
        }),
        &["action"],
    ));
    tools.push(tool(
        "manage_tasks",
        "CRUD, complete/move, get one, or search tasks within Google Tasks lists.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["list", "get", "create", "update", "delete", "complete", "move", "search"]),
            "tasklist_id": { "type": "string" },
            "task_id": { "type": "string", "description": "Required for get/update/delete/complete/move." },
            "task": { "type": "object" },
            "updates": { "type": "object" },
            "parent": { "type": "string" },
            "previous": { "type": "string" },
            "query": { "type": "string", "description": "search: case-insensitive substring matched against task title/notes across all lists." },
            "show_completed": { "type": "boolean", "description": "search: include completed tasks (default true)." },
        }),
        &["action"],
    ));
    tools.push(tool(
        "list_tasks",
        "List tasks from the default Google Tasks list (id, title, due, status, notes).",
        json!({
            "account": account_schema(),
            "tasklist_id": {
                "type": "string",
                "description": "Optional task list ID; defaults to the user's @default list.",
            },
            "max_results": {
                "type": "integer",
                "description": "Maximum number of tasks to return (default 20).",
                "minimum": 1,
                "maximum": 100,
            },
            "show_completed": {
                "type": "boolean",
                "description": "Include completed tasks (default false).",
            },
        }),
        &[],
    ));
    tools.push(tool(
        "complete_task",
        "Mark a single Google Task as completed.",
        json!({
            "account": account_schema(),
            "tasklist_id": {
                "type": "string",
                "description": "Optional task list ID; defaults to @default.",
            },
            "task_id": {
                "type": "string",
                "description": "The task ID (from list_tasks).",
            },
        }),
        &["task_id"],
    ));
}
