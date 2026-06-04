//! Minimal GitHub REST API v3 client for fetching pull requests.

use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use rusqlite::params;
use serde::Deserialize;
use tracing::{debug, warn};

use async_trait::async_trait;

use crate::collect::env_expand::expand_env_var;
use crate::collect::errors::{CollectError, Result};
use crate::collect::github::retry::retry_get;
use crate::collect::pr_provider::PrProvider;
use crate::core::config::{GithubConfig, RepositoryConfig};
use crate::core::db::Database;
use crate::core::models::{PrState, PullRequest};

/// HTTP `User-Agent` string sent on every request.
const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";
/// GitHub REST API base URL.
pub(crate) const GITHUB_API_BASE: &str = "https://api.github.com";
/// Page size for paginated list endpoints (GitHub max is 100).
pub(crate) const PAGE_SIZE: u32 = 100;

/// Async GitHub REST client.
///
/// Supports single-repo and multi-repo PR collection. The `owner` / `repo`
/// pair is the "primary" repository used by issue-oriented endpoints
/// ([`Self::fetch_issue`], [`Self::list_issues`]). The `repos` vector lists
/// every repository the bulk PR fetcher will iterate over and always contains
/// the primary repo as the first entry when one is set.
pub struct GitHubClient {
    client: reqwest::Client,
    token: Option<String>,
    /// Primary `owner` for issue-oriented endpoints.
    owner: String,
    /// Primary `repo` for issue-oriented endpoints.
    repo: String,
    /// Every `(owner, repo)` pair the PR fetcher will scan, in order. Never
    /// empty in single-repo mode; may contain many entries in org / multi-repo
    /// mode (see [`Self::new_for_prs`]).
    repos: Vec<(String, String)>,
}

#[derive(Debug, Deserialize)]
struct ApiPull {
    number: u64,
    title: String,
    user: Option<ApiUser>,
    state: String,
    created_at: DateTime<Utc>,
    merged_at: Option<DateTime<Utc>>,
    #[serde(default)]
    merge_commit_sha: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiUser {
    login: String,
}

/// Compute the JSON-encoded `commit_shas` value for a PR row.
///
/// Why: GitHub populates `merge_commit_sha` even for open or
/// closed-without-merge PRs — it's the SHA of a *test* merge commit on
/// `refs/pull/N/merge` (a mergeability probe). That SHA exists on no
/// branch and won't join against the `commits` table (issue #101). Only
/// truly merged PRs (`merged_at` set) carry a joinable merge SHA.
/// What: returns `["<sha>"]` only when the PR is merged and has a SHA;
/// otherwise returns the empty array `[]`.
/// Test: see `commit_shas_gated_on_merged_at` — non-merged PR with a
/// populated SHA yields `"[]"`, merged PR yields `r#"["<sha>"]"#`.
fn commit_shas_for_pull(p: &ApiPull) -> Result<String> {
    match (&p.merge_commit_sha, p.merged_at.is_some()) {
        (Some(s), true) => Ok(serde_json::to_string(&vec![s.clone()])?),
        _ => Ok("[]".to_string()),
    }
}

/// A GitHub issue as returned by the REST API.
///
/// This is the normalized payload returned by
/// [`GitHubClient::fetch_issue`]. Only the subset of fields used by the
/// project-management adapter are deserialized.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GitHubIssue {
    /// Issue number (the `N` in `#N`).
    pub number: u64,
    /// Issue title / summary.
    pub title: String,
    /// Workflow state — `"open"` or `"closed"`.
    pub state: String,
    /// Web URL to the issue on github.com.
    pub html_url: String,
    /// Labels applied to the issue.
    #[serde(default)]
    pub labels: Vec<GhLabel>,
    /// Issue body / description (Markdown). May be absent or empty.
    #[serde(default)]
    pub body: Option<String>,
}

/// A GitHub label as returned alongside a [`GitHubIssue`].
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GhLabel {
    /// Label name (e.g. `"bug"`, `"enhancement"`).
    pub name: String,
}

/// A GitHub user reference as embedded in reviews and other payloads.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GhUser {
    /// GitHub login (username).
    pub login: String,
}

/// Embedded git author metadata returned with a PR commit payload.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GhAuthor {
    /// Author display name from the git object.
    pub name: String,
    /// Author email from the git object.
    pub email: String,
    /// Author timestamp (ISO8601). May be absent on some endpoints.
    #[serde(default)]
    pub date: Option<String>,
}

/// Inner `commit` object shape returned by the PR commits endpoint.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GitHubCommitDetail {
    /// Full commit message (subject + body).
    pub message: String,
    /// Optional author block (`name`, `email`, `date`).
    #[serde(default)]
    pub author: Option<GhAuthor>,
}

/// A commit reference returned by the PR commits endpoint
/// (`GET /repos/{owner}/{repo}/pulls/{number}/commits`).
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GitHubPrCommit {
    /// Full 40-char commit SHA.
    pub sha: String,
    /// Nested commit metadata (message, author).
    pub commit: GitHubCommitDetail,
}

/// A pull-request review as returned by
/// `GET /repos/{owner}/{repo}/pulls/{number}/reviews`.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GitHubReview {
    /// Review id.
    pub id: u64,
    /// Review state (`APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, ...).
    pub state: String,
    /// Reviewer user (may be absent for deleted accounts).
    #[serde(default)]
    pub user: Option<GhUser>,
    /// ISO8601 submission timestamp. `None` for pending drafts.
    #[serde(default)]
    pub submitted_at: Option<String>,
}

