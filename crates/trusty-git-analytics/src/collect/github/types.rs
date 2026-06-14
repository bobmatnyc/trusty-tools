//! GitHub REST API payload types.
//!
//! Why: separating wire-protocol types from client logic keeps `client.rs`
//! under the 500-SLOC cap and lets other modules (`pm_adapter`, `reviewer_store`)
//! import only what they need without pulling in HTTP machinery.
//! What: public structs deserialized from GitHub REST responses; also re-exported
//! from `crate::collect::github` for downstream consumers.
//! Test: shape tests live in `client_tests.rs`, loaded via `#[path]` in `client.rs`.

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// Internal wire type for a pull request list element.
///
/// Why: only a subset of fields are needed; keeping this crate-private avoids
/// exposing GitHub's full PR response schema.
/// What: deserializes `/repos/{o}/{r}/pulls` list elements.
/// Test: `commit_shas_gated_on_merged_at` exercises `merged_at` / `merge_commit_sha`.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiPull {
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) user: Option<ApiUser>,
    pub(crate) state: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) merged_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub(crate) merge_commit_sha: Option<String>,
}

/// Internal wire type for an author / actor embedded in PR payloads.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiUser {
    pub(crate) login: String,
}

/// A GitHub issue as returned by the REST API.
///
/// This is the normalized payload returned by [`super::GitHubClient::fetch_issue`].
/// Only the subset of fields used by the project-management adapter are deserialized.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GitHubIssue {
    /// Issue number (the `N` in `#N`).
    pub number: u64,
    /// Issue title / summary.
    pub title: String,
    /// Workflow state — `"open"` or `"closed"`.
    pub state: String,
    /// Web URL to the issue on github.com.
    pub html_url: String,
    /// Labels applied to the issue.
    #[serde(default)]
    pub labels: Vec<GhLabel>,
    /// Issue body / description (Markdown). May be absent or empty.
    #[serde(default)]
    pub body: Option<String>,
}

/// A GitHub label as returned alongside a [`GitHubIssue`].
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GhLabel {
    /// Label name (e.g. `"bug"`, `"enhancement"`).
    pub name: String,
}

/// A GitHub user reference as embedded in reviews and other payloads.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GhUser {
    /// GitHub login (username).
    pub login: String,
}

/// Embedded git author metadata returned with a PR commit payload.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GhAuthor {
    /// Author display name from the git object.
    pub name: String,
    /// Author email from the git object.
    pub email: String,
    /// Author timestamp (ISO8601). May be absent on some endpoints.
    #[serde(default)]
    pub date: Option<String>,
}

/// Inner `commit` object shape returned by the PR commits endpoint.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GitHubCommitDetail {
    /// Full commit message (subject + body).
    pub message: String,
    /// Optional author block (`name`, `email`, `date`).
    #[serde(default)]
    pub author: Option<GhAuthor>,
}

/// A commit reference returned by the PR commits endpoint
/// (`GET /repos/{owner}/{repo}/pulls/{number}/commits`).
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GitHubPrCommit {
    /// Full 40-char commit SHA.
    pub sha: String,
    /// Nested commit metadata (message, author).
    pub commit: GitHubCommitDetail,
}

/// A pull-request review as returned by
/// `GET /repos/{owner}/{repo}/pulls/{number}/reviews`.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GitHubReview {
    /// Review id.
    pub id: u64,
    /// Review state (`APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, ...).
    pub state: String,
    /// Reviewer user (may be absent for deleted accounts).
    #[serde(default)]
    pub user: Option<GhUser>,
    /// ISO8601 submission timestamp. `None` for pending drafts.
    #[serde(default)]
    pub submitted_at: Option<String>,
}
