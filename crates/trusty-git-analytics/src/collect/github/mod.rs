//! GitHub REST API client for pull-request metadata.

pub mod client;
pub mod org_discovery;
pub(crate) mod retry;
pub mod reviewer_store;

pub use client::{GhLabel, GitHubClient, GitHubIssue};
pub use org_discovery::{discover_org_repos, effective_orgs};
pub use reviewer_store::{lookup_github_pr_id, upsert_github_pr_reviewer};
