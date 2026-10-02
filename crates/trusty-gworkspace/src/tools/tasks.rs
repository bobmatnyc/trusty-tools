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
            // #8629: `title` also renames on update; it was silently ignored there.
            "title": { "type": "string", "description": "List title (create; update renames). Merged with 'updates'; a different value in both is an error." },
            "updates": { "type": "object", "description": "Raw patch body (update). Merged with the flat 'title'." },
        }),
        &["action"],
    ));
    // #8629: re-advertise the flat task fields (the Python gworkspace-mcp
    // shape); the handler merges them with the `task` / `updates` object.
    tools.push(tool(
        "manage_tasks",
        "CRUD, complete/move, get one, or search tasks within Google Tasks lists. \
         create/update take task fields flat (title, notes, due, status, completed), \
         as a 'task' (create) or 'updates' (update) object, or both: the two are merged, \
         and a field set in both with different values is an error.",
        json!({
            "account": account_schema(),
            "action": action_enum(&["list", "get", "create", "update", "delete", "complete", "move", "search"]),
            "tasklist_id": { "type": "string", "description": "Task list ID. Defaults to '@default'." },
            "task_id": { "type": "string", "description": "Required for get/update/delete/complete/move." },
            "title": { "type": "string", "description": "Task title (create/update)." },
            "notes": { "type": "string", "description": "Task notes/description (create/update)." },
            "due": { "type": "string", "description": "Due date, RFC3339 (e.g. 2026-09-28T00:00:00Z) (create/update)." },
            "status": { "type": "string", "enum": ["needsAction", "completed"], "description": "Task status (create/update)." },
            "completed": { "type": "string", "description": "Completion time, RFC3339 (create/update)." },
            "task": { "type": "object", "description": "Raw Task resource (create). Merged with the flat fields." },
            "updates": { "type": "object", "description": "Raw patch body (update). Merged with the flat fields." },
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
