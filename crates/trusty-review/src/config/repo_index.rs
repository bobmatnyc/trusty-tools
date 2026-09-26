//! Resolve the trusty-search index for a PR's own repository, per call (#8649).
//!
//! Why: `ReviewConfig::resolve_index` picks ONE index at server startup from the
//! server's CWD and falls back to `"main"` on a miss. A `review_pr` call names
//! its repo explicitly, so reusing that startup index reviewed every other repo
//! against the wrong index, and the context gate then reported the resulting
//! unknown index as a generic `infra_unavailable` skip.
//!
//! What: [`resolve_repo_index`] maps `owner/repo` to an index id through the
//! `repo_identity` trusty-search records for each index (DOC-37). The session's
//! pinned index wins when it belongs to the repo. With no identity match, an
//! index whose id is the bare repo name and that carries no identity is used.
//! Anything else is a [`RepoIndexError`] naming the repo and the index id it
//! looked up — never a default index.
//!
//! Test: `src/mcp/tools_pr_index_tests.rs` (it drives this module through the
//! `review_pr` tool's per-call config as well as directly).

use trusty_common::github_path::parse_owner_repo;
use trusty_common::repo_identity::RepoIdentity;

use crate::integrations::search_client::{IndexIdentity, SearchClient, SearchClientError};

/// Why `review_pr` could not pick a trusty-search index for a repo (#8649).
///
/// Why: a missing index is a caller-fixable condition, not an outage; the
/// message must say which repo and which index id were looked up.
/// What: one variant per failure; each `Display` names the repo.
/// Test: `missing_index_error_names_the_repo_and_the_index_id`,
/// `registry_failure_is_an_error_not_a_default`.
#[derive(Debug, thiserror::Error)]
pub enum RepoIndexError {
    /// `owner` or `repo` is empty or not a repository name.
    #[error("cannot resolve a trusty-search index: {owner:?}/{repo:?} is not a repository name")]
    InvalidRepo {
        /// The `owner` argument as given.
        owner: String,
        /// The `repo` argument as given.
        repo: String,
    },
    /// The daemon's index list could not be read.
    #[error(
        "cannot resolve the trusty-search index for {repo} (looked up index {index_id:?}): \
         listing indexes failed: {source}"
    )]
    Registry {
        /// Canonical `owner/repo`.
        repo: String,
        /// The index id that was looked up.
        index_id: String,
        /// The listing failure.
        source: SearchClientError,
    },
    /// No registered index belongs to the repo.
    #[error(
        "no trusty-search index for {repo}: looked up index {index_id:?} and repo_identity \
         {repo:?} across {registered} registered index(es); {detail}. Index a checkout of \
         {repo} with `trusty-search index <path>`, then retry"
    )]
    NoIndex {
        /// Canonical `owner/repo`.
        repo: String,
        /// The index id that was looked up.
        index_id: String,
        /// How many indexes the daemon listed.
        registered: usize,
        /// Why the looked-up id did not qualify.
        detail: String,
    },
}

/// Resolve the trusty-search index serving `owner/repo`.
///
/// Why: `review_pr` must query the PR repo's index, not the index the MCP
/// server resolved from its own CWD at startup (#8649).
/// What: lists indexes with their `repo_identity` and delegates the choice to
/// [`select_repo_index`]; `pinned` is the session's configured index, used only
/// when it belongs to this repo. Fails closed: every miss is an error.
///
/// # Errors
///
/// [`RepoIndexError::InvalidRepo`] for an empty or unparseable name,
/// [`RepoIndexError::Registry`] when the list cannot be read, and
/// [`RepoIndexError::NoIndex`] when no index belongs to the repo.
///
/// Test: `two_repos_resolve_to_their_own_indexes_in_one_server`,
/// `missing_index_error_names_the_repo_and_the_index_id`,
/// `registry_failure_is_an_error_not_a_default`.
pub async fn resolve_repo_index(
    client: &dyn SearchClient,
    owner: &str,
    repo: &str,
    pinned: Option<&str>,
) -> Result<String, RepoIndexError> {
    let invalid = || RepoIndexError::InvalidRepo {
        owner: owner.to_string(),
        repo: repo.to_string(),
    };
    let (owner_t, repo_t) = (owner.trim(), repo.trim());
    if owner_t.is_empty() || repo_t.is_empty() || owner_t.contains('/') || repo_t.contains('/') {
        return Err(invalid());
    }
    let repo_key = parse_owner_repo(&format!("{owner_t}/{repo_t}"))
        .map(|gp| RepoIdentity::GitHub(gp).canonical())
        .ok_or_else(invalid)?;
    let index_id = repo_t.to_string();

    let indexes =
        client
            .list_index_identities()
            .await
            .map_err(|source| RepoIndexError::Registry {
                repo: repo_key.clone(),
                index_id: index_id.clone(),
                source,
            })?;
    select_repo_index(&indexes, &repo_key, &index_id, pinned).map_err(|detail| {
        RepoIndexError::NoIndex {
            repo: repo_key.clone(),
            index_id: index_id.clone(),
            registered: indexes.len(),
            detail,
        }
    })
}

/// Pick the index for `repo_key` from a listed registry, or explain the miss.
///
/// Why: kept pure so the selection rules are testable without a client.
/// What: among indexes whose normalised `repo_identity` equals `repo_key`, the
/// `pinned` id wins; otherwise the shortest `root_path` (the live checkout
/// sorts before its `.base` clone and session worktrees), then the smallest
/// id. With no identity match, an index whose id is `index_id` and that has
/// no identity is accepted. `Err` carries the reason the lookup missed.
/// Test: `pinned_index_wins_only_when_it_belongs_to_the_repo`,
/// `same_named_index_of_another_repo_is_refused`.
pub(crate) fn select_repo_index(
    indexes: &[IndexIdentity],
    repo_key: &str,
    index_id: &str,
    pinned: Option<&str>,
) -> Result<String, String> {
    let identity_of = |i: &IndexIdentity| {
        i.repo_identity
            .as_deref()
            .and_then(RepoIdentity::parse)
            .map(|r| r.canonical())
    };
    let matching: Vec<&IndexIdentity> = indexes
        .iter()
        .filter(|i| identity_of(i).as_deref() == Some(repo_key))
        .collect();
    if let Some(pin) = pinned
        && matching.iter().any(|i| i.id == pin)
    {
        return Ok(pin.to_string());
    }
    if let Some(best) = matching.iter().min_by_key(|i| {
        let len = i.root_path.as_ref().map_or(usize::MAX, String::len);
        (len, i.id.as_str())
    }) {
        return Ok(best.id.clone());
    }
    match indexes.iter().find(|i| i.id == index_id) {
        Some(i) => match identity_of(i) {
            None => Ok(i.id.clone()),
            Some(other) => Err(format!(
                "index {index_id:?} is registered but belongs to {other}"
            )),
        },
        None => Err(format!(
            "index {index_id:?} is not registered and no index carries that repo_identity"
        )),
    }
}
