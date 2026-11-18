//! The GitHub read behind `tm issue next`, and its 10-minute cache.
//!
//! Why: intake runs every factory cycle; the board Status of every issue must
//! come from batched reads, never one call per issue, and a repeat within ten
//! minutes must not call GitHub at all.
//! What: [`IntakeSource`] (the seam the tests fake), [`GhSource`] (paged
//! GraphQL through [`BoardClient`]: one repo query, the label pages, one
//! items query per linked board at 100 a page, the open-issue pages), and
//! [`cached_snapshot`]. A rate-limit answer is [`NextError::RateLimited`].
//! Test: `a_fresh_cache_is_read_without_fetching`,
//! `a_stale_cache_is_refetched`, `a_rate_limit_fails_closed`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::NextError;
use super::model::{BoardStatus, IssueRow, Snapshot};
use crate::commands::issue::board::client::BoardClient;
use crate::commands::issue::board::error::BoardError;
use crate::commands::issue::board::snapshot::{next_cursor, nodes, split_repo, text};

/// Cache lifetime, seconds (owner ruling: 10 minutes).
pub(crate) const CACHE_TTL_SECS: u64 = 600;

/// The board field that carries the lifecycle.
const STATUS_FIELD: &str = "Status";

/// Where a [`Snapshot`] comes from.
pub(crate) trait IntakeSource {
    /// Read `repo`'s intake snapshot.
    fn fetch(&self, repo: &str) -> Result<Snapshot, NextError>;
}

/// [`IntakeSource`] over `gh api graphql`.
pub(crate) struct GhSource<'a> {
    /// The shared board transport.
    pub(crate) client: BoardClient<'a>,
}

/// Whether a `gh` failure text is a rate-limit refusal.
///
/// Test: `a_rate_limit_fails_closed`, `rate_limit_texts_are_recognised`.
pub(crate) fn is_rate_limit(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    lower.contains("rate limit") || lower.contains("rate_limited") || lower.contains("ratelimit")
}

/// Map a board-transport failure to [`NextError`], rate limit first.
pub(crate) fn classify(err: BoardError) -> NextError {
    let detail = err.to_string();
    if is_rate_limit(&detail) {
        NextError::RateLimited { detail }
    } else {
        NextError::Fetch { detail }
    }
}

impl GhSource<'_> {
    fn query(
        &self,
        name: &str,
        doc: &str,
        strings: &[(&str, Option<&str>)],
    ) -> Result<Value, NextError> {
        self.client.query(name, doc, strings, &[]).map_err(classify)
    }

    /// The viewer login and the open boards linked to `repo`, as (id, number).
    fn meta(&self, owner: &str, name: &str) -> Result<(String, Vec<(String, u64)>), NextError> {
        let doc = "query IntakeMeta($owner: String!, $name: String!) { viewer { login } \
                   repository(owner: $owner, name: $name) { projectsV2(first: 20) { nodes { id number closed } } } }";
        let data = self.query(
            "IntakeMeta",
            doc,
            &[("owner", Some(owner)), ("name", Some(name))],
        )?;
        let boards = nodes(&data["repository"]["projectsV2"])
            .iter()
            .filter(|p| p["closed"].as_bool() != Some(true))
            .map(|p| (text(&p["id"]), p["number"].as_u64().unwrap_or(0)))
            .collect();
        Ok((text(&data["viewer"]["login"]), boards))
    }

    fn labels(&self, owner: &str, name: &str) -> Result<Vec<String>, NextError> {
        let doc = "query IntakeLabels($owner: String!, $name: String!, $cursor: String) { \
                   repository(owner: $owner, name: $name) { labels(first: 100, after: $cursor) { \
                   pageInfo { hasNextPage endCursor } nodes { name } } } }";
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let data = self.query(
                "IntakeLabels",
                doc,
                &[
                    ("owner", Some(owner)),
                    ("name", Some(name)),
                    ("cursor", cursor.as_deref()),
                ],
            )?;
            let conn = &data["repository"]["labels"];
            out.extend(nodes(conn).iter().map(|n| text(&n["name"])));
            match next_cursor(conn) {
                Some(c) => cursor = Some(c),
                None => return Ok(out),
            }
        }
    }

    /// One batched read of a board: every item's issue and Status, 100 a page.
    fn board(&self, id: &str, number: u64, repo: &str) -> Result<BoardStatus, NextError> {
        let doc = "query IntakeBoard($id: ID!, $field: String!, $cursor: String) { node(id: $id) { \
                   ... on ProjectV2 { items(first: 100, after: $cursor) { pageInfo { hasNextPage endCursor } \
                   nodes { content { __typename ... on Issue { number state repository { nameWithOwner } } } \
                   fieldValueByName(name: $field) { ... on ProjectV2ItemFieldSingleSelectValue { name } } } } } } }";
        let mut statuses = BTreeMap::new();
        let mut cursor: Option<String> = None;
        loop {
            let data = self.query(
                "IntakeBoard",
                doc,
                &[
                    ("id", Some(id)),
                    ("field", Some(STATUS_FIELD)),
                    ("cursor", cursor.as_deref()),
                ],
            )?;
            let conn = &data["node"]["items"];
            for n in nodes(conn) {
                let c = &n["content"];
                let ours = c["__typename"].as_str() == Some("Issue")
                    && c["state"].as_str() == Some("OPEN")
                    && c["repository"]["nameWithOwner"]
                        .as_str()
                        .is_some_and(|r| r.eq_ignore_ascii_case(repo));
                if let (true, Some(num), Some(status)) = (
                    ours,
                    c["number"].as_u64(),
                    n["fieldValueByName"]["name"].as_str(),
                ) {
                    statuses.insert(num, status.to_string());
                }
            }
            match next_cursor(conn) {
                Some(c) => cursor = Some(c),
                None => return Ok(BoardStatus { number, statuses }),
            }
        }
    }

    fn issues(&self, owner: &str, name: &str) -> Result<Vec<IssueRow>, NextError> {
        let doc = "query IntakeIssues($owner: String!, $name: String!, $cursor: String) { \
                   repository(owner: $owner, name: $name) { issues(states: OPEN, first: 100, after: $cursor, \
                   orderBy: {field: CREATED_AT, direction: ASC}) { pageInfo { hasNextPage endCursor } \
                   nodes { number title createdAt body labels(first: 50) { nodes { name } } \
                   assignees(first: 10) { nodes { login } } } } } }";
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let data = self.query(
                "IntakeIssues",
                doc,
                &[
                    ("owner", Some(owner)),
                    ("name", Some(name)),
                    ("cursor", cursor.as_deref()),
                ],
            )?;
            let conn = &data["repository"]["issues"];
            out.extend(nodes(conn).iter().map(|n| {
                IssueRow {
                    number: n["number"].as_u64().unwrap_or(0),
                    title: text(&n["title"]),
                    created_at: text(&n["createdAt"]),
                    body: text(&n["body"]),
                    labels: nodes(&n["labels"])
                        .iter()
                        .map(|l| text(&l["name"]))
                        .collect(),
                    assignees: nodes(&n["assignees"])
                        .iter()
                        .map(|a| text(&a["login"]))
                        .collect(),
                }
            }));
            match next_cursor(conn) {
                Some(c) => cursor = Some(c),
                None => return Ok(out),
            }
        }
    }
}

