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
//! index whose id is the bare repo name and that has no recorded identity is
//! used, with a warning. Anything else is a [`RepoIndexError`] naming the repo
//! and the index id it looked up — never a default index.
//!
//! Test: `src/mcp/tools_pr_index_tests.rs` (it drives this module through the
//! `review_pr` tool's per-call config as well as directly).

use std::cmp::Reverse;

use tracing::warn;
use trusty_common::github_path::parse_owner_repo;
use trusty_common::repo_identity::RepoIdentity;

use crate::integrations::search_client::{IndexIdentity, SearchClient, SearchClientError};

/// Why `review_pr` could not pick a trusty-search index for a repo (#8649).
///
/// Why: a missing index is a caller-fixable condition, not an outage; the
/// message must say which repo and which index id were looked up.
/// What: one variant per failure; each `Display` names the repo. `Registry`
/// is the only variant `review_pr` may degrade on (search not required).
/// Test: `missing_index_error_names_the_repo_and_the_index_id`,
/// `registry_failure_is_an_error_when_search_is_required`.
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
/// What: asks the daemon for the indexes recorded for this repo
/// (`?repo_identity=`, filtered server-side) and re-checks each identity
/// locally; only on no match does it list every index for the bare-name
/// fallback in [`select_repo_index`]. `pinned` is the session's configured
/// index, used only when it belongs to this repo. Every miss is an error.
///
/// # Errors
///
/// [`RepoIndexError::InvalidRepo`] for an empty or unparseable name,
/// [`RepoIndexError::Registry`] when a list cannot be read, and
/// [`RepoIndexError::NoIndex`] when no index belongs to the repo.
///
/// Test: `two_repos_resolve_to_their_own_indexes_in_one_server`,
/// `missing_index_error_names_the_repo_and_the_index_id`,
/// `registry_failure_is_an_error_when_search_is_required`,
/// `identity_filter_is_queried_first_and_the_full_list_only_on_a_miss`.
pub(crate) async fn resolve_repo_index(
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
    let registry = |source| RepoIndexError::Registry {
        repo: repo_key.clone(),
        index_id: index_id.clone(),
        source,
    };

    // #8649: the daemon filters by identity before its per-index disk walk.
    let scoped = client
        .list_index_identities(Some(&repo_key))
        .await
        .map_err(registry)?;
    if let Some(id) = pick_identity_match(&scoped, &repo_key, pinned) {
        return Ok(id);
    }
    let indexes = client.list_index_identities(None).await.map_err(registry)?;
    select_repo_index(&indexes, &repo_key, &index_id, pinned).map_err(|detail| {
        RepoIndexError::NoIndex {
            repo: repo_key.clone(),
            index_id: index_id.clone(),
            registered: indexes.len(),
            detail,
        }
    })
}

/// The canonical identity an index records, or `None` when absent/unparseable.
fn canonical_identity(index: &IndexIdentity) -> Option<String> {
    index
        .repo_identity
        .as_deref()
        .and_then(RepoIdentity::parse)
        .map(|r| r.canonical())
}

/// Among indexes recorded for `repo_key`: the pin, else the most recently
/// used, then the shortest `root_path`, then the smallest id (#8649).
///
/// Why: one repo can own several indexes (live checkout, `.base` clone,
/// session worktrees); the choice must be deterministic and favour the one in
/// use. The live checkout's path is usually the shortest.
/// What: `None` when no index's identity parses to `repo_key`.
/// Test: `pinned_index_wins_only_when_it_belongs_to_the_repo`,
/// `identity_tiebreak_prefers_recent_use_then_short_root_then_id`.
fn pick_identity_match(
    indexes: &[IndexIdentity],
    repo_key: &str,
    pinned: Option<&str>,
) -> Option<String> {
    let matching: Vec<&IndexIdentity> = indexes
        .iter()
        .filter(|i| canonical_identity(i).as_deref() == Some(repo_key))
        .collect();
    if let Some(pin) = pinned
        && matching.iter().any(|i| i.id == pin)
    {
        return Some(pin.to_string());
    }
    matching
        .into_iter()
        .min_by_key(|i| {
            let root_len = i.root_path.as_ref().map_or(usize::MAX, String::len);
            (Reverse(i.last_used_unix), root_len, i.id.as_str())
        })
        .map(|i| i.id.clone())
}

/// Pick the index for `repo_key` from a listed registry, or explain the miss.
///
/// Why: kept pure so the selection rules are testable without a client.
/// What: an identity match per [`pick_identity_match`]; otherwise the index
/// whose id is `index_id`, only when it has NO recorded identity (logged at
/// `warn`). A recorded identity that is unparseable or names another repo is
/// refused. `Err` carries the reason, and names the pinned index and its
/// recorded identity when the pin exists but was not used (a fork's index).
/// Test: `pinned_index_wins_only_when_it_belongs_to_the_repo`,
/// `same_named_index_of_another_repo_is_refused`,
/// `unreadable_identity_is_refused_by_the_name_fallback`,
/// `foreign_pin_is_named_in_the_miss`.
pub(crate) fn select_repo_index(
    indexes: &[IndexIdentity],
    repo_key: &str,
    index_id: &str,
    pinned: Option<&str>,
) -> Result<String, String> {
    if let Some(id) = pick_identity_match(indexes, repo_key, pinned) {
        return Ok(id);
    }
    // #8649: a fork user must see why their session's index was not used.
    let pin_note = pinned
        .filter(|pin| *pin != index_id)
        .and_then(|pin| indexes.iter().find(|i| i.id == pin))
        .map(|pin| {
            let recorded = pin
                .repo_identity
                .as_deref()
                .map_or_else(|| "no recorded repo_identity".to_string(), str::to_string);
            format!(
                "; the session's index {:?} is registered for {recorded}, not {repo_key}, \
                 so it is not used (an index recorded for another repo or owner, such as a \
                 fork's, never is)",
                pin.id
            )
        })
        .unwrap_or_default();
    let Some(named) = indexes.iter().find(|i| i.id == index_id) else {
        return Err(format!(
            "index {index_id:?} is not registered and no index carries that repo_identity{pin_note}"
        ));
    };
    match named.repo_identity.as_deref() {
        None => {
            warn!(
                index = %named.id,
                repo = %repo_key,
                "review_pr: using index {:?} by name; it has no recorded repo_identity, so it \
                 cannot be verified to belong to {repo_key} (#8649)",
                named.id
            );
            Ok(named.id.clone())
        }
        Some(raw) => match RepoIdentity::parse(raw) {
            Some(other) => Err(format!(
                "index {index_id:?} is registered but belongs to {}{pin_note}",
                other.canonical()
            )),
            None => Err(format!(
                "index {index_id:?} is registered with an unreadable repo_identity {raw:?}, so \
                 it cannot be verified to belong to {repo_key}{pin_note}"
            )),
        },
    }
}
