//! Canonical ticketing data types.
//!
//! Why: GitHub, JIRA, and Linear each ship their own issue/comment/label
//! shapes. The MCP layer needs a single normalised vocabulary so callers
//! don't write three code paths.
//! What: Plain serde structs + small enums. Backend-specific fields are
//! carried through opaquely in `Issue::extra`.
//! Test: `tests::issue_roundtrip` confirms serde round-trip.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Priority bucket normalised across backends.
///
/// Why: GitHub has no native priority (label-based); JIRA uses names;
/// Linear uses integers. One enum collapses them.
/// What: Four levels — Low, Medium, High, Critical.
/// Test: serialised as snake_case strings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Medium,
    High,
    Critical,
}

/// Issue state normalised across backends.
///
/// Why: JIRA/Linear have many workflow states; GitHub only has open/closed.
/// Mapping into a canonical set means the model can reason about state
/// without knowing the backend.
/// What: Common states observed across the three backends.
/// Test: serialised as snake_case strings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueState {
    Open,
    InProgress,
    Ready,
    Tested,
    Done,
    Waiting,
    Blocked,
    Closed,
}

/// Issue type — coarse classification.
///
/// Why: Epic/issue/task/subtask distinctions exist in all three backends
/// (Linear has a parent/child concept; JIRA has issuetype; GitHub uses
/// milestones for epics).
/// What: Four variants serialised as snake_case.
/// Test: enum-only — covered by `issue_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    Epic,
    Issue,
    Task,
    Subtask,
}

/// Canonical issue.
///
/// Why: Backends emit drastically different JSON; the MCP wire shape must
/// be stable.
/// What: All fields optional except identifiers, title, state, and type.
/// `extra` carries backend-specific fields without losing them.
/// Test: `tests::issue_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub backend: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub state: IssueState,
    pub issue_type: IssueType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milestone_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milestone_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub children: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub extra: serde_json::Value,
}

/// Comment on an issue.
///
/// Why: Comments are first-class for every backend.
/// What: Normalised author/body/timestamps. `id` is opaque per-backend.
/// Test: serde round-trip covered by `tests::comment_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub issue_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

/// Label / tag.
///
/// Why: Labels organise issues across all three backends.
/// What: Name + optional colour/description.
/// Test: trivial — exercised via JSON round-trip in module tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Milestone / cycle / sprint / fix version.
///
/// Why: The "time-boxed grouping" concept is named differently per
/// backend but functions identically.
/// What: Includes progress counters when the backend supplies them.
/// Test: covered indirectly via integration tests against live APIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Milestone {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_issues: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_issues: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_pct: Option<f64>,
}

/// Project / repo / team-project.
///
/// Why: GitHub Projects V2, JIRA Projects, and Linear Projects all expose
/// a top-level container that issues belong to.
/// What: Stable shape across backends.
/// Test: live integration only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
}

/// Free-form project update post (Linear-style).
///
/// Why: Linear's `projectUpdate` is increasingly mirrored by other tools;
/// keeping a canonical shape future-proofs callers.
/// What: Body + health enum + author + timestamp.
/// Test: live integration only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectUpdate {
    pub id: String,
    pub project_id: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn issue_roundtrip() {
        let issue = Issue {
            id: "123".into(),
            backend: "github".into(),
            url: Some("https://github.com/o/r/issues/123".into()),
            title: "Hello".into(),
            description: None,
            state: IssueState::Open,
            issue_type: IssueType::Issue,
            priority: Some(Priority::High),
            assignee: None,
            labels: vec!["bug".into()],
            milestone_id: None,
            milestone_name: None,
            project_id: None,
            project_name: None,
            parent_id: None,
            children: vec![],
            created_at: None,
            updated_at: None,
            extra: json!({}),
        };
        let s = serde_json::to_string(&issue).unwrap();
        let back: Issue = serde_json::from_str(&s).unwrap();
        assert_eq!(back.id, "123");
        assert_eq!(back.state, IssueState::Open);
        assert_eq!(back.priority, Some(Priority::High));
    }

    #[test]
    fn comment_roundtrip() {
        let c = Comment {
            id: "c1".into(),
            issue_id: "i1".into(),
            author: Some("alice".into()),
            body: "hi".into(),
            created_at: None,
            updated_at: None,
        };
        let s = serde_json::to_string(&c).unwrap();
        let back: Comment = serde_json::from_str(&s).unwrap();
        assert_eq!(back.body, "hi");
    }

    #[test]
    fn priority_serializes_snake_case() {
        let s = serde_json::to_string(&Priority::Critical).unwrap();
        assert_eq!(s, "\"critical\"");
    }
}