impl IntakeSource for GhSource<'_> {
    fn fetch(&self, repo: &str) -> Result<Snapshot, NextError> {
        let (owner, name) = split_repo(repo);
        let (viewer, board_ids) = self.meta(owner, name)?;
        let repo_labels = self.labels(owner, name)?;
        let boards = board_ids
            .iter()
            .map(|(id, number)| self.board(id, *number, repo))
            .collect::<Result<Vec<_>, _>>()?;
        let issues = self.issues(owner, name)?;
        Ok(Snapshot {
            repo: repo.to_string(),
            viewer,
            repo_labels,
            boards,
            issues,
        })
    }
}

/// The cache file's shape.
#[derive(Serialize, Deserialize)]
struct CacheFile {
    fetched_at: u64,
    snapshot: Snapshot,
}

/// `~/.trusty-tools/trusty-mpm/issue-next-cache/<owner>-<repo>.json`.
pub(crate) fn cache_path(repo: &str) -> Option<PathBuf> {
    let base = crate::commands::issue::config::user_config_path()?;
    Some(
        base.parent()?
            .join("issue-next-cache")
            .join(format!("{}.json", repo.replace('/', "-"))),
    )
}

/// `repo`'s snapshot from the cache at `path` when younger than
/// [`CACHE_TTL_SECS`] at `now`, else from `source`, written back to `path`.
///
/// What: an unreadable or stale cache is refetched. A fetch failure is
/// returned as is and the old cache is never used in its place. A cache write
/// failure is reported on stderr and does not fail the read.
/// Test: `a_fresh_cache_is_read_without_fetching`,
/// `a_stale_cache_is_refetched`, `a_rate_limit_fails_closed`.
pub(crate) fn cached_snapshot(
    source: &dyn IntakeSource,
    repo: &str,
    path: Option<&Path>,
    now: u64,
) -> Result<(Snapshot, bool), NextError> {
    if let Some(hit) = path.and_then(|p| read_cache(p, repo, now)) {
        return Ok((hit, true));
    }
    let snapshot = source.fetch(repo)?;
    if let Some(p) = path {
        let file = CacheFile {
            fetched_at: now,
            snapshot: snapshot.clone(),
        };
        let written = p
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                let json = serde_json::to_vec(&file).map_err(std::io::Error::other)?;
                std::fs::write(p, json)
            });
        if let Err(e) = written {
            eprintln!("tm issue next: cache not written to {}: {e}", p.display());
        }
    }
    Ok((snapshot, false))
}

fn read_cache(path: &Path, repo: &str, now: u64) -> Option<Snapshot> {
    let bytes = std::fs::read(path).ok()?;
    let file: CacheFile = serde_json::from_slice(&bytes).ok()?;
    let fresh = now.saturating_sub(file.fetched_at) < CACHE_TTL_SECS && file.fetched_at <= now;
    (fresh && file.snapshot.repo == repo).then_some(file.snapshot)
}
