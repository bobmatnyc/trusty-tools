//! MCP `tools/list` schema for trusty-tickets.
//!
//! Why: Claude Code needs a machine-readable contract per tool.
//! What: `tool_list_response()` returns the full schema bundle.
//! Test: `tool_list_has_expected_count` asserts >= 30 tools and unique names.

use serde_json::{Value, json};

fn backend_schema() -> Value {
    json!({
        "type": "string",
        "description": "Backend to use: github, jira, linear. Defaults to configured default.",
    })
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
        }
    })
}

/// Build the full `tools/list` response.
///
/// Why: Single source of truth grep-able by tool name.
/// What: ~30 tools across issues, comments, labels, milestones,
/// projects/epics, workflow, and meta.
/// Test: `tool_list_has_expected_count`.
pub fn tool_list_response() -> Value {
    let mut tools = Vec::<Value>::new();

    // ----- Issues -----
    tools.push(tool(
        "create_issue",
        "Create a new issue/ticket on the selected backend.",
        json!({
            "backend": backend_schema(),
            "title": { "type": "string" },
            "description": { "type": "string" },
            "priority": { "type": "string", "enum": ["low", "medium", "high", "critical"] },
            "assignee": { "type": "string" },
            "labels": { "type": "array", "items": { "type": "string" } },
            "milestone_id": { "type": "string" },
            "project_id": { "type": "string" },
            "parent_id": { "type": "string" },
            "issue_type": { "type": "string", "enum": ["epic", "issue", "task", "subtask"] },
        }),
        &["title"],
    ));
    tools.push(tool(
        "get_issue",
        "Get a single issue by ID.",
        json!({
            "backend": backend_schema(),
            "issue_id": { "type": "string" },
        }),
        &["issue_id"],
    ));
    tools.push(tool(
        "update_issue",
        "Update an existing issue.",
        json!({
            "backend": backend_schema(),
            "issue_id": { "type": "string" },
            "title": { "type": "string" },
            "description": { "type": "string" },
            "state": { "type": "string" },
            "priority": { "type": "string", "enum": ["low", "medium", "high", "critical"] },
            "assignee": { "type": "string" },
            "labels": { "type": "array", "items": { "type": "string" } },
            "milestone_id": { "type": "string" },
        }),
        &["issue_id"],
    ));
    tools.push(tool(
        "close_issue",
        "Close (resolve) an issue.",
        json!({
            "backend": backend_schema(),
            "issue_id": { "type": "string" },
            "comment": { "type": "string", "description": "Optional closing comment." },
        }),
        &["issue_id"],
    ));
    tools.push(tool(
        "reopen_issue",
        "Reopen a previously closed issue.",
        json!({
            "backend": backend_schema(),
            "issue_id": { "type": "string" },
        }),
        &["issue_id"],
    ));
    tools.push(tool(
        "list_issues",
        "List issues with optional filters.",
        json!({
            "backend": backend_schema(),
            "project_id": { "type": "string" },
            "state": { "type": "string" },
            "assignee": { "type": "string" },
            "labels": { "type": "array", "items": { "type": "string" } },
            "limit": { "type": "integer", "default": 20 },
            "offset": { "type": "integer", "default": 0 },
        }),
        &[],
    ));
    tools.push(tool(
        "search_issues",
        "Search issues with a free-text query and optional filters.",
        json!({
            "backend": backend_schema(),
            "query": { "type": "string" },
            "state": { "type": "string" },
            "priority": { "type": "string" },
            "labels": { "type": "array", "items": { "type": "string" } },
            "assignee": { "type": "string" },
            "project_id": { "type": "string" },
            "milestone_id": { "type": "string" },
            "limit": { "type": "integer", "default": 10 },
            "offset": { "type": "integer", "default": 0 },
        }),
        &[],
    ));

    // ----- Comments -----
    tools.push(tool(
        "add_comment",
        "Add a comment to an issue.",
        json!({
            "backend": backend_schema(),
            "issue_id": { "type": "string" },
            "body": { "type": "string" },
        }),
        &["issue_id", "body"],
    ));
    tools.push(tool(
        "list_comments",
        "List comments on an issue.",
        json!({
            "backend": backend_schema(),
            "issue_id": { "type": "string" },
        }),
        &["issue_id"],
    ));
    tools.push(tool(
        "update_comment",
        "Update an existing comment.",
        json!({
            "backend": backend_schema(),
            "issue_id": { "type": "string" },
            "comment_id": { "type": "string" },
            "body": { "type": "string" },
        }),
        &["issue_id", "comment_id", "body"],
    ));
    tools.push(tool(
        "delete_comment",
        "Delete a comment.",
        json!({
            "backend": backend_schema(),
            "issue_id": { "type": "string" },
            "comment_id": { "type": "string" },
        }),
        &["issue_id", "comment_id"],
    ));

    // ----- Labels -----
    tools.push(tool(
        "list_labels",
        "List all labels available on this backend.",
        json!({ "backend": backend_schema() }),
        &[],
    ));
    tools.push(tool(
        "create_label",
        "Create a new label.",
        json!({
            "backend": backend_schema(),
            "name": { "type": "string" },
            "color": { "type": "string", "description": "Hex without #." },
            "description": { "type": "string" },
        }),
        &["name"],
    ));
    tools.push(tool(
        "add_labels",
        "Attach labels to an issue.",
        json!({
            "backend": backend_schema(),
            "issue_id": { "type": "string" },
            "labels": { "type": "array", "items": { "type": "string" } },
        }),
        &["issue_id", "labels"],
    ));
    tools.push(tool(
        "remove_labels",
        "Detach labels from an issue.",
        json!({
            "backend": backend_schema(),
            "issue_id": { "type": "string" },
            "labels": { "type": "array", "items": { "type": "string" } },
        }),
        &["issue_id", "labels"],
    ));

    // ----- Milestones / cycles / sprints -----
    tools.push(tool(
        "list_milestones",
        "List milestones / cycles / fix-versions for the active project.",
        json!({ "backend": backend_schema() }),
        &[],
    ));
    tools.push(tool(
        "create_milestone",
        "Create a new milestone / cycle.",
        json!({
            "backend": backend_schema(),
            "name": { "type": "string" },
            "description": { "type": "string" },
            "due_date": { "type": "string", "description": "ISO 8601 date." },
        }),
        &["name"],
    ));
    tools.push(tool(
        "close_milestone",
        "Close / complete a milestone.",
        json!({
            "backend": backend_schema(),
            "milestone_id": { "type": "string" },
        }),
        &["milestone_id"],
    ));
    tools.push(tool(
        "get_milestone_issues",
        "List the issues belonging to a milestone.",
        json!({
            "backend": backend_schema(),
            "milestone_id": { "type": "string" },
        }),
        &["milestone_id"],
    ));

    // ----- Projects / epics -----
    tools.push(tool(
        "list_projects",
        "List projects / GitHub Projects V2 / JIRA projects / Linear projects.",
        json!({ "backend": backend_schema() }),
        &[],
    ));
    tools.push(tool(
        "get_project",
        "Get a project by ID.",
        json!({
            "backend": backend_schema(),
            "project_id": { "type": "string" },
        }),
        &["project_id"],
    ));
    tools.push(tool(
        "list_epics",
        "List epics. GitHub: milestones-as-epics. JIRA: issuetype=Epic. Linear: issues with children.",
        json!({ "backend": backend_schema() }),
        &[],
    ));
    tools.push(tool(
        "get_epic_issues",
        "List issues that belong to an epic.",
        json!({
            "backend": backend_schema(),
            "epic_id": { "type": "string" },
        }),
        &["epic_id"],
    ));
    tools.push(tool(
        "create_project_update",
        "Post a status update on a project (Linear-only currently).",
        json!({
            "backend": backend_schema(),
            "project_id": { "type": "string" },
            "body": { "type": "string" },
            "health": { "type": "string", "enum": ["on_track", "at_risk", "off_track", "complete", "inactive"] },
        }),
        &["project_id", "body"],
    ));
    tools.push(tool(
        "list_project_updates",
        "List status updates on a project (Linear-only currently).",
        json!({
            "backend": backend_schema(),
            "project_id": { "type": "string" },
        }),
        &["project_id"],
    ));

    // ----- Workflow -----
    tools.push(tool(
        "list_states",
        "List available workflow states.",
        json!({ "backend": backend_schema() }),
        &[],
    ));
    tools.push(tool(
        "transition_issue",
        "Move an issue to a different workflow state.",
        json!({
            "backend": backend_schema(),
            "issue_id": { "type": "string" },
            "state": { "type": "string" },
        }),
        &["issue_id", "state"],
    ));
    tools.push(tool(
        "assign_issue",
        "Assign an issue to a user.",
        json!({
            "backend": backend_schema(),
            "issue_id": { "type": "string" },
            "assignee": { "type": "string" },
        }),
        &["issue_id", "assignee"],
    ));

    // ----- Meta -----
    tools.push(tool(
        "list_backends",
        "List configured ticketing backends and which one is default.",
        json!({}),
        &[],
    ));
    tools.push(tool(
        "list_teams",
        "List teams / orgs visible to the selected backend.",
        json!({ "backend": backend_schema() }),
        &[],
    ));

    json!({ "tools": tools })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_has_expected_count() {
        let v = tool_list_response();
        let tools = v["tools"].as_array().expect("tools array");
        assert!(
            tools.len() >= 30,
            "expected >= 30 tools, got {}",
            tools.len()
        );
        for t in tools {
            assert!(t["name"].is_string(), "every tool has a name");
            assert!(t["description"].is_string(), "every tool has a description");
            assert_eq!(
                t["inputSchema"]["type"], "object",
                "every tool has object inputSchema"
            );
        }
    }

    #[test]
    fn every_tool_name_is_unique() {
        use std::collections::HashSet;
        let v = tool_list_response();
        let mut seen = HashSet::new();
        for t in v["tools"].as_array().unwrap() {
            let name = t["name"].as_str().unwrap().to_string();
            assert!(seen.insert(name.clone()), "duplicate tool: {name}");
        }
    }
}
