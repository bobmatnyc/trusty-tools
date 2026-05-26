//! Backend trait + param structs.
//!
//! Why: GitHub/JIRA/Linear share the same conceptual operations but speak
//! different HTTP and authentication dialects. A single trait gives the
//! MCP layer one entry point per operation.
//! What: Async-trait `Backend` plus the parameter bundles used by the
//! dispatcher in `client.rs`.
//! Test: Each backend impl has its own module-level tests; trait shape
//! is verified by `cargo check`.

use crate::tickets::api::models::*;
use anyhow::Result;
use async_trait::async_trait;

pub mod github;
pub mod jira;
pub mod linear;

/// Parameters for `create_issue`.
///
/// Why: Keep the trait method signatures readable.
/// What: Plain owned struct; optional fields use `Option`.
/// Test: built from JSON in `tools.rs` dispatch.
#[derive(Debug, Clone, Default)]
pub struct CreateIssueParams {
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub milestone_id: Option<String>,
    pub project_id: Option<String>,
    pub parent_id: Option<String>,
    pub issue_type: Option<String>,
}

/// Parameters for `update_issue`.
///
/// Why: Partial-update payload; `None` means "don't touch".
/// What: Optional everything.
/// Test: built from JSON in dispatch.
#[derive(Debug, Clone, Default)]
pub struct UpdateIssueParams {
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub assignee: Option<String>,
    pub labels: Option<Vec<String>>,
    pub milestone_id: Option<String>,
    pub state: Option<String>,
}

/// Parameters for `list_issues`.
///
/// Why: Pagination + filter combo used by every backend.
/// What: Sensible defaults filled in by the dispatcher.
/// Test: dispatch tests.
#[derive(Debug, Clone, Default)]
pub struct ListIssuesParams {
    pub project_id: Option<String>,
    pub state: Option<String>,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub limit: u32,
    pub offset: u32,
}

/// Parameters for `search_issues`.
///
/// Why: Search is a superset of list — adds free-text query and priority.
/// What: All fields optional.
/// Test: dispatch tests.
#[derive(Debug, Clone, Default)]
pub struct SearchIssuesParams {
    pub query: Option<String>,
    pub state: Option<String>,
    pub priority: Option<String>,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub project_id: Option<String>,
    pub milestone_id: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

/// Parameters for `create_milestone`.
///
/// Why: All three backends accept name + optional description + due date.
/// What: ISO-8601 strings; backends parse as appropriate.
/// Test: live integration.
#[derive(Debug, Clone, Default)]
pub struct CreateMilestoneParams {
    pub name: String,
    pub description: Option<String>,
    pub due_date: Option<String>,
}

/// Unified backend interface.
///
/// Why: One trait = one dispatcher = one MCP server.
/// What: Async methods returning `anyhow::Result`. Backends that don't
/// support an operation should return `Err` with a clear message.
/// Test: every backend impl has unit tests for shape (and integration
/// tests gated behind env-var creds).
#[async_trait]
pub trait Backend: Send + Sync {
    /// Backend identifier ("github" / "jira" / "linear"). Why: lets the
    /// dispatcher tag returned `Issue.backend` consistently.
    fn name(&self) -> &'static str;

    // ----- Issues -----
    async fn create_issue(&self, params: CreateIssueParams) -> Result<Issue>;
    async fn get_issue(&self, id: &str) -> Result<Issue>;
    async fn update_issue(&self, id: &str, params: UpdateIssueParams) -> Result<Issue>;
    async fn close_issue(&self, id: &str, comment: Option<&str>) -> Result<Issue>;
    async fn reopen_issue(&self, id: &str) -> Result<Issue>;
    async fn list_issues(&self, params: ListIssuesParams) -> Result<Vec<Issue>>;
    async fn search_issues(&self, params: SearchIssuesParams) -> Result<Vec<Issue>>;

    // ----- Comments -----
    async fn add_comment(&self, issue_id: &str, body: &str) -> Result<Comment>;
    async fn list_comments(&self, issue_id: &str) -> Result<Vec<Comment>>;
    async fn update_comment(&self, issue_id: &str, comment_id: &str, body: &str)
    -> Result<Comment>;
    async fn delete_comment(&self, issue_id: &str, comment_id: &str) -> Result<()>;

    // ----- Labels -----
    async fn list_labels(&self) -> Result<Vec<Label>>;
    async fn create_label(
        &self,
        name: &str,
        color: Option<&str>,
        description: Option<&str>,
    ) -> Result<Label>;
    async fn add_labels(&self, issue_id: &str, labels: &[String]) -> Result<()>;
    async fn remove_labels(&self, issue_id: &str, labels: &[String]) -> Result<()>;

    // ----- Milestones -----
    async fn list_milestones(&self) -> Result<Vec<Milestone>>;
    async fn create_milestone(&self, params: CreateMilestoneParams) -> Result<Milestone>;
    async fn close_milestone(&self, id: &str) -> Result<Milestone>;
    async fn get_milestone_issues(&self, id: &str) -> Result<Vec<Issue>>;

    // ----- Projects / Epics -----
    async fn list_projects(&self) -> Result<Vec<Project>>;
    async fn get_project(&self, id: &str) -> Result<Project>;
    async fn list_epics(&self) -> Result<Vec<Issue>>;
    async fn get_epic_issues(&self, epic_id: &str) -> Result<Vec<Issue>>;

    // ----- Project updates -----
    async fn create_project_update(
        &self,
        project_id: &str,
        body: &str,
        health: Option<&str>,
    ) -> Result<ProjectUpdate>;
    async fn list_project_updates(&self, project_id: &str) -> Result<Vec<ProjectUpdate>>;

    // ----- Workflow -----
    async fn list_states(&self) -> Result<Vec<String>>;
    async fn transition_issue(&self, id: &str, state: &str) -> Result<Issue>;
    async fn assign_issue(&self, id: &str, assignee: &str) -> Result<Issue>;
}
