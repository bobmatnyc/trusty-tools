//! Linear backend — shared types, constants, and parse helpers.
//!
//! Why: Separates data-shaping logic from HTTP transport and the Backend impl
//! so each submodule stays under the 500-SLOC cap.
//! What: Defines `LinearBackend`, priority/state conversion helpers, the
//! `parse_issue` function, and the `ISSUE_FIELDS` GraphQL fragment.
//! Test: `super::backend::tests` exercises all helpers.

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde_json::Value;
use std::sync::Mutex;

use crate::tickets::api::models::*;

pub(super) const GRAPHQL_URL: &str = "https://api.linear.app/graphql";
pub(super) const USER_AGENT: &str = "trusty-tickets/0.1";

/// Linear GraphQL backend.
///
/// Why: API key + lazily-resolved team_id + HTTP client.
/// What: `team_id` is resolved on first use from `team_key`, cached in a
/// `Mutex<Option<String>>`.
/// Test: `tests::priority_to_int_mapping`.
pub struct LinearBackend {
    pub(super) api_key: String,
    pub(super) team_key: Option<String>,
    pub(super) team_id: Mutex<Option<String>>,
    pub(super) http: Client,
}

/// Map a priority name string to the Linear integer priority code.
///
/// Why: Linear GraphQL takes integer priorities (1=urgent … 4=low).
/// What: Returns 1–4 for known priorities, 0 for unknown/no-priority.
/// Test: `tests::priority_to_int_mapping`.
pub(super) fn priority_to_int(p: &str) -> i32 {
    match p {
        "critical" => 1,
        "high" => 2,
        "medium" => 3,
        "low" => 4,
        _ => 0,
    }
}

/// Map a Linear integer priority code to the canonical `Priority`.
///
/// Why: Converts the raw integer from the GraphQL response to the shared enum.
/// What: Returns `Some(Priority)` for 1–4, `None` for 0 / unrecognised.
/// Test: `tests::int_to_priority_mapping`.
pub(super) fn int_to_priority(n: i64) -> Option<Priority> {
    match n {
        1 => Some(Priority::Critical),
        2 => Some(Priority::High),
        3 => Some(Priority::Medium),
        4 => Some(Priority::Low),
        _ => None,
    }
}

/// Map a Linear state name to the canonical `IssueState`.
///
/// Why: Normalises the free-form Linear state names to the small canonical set.
/// What: Case-insensitive match on well-known names; falls through to `Open`.
/// Test: `tests::state_mapping`.
pub(super) fn state_from_name(s: &str) -> IssueState {
    let l = s.to_lowercase();
    match l.as_str() {
        "in progress" | "in_progress" => IssueState::InProgress,
        "ready" => IssueState::Ready,
        "tested" => IssueState::Tested,
        "done" | "completed" => IssueState::Done,
        "blocked" => IssueState::Blocked,
        "waiting" => IssueState::Waiting,
        "canceled" | "cancelled" | "closed" => IssueState::Closed,
        _ => IssueState::Open,
    }
}

/// Convert a Linear issue GraphQL node into the canonical `Issue`.
///
/// Why: Decouples JSON shape knowledge from the Backend impl methods.
/// What: Extracts all standard issue fields from the raw Linear GraphQL
/// response node, resolving nested objects (state, assignee, labels, etc.).
/// Test: `tests::state_mapping` (indirectly via `state_from_name`).
pub(super) fn parse_issue(node: &Value) -> Issue {
    let id = node["id"].as_str().unwrap_or("").to_string();
    let state_name = node["state"]["name"].as_str().unwrap_or("Backlog");
    let labels = node["labels"]["nodes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| l.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let assignee = node["assignee"]["name"].as_str().map(String::from);
    let priority = node["priority"].as_i64().and_then(int_to_priority);
    Issue {
        id,
        backend: "linear".into(),
        url: node["url"].as_str().map(String::from),
        title: node["title"].as_str().unwrap_or("").to_string(),
        description: node["description"].as_str().map(String::from),
        state: state_from_name(state_name),
        issue_type: IssueType::Issue,
        priority,
        assignee,
        labels,
        milestone_id: node["cycle"]["id"].as_str().map(String::from),
        milestone_name: node["cycle"]["name"].as_str().map(String::from),
        project_id: node["project"]["id"].as_str().map(String::from),
        project_name: node["project"]["name"].as_str().map(String::from),
        parent_id: node["parent"]["id"].as_str().map(String::from),
        children: node["children"]["nodes"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.get("id").and_then(|v| v.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        created_at: node["createdAt"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        updated_at: node["updatedAt"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc)),
        extra: node.clone(),
    }
}

/// GraphQL field fragment for issue nodes.
///
/// Why: Reused in every query/mutation that returns an issue — prevents
/// drift between create/get/update/list responses.
/// What: Inline selection set string; interpolated into GraphQL queries.
/// Test: shape validated by every Backend method that parses a Linear issue.
pub(super) const ISSUE_FIELDS: &str = r#"
    id identifier title description priority url createdAt updatedAt
    state { name }
    assignee { name }
    labels { nodes { name } }
    cycle { id name }
    project { id name }
    parent { id }
    children { nodes { id } }
"#;
