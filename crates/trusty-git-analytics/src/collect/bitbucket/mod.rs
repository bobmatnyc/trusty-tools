//! Bitbucket Cloud REST API client for pull-request metadata.
//!
//! Surfaces a single [`BitbucketClient`] that implements
//! [`crate::collect::pr_provider::PrProvider`], so the pipeline can use it
//! interchangeably with the GitHub client, plus the workspace → repository
//! discovery that decides which repositories that client covers (#5220).

pub mod client;
pub mod types;
pub mod workspace_discovery;

pub use client::BitbucketClient;
pub use workspace_discovery::{
    discover_workspace_repos, effective_workspaces, resolve_bitbucket_repos,
    run_workspace_discovery, WorkspaceDiscovery,
};
