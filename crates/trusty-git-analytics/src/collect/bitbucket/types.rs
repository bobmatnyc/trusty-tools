//! Wire-shape types for Bitbucket Cloud REST API v2.0 responses.
//!
//! Only the fields consumed by the project are deserialized; extras are
//! ignored so a future Bitbucket field rename outside our subset does not
//! break the collector.

use serde::Deserialize;

/// Paginated envelope around any Bitbucket Cloud list endpoint.
///
/// Bitbucket uses cursor-style pagination: the response carries an
/// absolute `next` URL when more pages exist. The pipeline follows
/// `next` until it is `None` rather than incrementing a page counter.
#[derive(Debug, Deserialize)]
pub struct BbPaged<T> {
    /// Items in this page.
    #[serde(default = "Vec::new")]
    pub values: Vec<T>,
    /// Absolute URL of the next page, if any.
    #[serde(default)]
    pub next: Option<String>,
}

/// A single pull-request record as returned by
/// `GET /2.0/repositories/{workspace}/{repo_slug}/pullrequests`.
#[derive(Debug, Deserialize)]
pub struct BbPullRequest {
    /// Bitbucket PR id (per-repo, monotonically increasing).
    pub id: u64,
    /// PR title.
    pub title: String,
    /// One of `OPEN`, `MERGED`, `DECLINED`, `SUPERSEDED`.
    pub state: String,
    /// ISO8601 creation timestamp.
    pub created_on: String,
    /// ISO8601 last-update timestamp. Used as the merge time fallback when
    /// `state == "MERGED"` because Bitbucket does not surface an explicit
    /// `merged_on` on the list endpoint.
    #[serde(default)]
    pub updated_on: Option<String>,
    /// Author of the PR (may be absent if the account was deleted).
    #[serde(default)]
    pub author: Option<BbAuthor>,
    /// Merge commit reference, present once the PR is merged.
    #[serde(default)]
    pub merge_commit: Option<BbCommitRef>,
}

/// Bitbucket account block embedded in PRs and comments.
///
/// Bitbucket exposes `display_name` and `nickname` but only some accounts
/// have a non-empty `nickname` — fall back through both before using the
/// stable `uuid` so we always get *something* sortable for reports.
#[derive(Debug, Deserialize)]
pub struct BbAuthor {
    /// Human-friendly display name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Workspace-scoped nickname (typically the URL-safe handle).
    #[serde(default)]
    pub nickname: Option<String>,
    /// Atlassian account UUID — stable last-resort identifier.
    #[serde(default)]
    pub uuid: Option<String>,
}

impl BbAuthor {
    /// Pick the best available human-readable identifier.
    ///
    /// Priority: `nickname` → `display_name` → `uuid` → empty string.
    pub fn best_name(&self) -> String {
        let pick = |o: &Option<String>| {
            o.as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        };
        pick(&self.nickname)
            .or_else(|| pick(&self.display_name))
            .or_else(|| pick(&self.uuid))
            .unwrap_or_default()
    }
}

/// Commit reference returned inside PR shapes.
#[derive(Debug, Deserialize)]
pub struct BbCommitRef {
    /// Full commit hash (Bitbucket Cloud uses git, so 40-char hex).
    pub hash: String,
}

/// A single commit record as returned by
/// `GET /2.0/repositories/{workspace}/{repo_slug}/pullrequests/{id}/commits`.
///
/// Only `hash` is consumed; the endpoint also returns author/message/date
/// but the collector only needs the SHA to join against `commits.sha`.
#[derive(Debug, Deserialize)]
pub struct BbCommit {
    /// Full commit hash (40-char hex for Bitbucket Cloud git repos).
    pub hash: String,
}
