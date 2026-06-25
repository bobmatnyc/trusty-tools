//! MCP tool schema definitions for the three task tools (issue #1722).
//!
//! Why: `definitions.rs` was at 491 SLOC before Phase 4; adding the task tool
//! JSON inline would exceed the 500-SLOC production cap. Extracting to a
//! sibling module keeps each file focused (task tools are a coherent group)
//! and allows the cap to be honoured without splitting `definitions.rs` itself.
//! What: exports `task_tool_definitions(has_default)` — returns a `Vec<Value>`
//! containing the three task tool schemas ready to be spliced into the main
//! tools array in `tool_definitions_with`.
//! Test: `tool_definitions_lists_all_tools` in `tools::tests` verifies the
//! names appear in the merged list; `every_tool_has_scopes` in `openrpc::tests`
//! verifies scopes are declared.

use serde_json::{json, Value};

/// Build the three task tool schemas conditioned on whether a default palace
/// is configured.
///
/// Why: follows the same `has_default` pattern as every other tool group so
/// the `palace` argument moves out of `required` when the server is bound to
/// a single palace via `--palace`.
/// What: returns `[task_add, task_list, task_complete]` as `Vec<Value>`.
/// Test: spliced into `tool_definitions_with` and covered by
/// `tool_definitions_lists_all_tools` in `tools::tests`.
pub(super) fn task_tool_definitions(has_default: bool) -> Vec<Value> {
    let task_add_required: Vec<&str> = if has_default {
        vec!["content"]
    } else {
        vec!["palace", "content"]
    };
    let task_complete_required: Vec<&str> = if has_default {
        vec!["drawer_id"]
    } else {
        vec!["palace", "drawer_id"]
    };
    let task_list_required: Vec<&str> = if has_default { vec![] } else { vec!["palace"] };

    vec![
        json!({
            "name": "task_add",
            "description": "Create a Task drawer in a palace (spec-001 issue #1722). Task drawers are protected from the dream cycle — never evicted or consolidated regardless of age or importance. Use for goals, milestones, or any long-lived context the application must retain across sessions. Returns { drawer_id, palace, status, drawer_type }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":  {"type": "string", "description": "Palace ID (optional if server started with --palace)"},
                    "content": {"type": "string", "description": "Task description or goal."},
                    "room":    {"type": "string", "description": "Room type (optional, defaults to General)."},
                    "tags":    {"type": "array", "items": {"type": "string"}}
                },
                "required": task_add_required,
            }
        }),
        json!({
            "name": "task_list",
            "description": "List Task drawers in a palace (spec-001 issue #1722). Returns only open (incomplete) tasks by default; pass include_completed=true to include tasks with a completed_at timestamp. Returns { palace, tasks: [ { drawer_id, content, importance, tags, created_at, completed_at, drawer_type } ] }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":            {"type": "string", "description": "Palace ID (optional if server started with --palace)"},
                    "include_completed": {"type": "boolean", "default": false, "description": "Include tasks with a completed_at timestamp in the results."}
                },
                "required": task_list_required,
            }
        }),
        json!({
            "name": "task_complete",
            "description": "Mark a Task drawer as completed by setting its completed_at timestamp (spec-001 issue #1722). Completed tasks are no longer returned by the default task_list view but are never auto-evicted — they remain eligible for manual cleanup only. Errors if drawer_id does not exist or is not a Task drawer. Returns { palace, drawer_id, completed_at, status }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "palace":    {"type": "string", "description": "Palace ID (optional if server started with --palace)"},
                    "drawer_id": {"type": "string", "description": "UUID of the Task drawer to mark completed."}
                },
                "required": task_complete_required,
            }
        }),
    ]
}