/// Parse an `owner/name` slug, returning a [`CollectError::Config`] on
/// malformed input. Extracted so both [`GitHubClient::new`] and
/// [`resolve_github_repos`] share one error message format.
fn parse_slug(slug: &str) -> Result<(String, String)> {
    let (owner, repo) = slug.split_once('/').ok_or_else(|| {
        CollectError::Config(format!("github repo must be 'owner/name', got '{slug}'"))
    })?;
    if owner.is_empty() || repo.is_empty() {
        return Err(CollectError::Config(format!(
            "github repo must be 'owner/name', got '{slug}'"
        )));
    }
    Ok((owner.to_string(), repo.to_string()))
}

/// Build the shared authenticated `reqwest::Client` for all GitHub HTTP traffic.
///
/// Why: org-discovery, reviewer-ingestion, and the PR client all need the same
/// authed client; `pub(crate)` visibility avoids duplicating header-build logic
/// without widening the public API surface.
/// What: builds a `reqwest::Client` with `Authorization: Bearer <token>` (when
/// a token is configured), the GitHub `Accept` header, and a 30-second timeout.
/// Test: used by all GitHub call sites — covered indirectly by their tests.
pub(crate) fn build_http_client(config: &GithubConfig) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    if let Some(raw) = &config.token {
        let val = HeaderValue::from_str(&format!("Bearer {}", expand_env_var(raw)))
            .map_err(|e| CollectError::Config(format!("invalid token header: {e}")))?;
        headers.insert(AUTHORIZATION, val);
    }
    Ok(reqwest::Client::builder()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

/// Try to read `origin`'s URL from a local git repository and extract an
/// `owner/name` pair if it looks like a GitHub URL.
///
/// Accepts both `https://github.com/owner/name(.git)?` and
/// `git@github.com:owner/name(.git)?` forms. Returns `None` for non-GitHub
/// remotes, missing `origin`, or anything that fails to parse — the caller
/// then falls back to other resolution rules.
///
/// Why: per-repo entries in `repositories[]` often don't declare an `org:`
/// field; the local clone's remote already encodes the canonical
/// `owner/name`, so probing it is the cheapest correct fallback.
/// What: opens the repo via `git2`, finds the `origin` remote, parses the
/// URL.
/// Test: covered by `extract_owner_repo_from_url` below (URL-parse path) —
/// the disk-touching path is exercised end-to-end via integration tests.
fn owner_repo_from_remote(repo_path: &std::path::Path) -> Option<(String, String)> {
    let repo = git2::Repository::open(repo_path).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    let url = remote.url()?;
    extract_owner_repo_from_url(url)
}

/// Pure-string helper for [`owner_repo_from_remote`]: extracts `owner/name`
/// from a GitHub remote URL string. Returns `None` for non-GitHub URLs or
/// malformed input.
fn extract_owner_repo_from_url(url: &str) -> Option<(String, String)> {
    // Strip `.git` suffix if present.
    let cleaned = url.strip_suffix(".git").unwrap_or(url);
    // SSH form: git@github.com:owner/repo
    if let Some(rest) = cleaned.strip_prefix("git@github.com:") {
        return split_owner_repo(rest);
    }
    // HTTPS form: https://github.com/owner/repo or https://<user>@github.com/owner/repo
    for prefix in [
        "https://github.com/",
        "http://github.com/",
        "ssh://git@github.com/",
    ] {
        if let Some(rest) = cleaned.strip_prefix(prefix) {
            return split_owner_repo(rest);
        }
    }
    // https://user@github.com/owner/repo
    if let Some(after_scheme) = cleaned.strip_prefix("https://") {
        if let Some(at_idx) = after_scheme.find('@') {
            let after_at = &after_scheme[at_idx + 1..];
            if let Some(rest) = after_at.strip_prefix("github.com/") {
                return split_owner_repo(rest);
            }
        }
    }
    None
}

/// Split a `owner/name(/...)` tail into a `(String, String)` pair, taking
/// only the first two path segments. Returns `None` if either is empty.
fn split_owner_repo(rest: &str) -> Option<(String, String)> {
    let mut parts = rest.splitn(3, '/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((owner.to_string(), name.to_string()))
}

/// Resolve the set of `(owner, repo)` pairs the GitHub PR fetcher should
/// scan, given the GitHub config and the project's repository list.
///
/// Resolution rules, tried in order for each repository:
/// 1. `github.repo` (single-repo mode) — when set, returns a single-entry
///    list and ignores `repositories[]` / `github.org`.
/// 2. For each `RepositoryConfig`:
///    - if `repo.org` is set, derive `owner/name` from `org` + repo name
///      (path basename or explicit `name`);
///    - else, try `git remote get-url origin` on `repo.path`;
///    - else, fall back to `github.org` as the owner with the repo's name.
/// 3. Deduplicate; preserve first-seen order.
///
/// Returns an empty vec if no resolution is possible — the caller should
/// treat that as "skip PR fetching". Org-discovered repos (from
/// `github.orgs`) are unioned in by the caller via
/// [`crate::collect::github::org_discovery::resolve_github_repos_with_discovered`].
///
/// Why: org-wide deployments (issue #87) need to drive PR collection from
/// `repositories[]` rather than a single `github.repo`. Mirrors the ADO PR
/// fetcher's per-repo expansion strategy.
/// What: walks the three fallback paths above and returns a deduped vec.
/// Test: `resolve_github_repos_*` cases below.
pub fn resolve_github_repos(
    github: &GithubConfig,
    repositories: &[RepositoryConfig],
) -> Vec<(String, String)> {
    // Mode 1: explicit single-repo slug wins.
    if let Some(slug) = &github.repo {
        if let Ok(pair) = parse_slug(slug) {
            return vec![pair];
        } else {
            tracing::warn!(slug = %slug, "github.repo is malformed; falling back to repositories[]");
        }
    }

    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();

    for repo_cfg in repositories {
        // Repo display name: explicit `name`, else path basename.
        let repo_name = repo_cfg
            .name
            .clone()
            .or_else(|| {
                repo_cfg
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();

        // Owner: per-repo `org`, else `github.org`. We may still defer to
        // the remote URL below when neither is present.
        let owner_from_cfg = repo_cfg.org.clone().or_else(|| github.org.clone());

        let pair = if let Some(owner) = &owner_from_cfg {
            // We have an owner from config — pair it with the repo name. If
            // the repo name is empty (no `name`, no path basename), the
            // remote-URL path below is the only viable resolution.
            if repo_name.is_empty() {
                owner_repo_from_remote(&repo_cfg.path)
            } else {
                Some((owner.clone(), repo_name.clone()))
            }
        } else {
            owner_repo_from_remote(&repo_cfg.path)
        };

        if let Some(p) = pair {
            if seen.insert(p.clone()) {
                out.push(p);
            }
        } else {
            debug!(
                path = %repo_cfg.path.display(),
                "could not resolve owner/repo for repository; skipping for GitHub PR fetch"
            );
        }
    }

    out
}

impl GitHubClient {
    /// Build a client from a [`GithubConfig`].
    ///
    /// The config's `repo` field is expected in `owner/name` form. If the
    /// org-only mode is in use (`org` set, `repo` unset), per-repo calls
    /// will fail until a concrete repo is selected.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Config`] if `repo` is missing or malformed.
    /// - [`CollectError::Http`] if the underlying `reqwest::Client` cannot
    ///   be built.
    pub fn new(config: &GithubConfig) -> Result<Self> {
        let repo_slug = config
            .repo
            .as_ref()
            .ok_or_else(|| CollectError::Config("github.repo is required (owner/name)".into()))?;
        let (owner, repo) = parse_slug(repo_slug)?;
        let http = build_http_client(config)?;

        Ok(Self {
            client: http,
            token: config.token.clone(),
            owner: owner.clone(),
            repo: repo.clone(),
            repos: vec![(owner, repo)],
        })
    }

    /// Construct a client that will fetch pull requests across every
    /// `(owner, repo)` in `repos`.
    ///
    /// Why: org-wide / multi-repo deployments need to drive PR collection
    /// from `repositories[]` (or `github.org` as fallback) rather than a
    /// single `github.repo`. Mirrors the ADO PR-fetcher contract from #84.
    /// What: stores the full list, uses the first entry as the "primary"
    /// for issue-oriented endpoints. Issue endpoints remain single-repo —
    /// the PM adapter still needs a concrete `owner/repo` to hit
    /// `GET /repos/{o}/{r}/issues/{n}`.
    /// Test: covered by `multi_repo_constructor_*` in this module.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Config`] if `repos` is empty.
    /// - [`CollectError::Http`] if the underlying `reqwest::Client` cannot
    ///   be built.
    pub fn new_for_prs(config: &GithubConfig, repos: Vec<(String, String)>) -> Result<Self> {
        if repos.is_empty() {
            return Err(CollectError::Config(
                "GitHubClient::new_for_prs requires at least one (owner, repo)".into(),
            ));
        }
        let (primary_owner, primary_repo) = repos[0].clone();
        let http = build_http_client(config)?;
        Ok(Self {
            client: http,
            token: config.token.clone(),
            owner: primary_owner,
            repo: primary_repo,
            repos,
        })
    }

    /// Construct a minimal authenticated client for fetching PR reviews only.
    ///
    /// Why: the reviewer-ingestion pass needs an authed client to call
    /// `fetch_pr_reviews_for_repo(owner, repo, pr_number)` without requiring
    /// a dummy repo slug (the old `new_for_prs("_dummy","_dummy")` workaround
    /// was fragile — it relied on the reviews method ignoring `self.owner`).
    /// What: builds the authed client; `owner`/`repo`/`repos` are left empty.
    /// Only use methods that take explicit `(owner, repo)` args.
    /// Test: `new_for_reviews_builds_without_dummy_slugs` below.
    ///
    /// # Errors
    ///
    /// Returns [`CollectError::Http`] if the `reqwest::Client` cannot be built.
    pub fn new_for_reviews(config: &GithubConfig) -> Result<Self> {
        let http = build_http_client(config)?;
        Ok(Self {
            client: http,
            token: config.token.clone(),
            owner: String::new(),
            repo: String::new(),
            repos: Vec::new(),
        })
    }

    /// Fetch all PRs (open + closed + merged) by paginating through the
    /// GitHub REST API.
    ///
    /// # Errors
    ///
    /// Returns [`CollectError::Http`] on transport or non-success status,
    /// and [`CollectError::Json`] on payload parse failures.
    pub async fn fetch_pull_requests(&self) -> Result<Vec<PullRequest>> {
        let mut out: Vec<PullRequest> = Vec::new();
        for (owner, repo) in &self.repos {
            match self.fetch_pull_requests_for_repo(owner, repo).await {
                Ok(mut prs) => out.append(&mut prs),
                Err(e) => {
                    // Partial-success semantics (issue #87): one bad repo
                    // (404, no token access, transient 5xx after retries)
                    // must not abort PR collection for the rest of the org.
                    warn!(
                        owner = %owner,
                        repo = %repo,
                        error = %e,
                        "GitHub PR fetch failed for repo; continuing with remaining repos"
                    );
                }
            }
        }
        Ok(out)
    }

    /// Fetch all PRs for a single `(owner, repo)` pair, paginating until
    /// exhausted. Internal helper for [`Self::fetch_pull_requests`].
    async fn fetch_pull_requests_for_repo(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<PullRequest>> {
        let mut out: Vec<PullRequest> = Vec::new();
        let mut page = 1u32;
        loop {
            let url = format!(
                "{GITHUB_API_BASE}/repos/{owner}/{repo}/pulls?state=all&per_page={PAGE_SIZE}&page={page}"
            );
            debug!(url = %url, "GET");
            let resp = self.retry_request(&url).await?;

            // Respect rate-limit hints.
            if let Some(rem) = resp
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u32>().ok())
            {
                if rem < 5 {
                    warn!(remaining = rem, "GitHub rate limit nearly exhausted");
                }
            }

            let resp = resp.error_for_status()?;
            let pulls: Vec<ApiPull> = resp.json().await?;
            if pulls.is_empty() {
                break;
            }
            let n = pulls.len();
            for p in pulls {
                let state = if p.merged_at.is_some() {
                    PrState::Merged
                } else if p.state == "closed" {
                    PrState::Closed
                } else {
                    PrState::Open
                };
                let commit_shas = commit_shas_for_pull(&p)?;
                out.push(PullRequest {
                    id: 0,
                    pr_number: p.number,
                    repository: format!("{owner}/{repo}"),
                    title: p.title,
                    author: p.user.map(|u| u.login).unwrap_or_default(),
                    state,
                    created_at: p.created_at,
                    merged_at: p.merged_at,
                    commit_shas,
                });
            }
            if (n as u32) < PAGE_SIZE {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Persist a batch of [`PullRequest`] rows into the database.
    ///
    /// Existing rows with the same `(provider, repository, pr_number)` are
    /// replaced. The `provider` column is set to `'github'` for all rows
    /// written by this client; `repository` comes from each `PullRequest`
    /// in `"owner/repo"` form (see migrations
    /// `0010_pull_requests_provider.sql` and
    /// `0012_pull_requests_repository.sql`). Including `repository` in the
    /// uniqueness key prevents cross-repo collisions on `pr_number` (fix
    /// for issue #88).
    ///
    /// # Errors
    ///
    /// Propagates [`crate::core::TgaError::DbError`] on SQL failures.
    pub fn store_pull_requests(
        &self,
        db: &Database,
        prs: &[PullRequest],
    ) -> crate::core::Result<usize> {
        let conn = db.connection();
        let mut count = 0usize;
        for pr in prs {
            conn.execute(
                "INSERT OR REPLACE INTO pull_requests \
                 (provider, repository, pr_number, title, author, state, created_at, merged_at, commit_shas) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    "github",
                    pr.repository,
                    pr.pr_number as i64,
                    pr.title,
                    pr.author,
                    pr.state.as_str(),
                    pr.created_at.to_rfc3339(),
                    pr.merged_at.map(|t| t.to_rfc3339()),
                    pr.commit_shas,
                ],
            )?;
            count += 1;
        }
        Ok(count)
    }

    /// Whether this client was constructed with an authentication token.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// Fetch a single issue by number from the GitHub REST API.
    ///
    /// Hits `GET /repos/{owner}/{repo}/issues/{number}`. Uses the same
    /// `Bearer` token (if any) as the bulk PR fetch.
    ///
    /// Returns `Ok(None)` when the API responds with `404 Not Found`
    /// (deleted or invisible issue). All other non-success statuses, as
    /// well as transport and JSON-parse failures, are propagated as
    /// [`CollectError`].
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport or non-`404` non-success HTTP
    ///   responses.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn fetch_issue(&self, number: u64) -> Result<Option<GitHubIssue>> {
        let url = format!(
            "{GITHUB_API_BASE}/repos/{}/{}/issues/{number}",
            self.owner, self.repo
        );
        debug!(url = %url, "GET");
        let resp = self.client.get(&url).send().await?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        let resp = resp.error_for_status()?;
        let issue: GitHubIssue = resp.json().await?;
        Ok(Some(issue))
    }

    /// Send a GET request with exponential backoff on transient failures.
    ///
    /// Retries up to [`MAX_RETRIES`] times on HTTP 429 (rate limit) or any
    /// 5xx response. Delays follow `RETRY_BASE_MS * 2^attempt` — 1s, 2s, 4s
    /// for the default base.
    ///
    /// Why: GitHub occasionally returns 502/504 under load and 429 when the
    /// per-token rate limit drains; a tiny retry loop avoids surfacing those
    /// as pipeline failures.
    /// What: delegates to the free [`retry_get`] helper, passing `self.client`.
    /// Test: covered indirectly by callers and by `wiremock` integration tests.
    async fn retry_request(&self, url: &str) -> Result<reqwest::Response> {
        retry_get(&self.client, url).await
    }

    /// Fetch all reviews for a given pull request, paginating until exhausted.
    ///
    /// Why: review counts, approval status, and review latency are core PR
    /// metrics; the bulk-PR endpoint omits reviews entirely. Taking explicit
    /// `(owner, repo)` rather than using `self.owner`/`self.repo` is
    /// critical for multi-repo clients where the primary owner/repo is
    /// unrelated to the PR being reviewed (issue #742 bug fix — the old
    /// signature silently fetched reviews from the wrong repo).
    /// What: `GET /repos/{owner}/{repo}/pulls/{pr_number}/reviews?per_page=100`,
    /// looping pages until a short page indicates end-of-list.
    /// Test: deserialization shape covered by `github_review_deserializes`;
    /// correct routing verified by the reviewer-ingestion integration path.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses
    ///   after retries are exhausted.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn fetch_pr_reviews_for_repo(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<GitHubReview>> {
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let url = format!(
                "{GITHUB_API_BASE}/repos/{owner}/{repo}/pulls/{pr_number}/reviews?per_page={PAGE_SIZE}&page={page}"
            );
            let resp = self.retry_request(&url).await?.error_for_status()?;
            let batch: Vec<GitHubReview> = resp.json().await?;
            let n = batch.len();
            out.extend(batch);
            if (n as u32) < PAGE_SIZE {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Expose the internal HTTP client for org-discovery requests.
    ///
    /// Why: `discover_org_repos` lives in a sibling module and needs the
    /// same authenticated `reqwest::Client` without duplicating the header
    /// build logic.
    /// What: returns a shared reference to the underlying `reqwest::Client`.
    /// Test: used by the reviewer-ingestion path in `collector.rs`.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Fetch all commits attached to a pull request, paginating until exhausted.
    ///
    /// Why: PR-level commit lists let us attribute work to the PR author and
    /// reconstruct review-window churn even when the merge commit alone is
    /// recorded on the default branch.
    /// What: `GET /repos/{owner}/{repo}/pulls/{pr_number}/commits?per_page=100`.
    /// Test: deserialization shape covered by `github_pr_commit_deserializes`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses
    ///   after retries are exhausted.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn fetch_pr_commits(&self, pr_number: u64) -> Result<Vec<GitHubPrCommit>> {
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let url = format!(
                "{GITHUB_API_BASE}/repos/{}/{}/pulls/{pr_number}/commits?per_page={PAGE_SIZE}&page={page}",
                self.owner, self.repo
            );
            let resp = self.retry_request(&url).await?.error_for_status()?;
            let batch: Vec<GitHubPrCommit> = resp.json().await?;
            let n = batch.len();
            out.extend(batch);
            if (n as u32) < PAGE_SIZE {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// List issues on the configured repository, paginating until exhausted.
    ///
    /// Note: the GitHub `issues` endpoint includes pull requests in its
    /// response. Callers needing pure issues should call [`Self::fetch_pull_requests`]
    /// for PR-specific work.
    ///
    /// Why: bulk issue listing is needed for backfilling ticket metadata
    /// when commit messages reference `#NNN` without a project prefix.
    /// What: `GET /repos/{owner}/{repo}/issues?state={state}&since={since}&per_page=100`.
    /// Test: integration-tested via the `pm` adapter suite; deserialization
    /// reuses `GitHubIssue` whose shape is unit-tested above.
    ///
    /// # Arguments
    ///
    /// * `state` — one of `"open"`, `"closed"`, or `"all"`.
    /// * `since` — optional ISO8601 timestamp; only issues updated at or
    ///   after this time are returned.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses
    ///   after retries are exhausted.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn list_issues(&self, state: &str, since: Option<&str>) -> Result<Vec<GitHubIssue>> {
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let mut url = format!(
                "{GITHUB_API_BASE}/repos/{}/{}/issues?state={state}&per_page={PAGE_SIZE}&page={page}",
                self.owner, self.repo
            );
            if let Some(s) = since {
                url.push_str("&since=");
                url.push_str(s);
            }
            let resp = self.retry_request(&url).await?.error_for_status()?;
            let batch: Vec<GitHubIssue> = resp.json().await?;
            let n = batch.len();
            out.extend(batch);
            if (n as u32) < PAGE_SIZE {
                break;
            }
            page += 1;
        }
        Ok(out)
    }
}

#[async_trait]
impl PrProvider for GitHubClient {
    fn name(&self) -> &str {
        "github"
    }

    async fn fetch_pull_requests(&self) -> Result<Vec<PullRequest>> {
        GitHubClient::fetch_pull_requests(self).await
    }

    fn store_pull_requests(
        &self,
        db: &Database,
        prs: &[PullRequest],
    ) -> crate::core::Result<usize> {
        GitHubClient::store_pull_requests(self, db, prs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Confirm that the wire shape returned by the GitHub Issues API
    /// deserializes into `GitHubIssue` exactly.
    ///
    /// Why: protects against silent schema drift if GitHub renames or
    /// nests one of the fields we depend on.
    /// What: parses a representative JSON document.
    /// Test: assert that all six fields round-trip with expected values.
    #[test]
    fn github_issue_deserializes_full_payload() {
        let json = r#"{
            "number": 42,
            "title": "Crash on startup",
            "state": "open",
            "html_url": "https://github.com/o/r/issues/42",
            "labels": [
                {"name": "bug"},
                {"name": "high-priority"}
            ],
            "body": "Stack trace: ..."
        }"#;
        let issue: GitHubIssue = serde_json::from_str(json).expect("parses");
        assert_eq!(issue.number, 42);
        assert_eq!(issue.title, "Crash on startup");
        assert_eq!(issue.state, "open");
        assert_eq!(issue.html_url, "https://github.com/o/r/issues/42");
        assert_eq!(issue.labels.len(), 2);
        assert_eq!(issue.labels[0].name, "bug");
        assert_eq!(issue.labels[1].name, "high-priority");
        assert_eq!(issue.body.as_deref(), Some("Stack trace: ..."));
    }

    /// `body` and `labels` may be missing — GitHub omits empty arrays in
    /// some response shapes. Confirm the deserializer tolerates that.
    ///
    /// Why: serde defaults must apply, otherwise real API responses fail
    /// to parse.
    /// What: parses a minimal JSON document missing the optional fields.
    /// Test: assert defaults for `labels` (empty) and `body` (`None`).
    /// Verify the wire shape of a PR review payload deserializes correctly.
    ///
    /// Why: `submitted_at` may be `null` for pending reviews and `user`
    /// may be absent for deleted accounts — both must tolerate absence.
    /// What: parses a representative reviews JSON document.
    /// Test: assert state, user.login, and optional fields parse as expected.
    #[test]
    fn github_review_deserializes() {
        let json = r#"{
            "id": 12345,
            "state": "APPROVED",
            "user": {"login": "octocat"},
            "submitted_at": "2024-01-01T00:00:00Z"
        }"#;
        let r: GitHubReview = serde_json::from_str(json).expect("parses");
        assert_eq!(r.id, 12345);
        assert_eq!(r.state, "APPROVED");
        assert_eq!(r.user.as_ref().map(|u| u.login.as_str()), Some("octocat"));
        assert_eq!(r.submitted_at.as_deref(), Some("2024-01-01T00:00:00Z"));

        // Missing optional fields tolerated.
        let pending = r#"{"id": 1, "state": "PENDING"}"#;
        let r2: GitHubReview = serde_json::from_str(pending).expect("parses pending");
        assert!(r2.user.is_none());
        assert!(r2.submitted_at.is_none());
    }

    /// Verify the wire shape of a PR commit payload deserializes correctly.
    ///
    /// Why: PR commit responses nest the message and author under a
    /// `commit` object — the flat git2 shape doesn't apply here.
    /// What: parses a representative `/pulls/{n}/commits` element.
    /// Test: assert sha, message, and author fields all extract.
    #[test]
    fn github_pr_commit_deserializes() {
        let json = r#"{
            "sha": "deadbeefcafebabe",
            "commit": {
                "message": "feat: do the thing",
                "author": {
                    "name": "Ada Lovelace",
                    "email": "ada@example.com",
                    "date": "2024-01-01T00:00:00Z"
                }
            }
        }"#;
        let c: GitHubPrCommit = serde_json::from_str(json).expect("parses");
        assert_eq!(c.sha, "deadbeefcafebabe");
        assert_eq!(c.commit.message, "feat: do the thing");
        let author = c.commit.author.expect("author present");
        assert_eq!(author.name, "Ada Lovelace");
        assert_eq!(author.email, "ada@example.com");
        assert_eq!(author.date.as_deref(), Some("2024-01-01T00:00:00Z"));
    }

    #[test]
    fn github_issue_tolerates_missing_optional_fields() {
        let json = r#"{
            "number": 7,
            "title": "Q",
            "state": "closed",
            "html_url": "https://github.com/o/r/issues/7"
        }"#;
        let issue: GitHubIssue = serde_json::from_str(json).expect("parses");
        assert_eq!(issue.number, 7);
        assert!(issue.labels.is_empty());
        assert!(issue.body.is_none());
    }

    // -----------------------------------------------------------------------
    // Issue #87: multi-repo / org-wide resolution
    // -----------------------------------------------------------------------
    use std::path::PathBuf;

    use crate::core::config::RepositoryConfig;

    fn gh(repo: Option<&str>, org: Option<&str>) -> GithubConfig {
        GithubConfig {
            token: None,
            org: org.map(str::to_string),
            orgs: vec![],
            repo: repo.map(str::to_string),
            fetch_prs: true,
            fetch_pr_reviews: true,
            review_fetch_concurrency: 1,
            ticket_regex: None,
        }
    }

    fn repo_cfg(path: &str, name: Option<&str>, org: Option<&str>) -> RepositoryConfig {
        RepositoryConfig {
            path: PathBuf::from(path),
            name: name.map(str::to_string),
            org: org.map(str::to_string),
            ..Default::default()
        }
    }

    /// Why: `github.repo: owner/name` is the simplest case and must short-
    /// circuit resolution to a single-entry list regardless of what's in
    /// `repositories[]`.
    /// What: passes a single slug, asserts a one-element vec.
    /// Test: exact `(owner, repo)` parsed.
    #[test]
    fn resolve_github_repos_single_repo_mode() {
        let cfg = gh(Some("acme/widget"), None);
        let repos = resolve_github_repos(&cfg, &[]);
        assert_eq!(repos, vec![("acme".to_string(), "widget".to_string())]);
    }

    /// Why: when `github.repo` is unset, an `org`-only config must drive
    /// resolution from `repositories[]` (path basename + `github.org`).
    /// What: two repos with no explicit `org:` field, `github.org=acme`.
    /// Test: both pairs returned with `acme` as owner.
    #[test]
    fn resolve_github_repos_org_mode_uses_path_basename() {
        let cfg = gh(None, Some("acme"));
        let repos = vec![
            repo_cfg("/tmp/widget", None, None),
            repo_cfg("/tmp/gadget", None, None),
        ];
        let resolved = resolve_github_repos(&cfg, &repos);
        assert_eq!(
            resolved,
            vec![
                ("acme".to_string(), "widget".to_string()),
                ("acme".to_string(), "gadget".to_string()),
            ]
        );
    }

    /// Why: per-repo `org:` should override `github.org` for that entry.
    /// What: mix one repo with its own `org` and one without.
    /// Test: first uses per-repo owner, second falls back to `github.org`.
    #[test]
    fn resolve_github_repos_per_repo_org_overrides() {
        let cfg = gh(None, Some("default-org"));
        let repos = vec![
            repo_cfg("/tmp/alpha", None, Some("specific-org")),
            repo_cfg("/tmp/beta", None, None),
        ];
        let resolved = resolve_github_repos(&cfg, &repos);
        assert_eq!(
            resolved,
            vec![
                ("specific-org".to_string(), "alpha".to_string()),
                ("default-org".to_string(), "beta".to_string()),
            ]
        );
    }

    /// Why: explicit `name:` on a repo entry must be preferred over the path
    /// basename so renames and non-canonical directory layouts work.
    /// What: repo with mismatched path and `name`.
    /// Test: resolved name follows the explicit `name`.
    #[test]
    fn resolve_github_repos_uses_explicit_name() {
        let cfg = gh(None, Some("acme"));
        let repos = vec![repo_cfg(
            "/tmp/some-random-clone-dir",
            Some("real-repo-name"),
            None,
        )];
        let resolved = resolve_github_repos(&cfg, &repos);
        assert_eq!(
            resolved,
            vec![("acme".to_string(), "real-repo-name".to_string())]
        );
    }

    /// Why: with neither `github.repo` nor `github.org` (and no remote we
    /// can read for these synthetic paths), resolution must yield an empty
    /// vec so the caller can skip PR fetching gracefully.
    /// What: empty github config + repos with no `org:` and unreadable paths.
    /// Test: empty result.
    #[test]
    fn resolve_github_repos_returns_empty_when_unresolvable() {
        let cfg = gh(None, None);
        let repos = vec![repo_cfg("/tmp/no-such-clone", None, None)];
        let resolved = resolve_github_repos(&cfg, &repos);
        assert!(resolved.is_empty(), "got: {resolved:?}");
    }

    /// Why: with totally empty inputs, resolution must be a clean no-op.
    /// What: no github config slugs, no repositories.
    /// Test: empty result.
    #[test]
    fn resolve_github_repos_empty_inputs() {
        let cfg = gh(None, None);
        let resolved = resolve_github_repos(&cfg, &[]);
        assert!(resolved.is_empty());
    }

    /// Why: duplicate `(owner, repo)` pairs in `repositories[]` (e.g. same
    /// clone listed twice) must dedupe so the fetcher doesn't double-pull.
    /// What: two entries that resolve to the same owner/name.
    /// Test: deduped to one element.
    #[test]
    fn resolve_github_repos_deduplicates() {
        let cfg = gh(None, Some("acme"));
        let repos = vec![
            repo_cfg("/clone-a/widget", None, None),
            repo_cfg("/clone-b/widget", None, None),
        ];
        let resolved = resolve_github_repos(&cfg, &repos);
        assert_eq!(resolved, vec![("acme".to_string(), "widget".to_string())]);
    }

    /// Why: the multi-repo constructor must validate non-empty input — an
    /// empty list represents a programmer error from the orchestrator.
    /// What: call `new_for_prs` with `vec![]`.
    /// Test: returns `CollectError::Config`.
    #[test]
    fn new_for_prs_rejects_empty_repos() {
        let cfg = gh(None, None);
        match GitHubClient::new_for_prs(&cfg, vec![]) {
            Ok(_) => panic!("expected error for empty repos"),
            Err(CollectError::Config(msg)) => {
                assert!(msg.contains("at least one"), "unexpected msg: {msg}")
            }
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }

    /// Why: `new_for_reviews` must build a working client without requiring
    /// any dummy repo slugs; the previous workaround of passing
    /// `("_dummy","_dummy")` was fragile and confusing.
    /// What: call `new_for_reviews` and confirm the client builds successfully
    /// and does not populate owner/repo/repos with dummy values.
    /// Test: owner and repo are empty; repos vec is empty; no panic or error.
    #[test]
    fn new_for_reviews_builds_without_dummy_slugs() {
        let cfg = gh(None, None);
        let client = GitHubClient::new_for_reviews(&cfg).expect("client builds");
        assert!(
            client.owner.is_empty(),
            "owner should be empty for reviews-only client"
        );
        assert!(
            client.repo.is_empty(),
            "repo should be empty for reviews-only client"
        );
        assert!(
            client.repos.is_empty(),
            "repos should be empty for reviews-only client"
        );
    }

    /// Why: the multi-repo constructor must accept a populated list and
    /// expose every entry on `repos`. The first entry doubles as the
    /// "primary" repo for issue endpoints.
    /// What: build a client with two repos and inspect the internal state.
    /// Test: `repos.len() == 2`, primary owner/repo matches index 0.
    #[test]
    fn new_for_prs_stores_all_repos() {
        let cfg = gh(None, Some("acme"));
        let client = GitHubClient::new_for_prs(
            &cfg,
            vec![
                ("acme".into(), "alpha".into()),
                ("acme".into(), "beta".into()),
            ],
        )
        .expect("client builds");
        assert_eq!(client.repos.len(), 2);
        assert_eq!(client.owner, "acme");
        assert_eq!(client.repo, "alpha");
    }

    /// Why: the slug parser is a small but critical helper — bad slugs must
    /// be rejected with a clear message rather than silently producing
    /// `("", "repo")` or similar nonsense.
    /// What: a handful of well- and ill-formed slugs.
    /// Test: positives parse, negatives return `Config` errors.
    #[test]
    fn parse_slug_validates_input() {
        assert_eq!(
            parse_slug("owner/repo").unwrap(),
            ("owner".to_string(), "repo".to_string())
        );
        assert!(parse_slug("no-slash").is_err());
        assert!(parse_slug("/repo").is_err());
        assert!(parse_slug("owner/").is_err());
    }

    /// Why: GitHub remotes come in several URL flavors — the URL parser
    /// must cover the common HTTPS and SSH forms and reject non-GitHub
    /// hosts.
    /// What: probe each supported form and a couple of negative cases.
    /// Test: each call returns the expected `(owner, repo)` or `None`.
    #[test]
    fn extract_owner_repo_from_url_handles_common_forms() {
        assert_eq!(
            extract_owner_repo_from_url("https://github.com/acme/widget.git"),
            Some(("acme".to_string(), "widget".to_string()))
        );
        assert_eq!(
            extract_owner_repo_from_url("https://github.com/acme/widget"),
            Some(("acme".to_string(), "widget".to_string()))
        );
        assert_eq!(
            extract_owner_repo_from_url("git@github.com:acme/widget.git"),
            Some(("acme".to_string(), "widget".to_string()))
        );
        assert_eq!(
            extract_owner_repo_from_url("ssh://git@github.com/acme/widget.git"),
            Some(("acme".to_string(), "widget".to_string()))
        );
        assert_eq!(
            extract_owner_repo_from_url("https://user@github.com/acme/widget"),
            Some(("acme".to_string(), "widget".to_string()))
        );
        // Non-GitHub hosts: unsupported.
        assert!(extract_owner_repo_from_url("https://gitlab.com/acme/widget").is_none());
        assert!(extract_owner_repo_from_url("nonsense").is_none());
    }

    /// Confirm `commit_shas_for_pull` gates the merge SHA on `merged_at`.
    ///
    /// Why: issue #101 — GitHub populates `merge_commit_sha` even for open
    /// or closed-without-merge PRs (a `refs/pull/N/merge` test merge that
    /// exists on no branch), which would write a non-joinable value into
    /// `pull_requests.commit_shas`. Only merged PRs carry a joinable SHA.
    /// What: maps `ApiPull` payloads through `commit_shas_for_pull`.
    /// Test: non-merged PR with a populated SHA yields `"[]"`; a merged PR
    /// with a SHA yields `r#"["some-sha"]"#`.
    #[test]
    fn commit_shas_gated_on_merged_at() {
        // Non-merged PR with a populated (test-merge) SHA -> empty array.
        let json = r#"{
            "number": 101,
            "title": "Open PR",
            "user": {"login": "octocat"},
            "state": "open",
            "created_at": "2024-01-15T10:30:00Z",
            "merged_at": null,
            "merge_commit_sha": "some-sha"
        }"#;
        let p: ApiPull = serde_json::from_str(json).expect("parses");
        assert!(p.merge_commit_sha.is_some());
        assert!(p.merged_at.is_none());
        assert_eq!(
            commit_shas_for_pull(&p).expect("encodes"),
            "[]",
            "non-merged PR with a populated SHA must not emit commit_shas",
        );

        // Closed-without-merge PR with a populated SHA -> empty array.
        let json = r#"{
            "number": 102,
            "title": "Closed-no-merge PR",
            "user": {"login": "octocat"},
            "state": "closed",
            "created_at": "2024-01-15T10:30:00Z",
            "merged_at": null,
            "merge_commit_sha": "some-sha"
        }"#;
        let p: ApiPull = serde_json::from_str(json).expect("parses");
        assert_eq!(
            commit_shas_for_pull(&p).expect("encodes"),
            "[]",
            "closed-without-merge PR must not emit commit_shas",
        );

        // Merged PR with a populated SHA -> joinable single-element array.
        let json = r#"{
            "number": 103,
            "title": "Merged PR",
            "user": {"login": "octocat"},
            "state": "closed",
            "created_at": "2024-01-15T10:30:00Z",
            "merged_at": "2024-01-16T12:00:00Z",
            "merge_commit_sha": "some-sha"
        }"#;
        let p: ApiPull = serde_json::from_str(json).expect("parses");
        assert!(p.merged_at.is_some());
        assert_eq!(
            commit_shas_for_pull(&p).expect("encodes"),
            r#"["some-sha"]"#,
            "merged PR with a SHA should emit a joinable commit_shas array",
        );

        // Merged PR with no SHA at all -> still empty array.
        let json = r#"{
            "number": 104,
            "title": "Merged PR missing SHA",
            "user": {"login": "octocat"},
            "state": "closed",
            "created_at": "2024-01-15T10:30:00Z",
            "merged_at": "2024-01-16T12:00:00Z",
            "merge_commit_sha": null
        }"#;
        let p: ApiPull = serde_json::from_str(json).expect("parses");
        assert_eq!(
            commit_shas_for_pull(&p).expect("encodes"),
            "[]",
            "merged PR without a SHA yields the empty array",
        );
    }
}
