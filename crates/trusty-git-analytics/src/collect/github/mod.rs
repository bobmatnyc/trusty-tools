//! GitHub REST API client for pull-request metadata.

pub mod budget;
pub mod client;
pub mod issue_writer;
pub mod org_discovery;
pub(crate) mod repo_resolver;
pub(crate) mod retry;
pub mod reviewer_store;
pub mod types;

pub use client::GitHubClient;
pub use issue_writer::{
    find_thread_by_marker, issue_search_query, thread_marker_anchor, IssueUpsert,
};
pub use org_discovery::{discover_org_repos, discover_org_repos_at, effective_orgs};
pub use repo_resolver::resolve_github_repos;
// #5216: the non-interactive `tga install` path needs the one authed GitHub
// client builder rather than a second `reqwest::Client::builder()` of its own.
pub(crate) use repo_resolver::build_http_client;
pub use reviewer_store::{lookup_github_pr_id, upsert_github_pr_reviewer};
pub use types::{GhLabel, GitHubIssue};
