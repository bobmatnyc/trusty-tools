//! Stage 1 of the pipeline: extract commit data from local git repositories
//! and correlate it with external systems (GitHub pull requests, JIRA
//! tickets, developer identity records). All output is persisted via
//! [`crate::core::db::Database`].
//!
//! ## Submodules
//!
//! - [`git`] — commit extraction via libgit2
//! - [`identity`] — author identity resolution (exact + fuzzy)
//! - [`github`] — GitHub REST client (PRs)
//! - [`jira`] — JIRA REST client (issues)
//! - [`linear`] — Linear GraphQL client (issues)
//! - [`azdo`] — Azure DevOps stub client (Phase 1: config + AB# detection)
//! - [`bitbucket`] — Bitbucket Cloud REST client (PRs)
//! - [`pr_provider`] — provider-agnostic PR fetch trait
//! - [`ticket`] — ticket-reference detection on commit messages
//! - [`collector`] — end-to-end pipeline orchestrator
//! - [`errors`] — module-level error type ([`CollectError`])

pub mod ai_attribution;
pub mod azdo;
pub mod bitbucket;
pub mod collector;
pub mod env_expand;
pub mod errors;
pub mod git;
pub mod github;
pub mod identity;
pub mod jira;
pub mod linear;
pub mod pm_adapter;
pub mod pr_provider;
pub mod ticket;
pub mod weeks;

pub use collector::{CollectionPipeline, CollectionStats};
pub use errors::{CollectError, Result};
pub use pm_adapter::{
    build_adapters, AzureDevOpsAdapter, GitHubAdapter, JiraAdapter, LinearAdapter, PmAdapter,
    PmError, PmSource, PmTicket,
};
pub use pr_provider::PrProvider;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{Config, RepositoryConfig};

    #[test]
    fn git_collector_rejects_missing_path() {
        let cfg = RepositoryConfig {
            path: "/definitely/does/not/exist/here".into(),
            ..Default::default()
        };
        let err = git::GitCollector::new(&cfg).expect_err("should fail");
        match err {
            CollectError::Config(msg) => assert!(msg.contains("does not exist"), "msg: {msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn git_collector_rejects_non_repo_path() {
        // /tmp exists but is not a git repo.
        let cfg = RepositoryConfig {
            path: std::env::temp_dir(),
            ..Default::default()
        };
        let err = git::GitCollector::new(&cfg).expect_err("should fail");
        assert!(matches!(err, CollectError::Git(_)));
    }

    #[test]
    fn pipeline_constructs_with_default_config() {
        let cfg = Config::default();
        let _pipeline = CollectionPipeline::new(cfg);
    }
}
