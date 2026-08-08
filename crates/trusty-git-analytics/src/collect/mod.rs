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
pub mod correlate;
pub mod env_expand;
pub mod errors;
pub mod git;
pub mod github;
mod github_pipeline;
pub mod identity;
pub mod jira;
pub mod linear;
mod linear_pipeline;
pub mod pm_adapter;
pub mod pr_provider;
pub mod ticket;
pub mod weeks;

pub use collector::{CollectionPipeline, CollectionStats};
pub use correlate::{correlate_commits, CorrelationOutcome};
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

    /// #5197: the bus is opt-in, so an untouched pipeline publishes nothing.
    #[test]
    fn progress_is_disabled_by_default() {
        let observer = crate::core::progress::ProgressBus::bounded(8);
        let pipeline = CollectionPipeline::new(Config::default());
        let mut db = crate::core::db::Database::open_in_memory().expect("open");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(pipeline.run(&mut db)).expect("run");
        assert!(
            observer.drain().is_empty(),
            "a pipeline with no bus attached must publish nothing"
        );
    }

    /// #5197: every configured repository reaches a terminal progress event,
    /// even one that cannot be opened — otherwise its row spins forever.
    #[test]
    fn run_emits_a_terminal_event_per_repo() {
        let bus = crate::core::progress::ProgressBus::bounded(64);
        let cfg = Config {
            repositories: vec![RepositoryConfig {
                path: std::env::temp_dir().join("tga-5197-not-a-repo"),
                name: Some("ghost".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let pipeline = CollectionPipeline::new(cfg).with_progress(bus.clone());
        let mut db = crate::core::db::Database::open_in_memory().expect("open");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(pipeline.run(&mut db)).expect("run");

        let events = bus.drain();
        assert!(!events.is_empty());
        let terminal: Vec<_> = events
            .iter()
            .filter(|e| e.target == "ghost" && e.is_terminal())
            .collect();
        assert_eq!(terminal.len(), 1, "exactly one terminal event: {events:?}");
        assert!(
            matches!(
                terminal[0].outcome,
                Some(crate::core::progress::Outcome::Failed { .. })
            ),
            "an unopenable repo reports failed, not completed"
        );
    }
}
