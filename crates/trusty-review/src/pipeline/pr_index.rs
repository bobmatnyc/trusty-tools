//! The trusty-search index one GitHub-PR review runs against (#8649, #8651).
//!
//! Why: every surface that reviews a named `owner/repo` — the MCP `review_pr`
//! tool, the webhook drain, the service's `review` operation, and the CLI
//! `run` — must query that repo's own index. Reusing the index the process
//! resolved at startup (or `"main"`) reviewed every other repo against the
//! wrong one. #8649 fixed `review_pr` alone; #8651 moved the resolution here so
//! the other three surfaces share it.
//! What: [`resolve_pr_index`] maps `owner/repo` through
//! [`resolve_repo_index`](crate::config::repo_index) and applies the calling
//! surface's `require_search` contract to an unreadable registry. [`PrIndex`]
//! turns the outcome into the per-review config and deps.
//! Test: `pr_index_tests.rs`, plus each surface's own regression test.

use std::sync::Arc;

use tracing::warn;

use crate::{
    config::{
        InvocationSurface, ReviewConfig,
        repo_index::{RepoIndexError, resolve_repo_index},
    },
    integrations::{NullAnalyzeClient, NullSearchClient, search_client::SearchClient},
    pipeline::ReviewDeps,
};

/// The search index one GitHub-PR review runs against (#8649).
///
/// Why: the startup index belongs to whatever repo the process was launched
/// in; a registry that cannot be read must degrade to NO index, never to that
/// one.
/// What: `Resolved` carries the PR repo's index id; `DiffOnly` carries the
/// operator-facing notice for a degraded, diff-only review.
/// Test: `two_repos_resolve_to_their_own_indexes_in_one_server`,
/// `unreadable_registry_degrades_to_diff_only_when_search_is_not_required`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrIndex {
    /// The PR repo's own index.
    Resolved(String),
    /// No index could be resolved; review the diff alone, loudly.
    DiffOnly(String),
}

impl PrIndex {
    /// The per-review config and deps for this index.
    ///
    /// Why: `DiffOnly` must disconnect search AND analyze from any index, as
    /// `ReviewConfig::resolve_source_root`'s diff-only path does, or a healthy
    /// daemon would still be queried against the startup index.
    /// What: `Resolved` clones `base` with `search_index` set and explicit.
    /// `DiffOnly` clears `search_index`, relaxes both `require_*` flags, and
    /// swaps `deps.search`/`deps.analyze` for null clients carrying the notice,
    /// so the context gate returns `Degraded` with that notice as its reason.
    /// Test: `unreadable_registry_degrades_to_diff_only_when_search_is_not_required`.
    pub fn apply(self, base: &ReviewConfig, mut deps: ReviewDeps) -> (ReviewConfig, ReviewDeps) {
        let mut config = base.clone();
        config.search_index_explicit = true;
        match self {
            PrIndex::Resolved(index) => config.search_index = index,
            PrIndex::DiffOnly(notice) => {
                config.search_index = String::new();
                config.context.require_search = Some(false);
                config.context.require_analyze = false;
                deps.search = Arc::new(NullSearchClient::new(notice.clone()));
                deps.analyze = Some(Arc::new(NullAnalyzeClient::new(notice)));
            }
        }
        (config, deps)
    }
}

/// Resolve the index a review of `owner/repo` uses on `surface`, or the degrade.
///
/// Why: an unreadable registry is a search outage, so it follows the surface's
/// own `require_search` contract — the interactive MCP tool degrades by
/// default, while the hosted webhook bot and a CLI GitHub-PR run require
/// search by default (REV-011). An unregistered repo is a configuration fault
/// and never degrades (#6687).
/// What: `resolve_repo_index` against `search`, with `config.search_index` as
/// the pin (used only when it belongs to the repo). `RepoIndexError::Registry`
/// becomes [`PrIndex::DiffOnly`] (logged at `warn`) when
/// `effective_require_search(surface)` is false; every other error, and
/// `Registry` when search is required, is returned.
///
/// # Errors
///
/// Every [`RepoIndexError`] except a `Registry` failure on a surface that does
/// not require search.
///
/// Test: `hosted_surface_requires_search_for_an_unreadable_registry`,
/// `hosted_surface_degrades_when_the_operator_opts_out_of_search`,
/// `registry_failure_is_an_error_when_search_is_required`.
pub async fn resolve_pr_index(
    search: &dyn SearchClient,
    config: &ReviewConfig,
    surface: InvocationSurface,
    owner: &str,
    repo: &str,
) -> Result<PrIndex, RepoIndexError> {
    let pinned = Some(config.search_index.as_str());
    match resolve_repo_index(search, owner, repo, pinned).await {
        Ok(index) => Ok(PrIndex::Resolved(index)),
        Err(e @ RepoIndexError::Registry { .. })
            if !config.context.effective_require_search(surface) =>
        {
            let notice = format!(
                "{e} — search is not required for this call, so this review is DEGRADED: \
                 diff only, no code context and no static analysis, not the server's startup \
                 index (#8649)"
            );
            warn!("{notice}");
            Ok(PrIndex::DiffOnly(notice))
        }
        Err(e) => Err(e),
    }
}

// #8651: shared registry fake + surface-contract tests.
#[cfg(test)]
#[path = "pr_index_tests.rs"]
pub(crate) mod tests;
