//! Shared registry fake and the surface contract of `resolve_pr_index` (#8651).
//!
//! Why: four surfaces resolve a PR repo's index through `resolve_pr_index`
//! now; each needs the same registry fake, and the surface argument decides
//! whether an unreadable registry degrades or fails.
//! What: `Registry` (a trusty-search fake reporting `repo_identity` per index),
//! the `entry`/`two_repo_registry` builders, and the Hosted-surface tests. The
//! Interactive-surface tests live in `mcp/tools_pr_index_tests.rs`.
//! Test: this module IS the tests.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::{
    config::{InvocationSurface, ReviewConfig, repo_index::RepoIndexError},
    integrations::search_client::{
        EmbedderState, HealthResponse, IndexIdentity, IndexInfo, IndexStatusResponse, SearchClient,
        SearchClientError, SearchResult,
    },
};

use super::{PrIndex, resolve_pr_index};

/// Search fake: a healthy daemon whose registry reports each index's
/// `repo_identity`, or fails to list at all when `indexes` is `None`. Each
/// listing's `repo_identity` filter is recorded, and applied as the daemon
/// does — unless `ignore_filter` is set, modelling a daemon whose
/// `?repo_identity=` filtering is broken and returns the full list regardless
/// (#8649: proves `resolve_repo_index`'s local re-check is not dead code).
pub(crate) struct Registry {
    pub(crate) indexes: Option<Vec<IndexIdentity>>,
    pub(crate) listings: Mutex<Vec<Option<String>>>,
    pub(crate) ignore_filter: bool,
}

impl Registry {
    pub(crate) fn new(indexes: Option<Vec<IndexIdentity>>) -> Self {
        Self {
            indexes,
            listings: Mutex::new(Vec::new()),
            ignore_filter: false,
        }
    }

    /// A daemon that ignores the `repo_identity=` filter argument entirely.
    pub(crate) fn new_ignoring_filter(indexes: Vec<IndexIdentity>) -> Self {
        Self {
            indexes: Some(indexes),
            listings: Mutex::new(Vec::new()),
            ignore_filter: true,
        }
    }
}

#[async_trait]
impl SearchClient for Registry {
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        Ok(HealthResponse {
            status: "ok".into(),
            embedder: EmbedderState::Bool(true),
            warmboot_summary: None,
        })
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        unreachable!("#8649 resolution must read list_index_identities")
    }

    async fn list_index_identities(
        &self,
        repo_identity: Option<&str>,
    ) -> Result<Vec<IndexIdentity>, SearchClientError> {
        if let Ok(mut listings) = self.listings.lock() {
            listings.push(repo_identity.map(str::to_string));
        }
        let all = self
            .indexes
            .clone()
            .ok_or_else(|| SearchClientError::Transport("connection refused".into()))?;
        if self.ignore_filter {
            return Ok(all);
        }
        Ok(match repo_identity {
            Some(filter) => all
                .into_iter()
                .filter(|i| i.repo_identity.as_deref() == Some(filter))
                .collect(),
            None => all,
        })
    }

    async fn index_status(&self, id: &str) -> Result<IndexStatusResponse, SearchClientError> {
        Ok(IndexStatusResponse::ready(id))
    }

    async fn search(
        &self,
        _: &str,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Ok(vec![])
    }
}

pub(crate) fn entry(id: &str, root: &str, identity: Option<&str>) -> IndexIdentity {
    IndexIdentity {
        id: id.into(),
        root_path: Some(root.into()),
        repo_identity: identity.map(str::to_string),
        last_used_unix: None,
    }
}

/// A registry holding two repos (one with a session worktree facet) plus a
/// `"main"` index that belongs to neither.
pub(crate) fn two_repo_registry() -> Vec<IndexIdentity> {
    vec![
        entry("main", "/src/scratch", Some("someone/scratch")),
        entry(
            "code-intelligence-wt-abc",
            "/src/code-intelligence/.worktrees/abc",
            Some("duettoresearch/code-intelligence"),
        ),
        entry(
            "code-intelligence-9f1c2e3a",
            "/src/code-intelligence",
            Some("duettoresearch/code-intelligence"),
        ),
        entry(
            "trusty-tools-4e2cf878",
            "/src/trusty-tools",
            Some("bobmatnyc/trusty-tools"),
        ),
    ]
}

/// A config whose startup index is the `"main"` a CWD miss leaves behind.
pub(crate) fn startup_config(require_search: Option<bool>) -> ReviewConfig {
    let mut config = ReviewConfig::load(None);
    config.search_index = "main".into();
    config.search_index_explicit = false;
    config.context.require_search = require_search;
    config
}

/// The hosted surfaces (webhook drain, service `review`, CLI GitHub-PR `run`)
/// require search by default, so an unreadable registry is an error there —
/// never a silent diff-only review that then posts (REV-011, #8651).
#[tokio::test]
async fn hosted_surface_requires_search_for_an_unreadable_registry() {
    let config = startup_config(None);
    let err = resolve_pr_index(
        &Registry::new(None),
        &config,
        InvocationSurface::Hosted,
        "acme",
        "widget",
    )
    .await
    .expect_err("Hosted must not degrade past an unreadable registry by default");
    assert!(matches!(err, RepoIndexError::Registry { .. }), "{err}");
    assert!(err.to_string().contains("acme/widget"), "{err}");
}

/// An operator who opted out of search gets the DEGRADED diff-only review on
/// a hosted surface too — the surface's own `require_search` contract.
#[tokio::test]
async fn hosted_surface_degrades_when_the_operator_opts_out_of_search() {
    let config = startup_config(Some(false));
    let index = resolve_pr_index(
        &Registry::new(None),
        &config,
        InvocationSurface::Hosted,
        "acme",
        "widget",
    )
    .await
    .expect("an explicit opt-out degrades");
    assert!(
        matches!(index, PrIndex::DiffOnly(ref n) if n.contains("DEGRADED")),
        "{index:?}"
    );

    // A resolvable repo still resolves on the hosted surface.
    let index = resolve_pr_index(
        &Registry::new(Some(two_repo_registry())),
        &config,
        InvocationSurface::Hosted,
        "bobmatnyc",
        "trusty-tools",
    )
    .await;
    assert_eq!(
        index.ok(),
        Some(PrIndex::Resolved("trusty-tools-4e2cf878".into()))
    );
}
