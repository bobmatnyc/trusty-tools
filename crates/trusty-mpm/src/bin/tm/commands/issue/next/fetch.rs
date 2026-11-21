//! The GitHub read behind `tm issue next`, and its 10-minute cache.
//!
//! Why: intake runs every factory cycle; the board Status of every issue must
//! come from batched reads, never one call per issue, and a repeat within ten
//! minutes must not call GitHub at all.
//! What: [`IntakeSource`] (the seam the tests fake), [`GhSource`] (paged
//! GraphQL through [`BoardClient`]: one repo query, the label pages, one
//! items query per linked board at 100 a page, the open-issue pages), and
//! [`cached_snapshot`]. A rate-limit answer is [`NextError::RateLimited`]; a
//! reply missing a connection is [`NextError::Fetch`] and is never cached.
//! Test: `a_fresh_cache_is_read_without_fetching`,
//! `a_stale_cache_is_refetched`, `a_rate_limit_fails_closed`,
//! `a_malformed_graphql_reply_fails_closed_and_is_not_cached`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::NextError;
use super::model::{BoardStatus, IssueRow, Snapshot};
use crate::commands::issue::board::client::BoardClient;
use crate::commands::issue::board::error::BoardError;
use crate::commands::issue::board::snapshot::{next_cursor, nodes, split_repo, text};

/// The connection at `path` under `data`, checked for shape.
///
/// What: every key on `path` must be present and non-null, the connection
/// must carry a `nodes` array, and a `hasNextPage: true` page must carry an
/// `endCursor`. Anything else is [`NextError::Fetch`], so a malformed reply
/// fails closed instead of reading as an empty repo (code-critic BLOCK).
/// Test: `a_malformed_graphql_reply_fails_closed_and_is_not_cached`.
fn conn_of<'v>(data: &'v Value, path: &[&str]) -> Result<&'v Value, NextError> {
    let malformed = |why: &str| NextError::Fetch {
        detail: format!("malformed GraphQL reply at `{}`: {why}", path.join(".")),
    };
    let mut v = data;
    for key in path {
        v = v
            .get(key)
            .filter(|x| !x.is_null())
            .ok_or_else(|| malformed(&format!("`{key}` is missing or null")))?;
    }
    if !v["nodes"].is_array() {
        return Err(malformed("no `nodes` array"));
    }
    let page = &v["pageInfo"];
    if page["hasNextPage"].as_bool() == Some(true) && page["endCursor"].as_str().is_none() {
        return Err(malformed("`hasNextPage` without `endCursor`"));
    }
    Ok(v)
}

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
        let login = text(&data["viewer"]["login"]);
        if login.is_empty() {
            return Err(NextError::Fetch {
                detail: "malformed GraphQL reply: `viewer.login` is missing or empty".into(),
            });
        }
        let boards = nodes(conn_of(&data, &["repository", "projectsV2"])?)
            .iter()
            .filter(|p| p["closed"].as_bool() != Some(true))
            .map(|p| (text(&p["id"]), p["number"].as_u64().unwrap_or(0)))
            .collect();
        Ok((login, boards))
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
            let conn = conn_of(&data, &["repository", "labels"])?;
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
            let conn = conn_of(&data, &["node", "items"])?;
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
            let conn = conn_of(&data, &["repository", "issues"])?;
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
/// [`CACHE_TTL_SECS`] at `now` and `refresh` is false, else from `source`,
/// written back to `path`.
///
/// What: an unreadable or stale cache is refetched; `refresh` skips the read
/// and rewrites the file. A fetch failure is returned as is, nothing is
/// written, and the old cache is never used in its place. A cache write
/// failure is reported on stderr and does not fail the read.
/// Test: `a_fresh_cache_is_read_without_fetching`,
/// `a_stale_cache_is_refetched`, `a_rate_limit_fails_closed`,
/// `refresh_bypasses_and_rewrites_the_cache`.
pub(crate) fn cached_snapshot(
    source: &dyn IntakeSource,
    repo: &str,
    path: Option<&Path>,
    now: u64,
    refresh: bool,
) -> Result<(Snapshot, bool), NextError> {
    if !refresh && let Some(hit) = path.and_then(|p| read_cache(p, repo, now)) {
        return Ok((hit, true));
    }
    let snapshot = source.fetch(repo)?;
    if let Some(p) = path {
        let file = CacheFile {
            fetched_at: now,
            snapshot: snapshot.clone(),
        };
        if let Err(e) = write_cache(p, &file) {
            eprintln!("tm issue next: cache not written to {}: {e}", p.display());
        }
    }
    Ok((snapshot, false))
}

/// Write `file` to `path` atomically, private to the user.
///
/// What: the directory is created 0700 (and narrowed to 0700 if it exists);
/// the JSON goes to a 0600 temp file in that directory, then is renamed over
/// `path`, so a reader never sees a half-written file. The cache holds
/// private issue bodies.
/// Test: `the_cache_is_private_to_the_user`.
fn write_cache(path: &Path, file: &CacheFile) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache path has no parent directory"))?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        builder.mode(0o700);
        builder.create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    builder.create(dir)?;
    let json = serde_json::to_vec(file).map_err(std::io::Error::other)?;
    let name = path
        .file_name()
        .map_or_else(|| "cache".into(), |n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    // A leftover temp file keeps its old mode; remove it so `mode` applies.
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = opts
        .open(&tmp)
        .and_then(|mut f| f.write_all(&json).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn read_cache(path: &Path, repo: &str, now: u64) -> Option<Snapshot> {
    let bytes = std::fs::read(path).ok()?;
    let file: CacheFile = serde_json::from_slice(&bytes).ok()?;
    let fresh = now.saturating_sub(file.fetched_at) < CACHE_TTL_SECS && file.fetched_at <= now;
    (fresh && file.snapshot.repo == repo).then_some(file.snapshot)
}
