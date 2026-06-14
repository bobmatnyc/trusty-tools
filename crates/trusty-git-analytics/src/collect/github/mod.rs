//! GitHub REST API client for pull-request metadata.

pub mod client;
pub mod org_discovery;
pub(crate) mod repo_resolver;
pub(crate) mod retry;
pub mod reviewer_store;
pub mod types;

pub use client::GitHubClient;
pub use org_discovery::{discover_org_repos, effective_orgs};
pub use repo_resolver::resolve_github_repos;
pub use reviewer_store::{lookup_github_pr_id, upsert_github_pr_reviewer};
pub use types::{GhLabel, GitHubIssue};
