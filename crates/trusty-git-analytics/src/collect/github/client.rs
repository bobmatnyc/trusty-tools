//! Async GitHub REST API v3 client for pull-request and issue metadata.

use chrono::Utc;
use rusqlite::params;
use tracing::{debug, warn};

use async_trait::async_trait;

use crate::collect::errors::Result;
use crate::collect::github::repo_resolver::{build_http_client, parse_slug};
use crate::collect::github::retry::retry_get;
use crate::collect::github::types::{ApiPull, GitHubIssue, GitHubPrCommit, GitHubReview};
use crate::collect::pr_provider::PrProvider;
use crate::core::config::GithubConfig;
use crate::core::db::Database;
use crate::core::models::{PrState, PullRequest};

/// GitHub REST API base URL.
pub(crate) const GITHUB_API_BASE: &str = "https://api.github.com";
/// Page size for paginated list endpoints (GitHub max is 100).
pub(crate) const PAGE_SIZE: u32 = 100;
/// HTTP `User-Agent` string sent on every request.
pub(crate) const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";

/// Async GitHub REST client.
///
/// Supports single-repo and multi-repo PR collection. The `owner` / `repo`
/// pair is the "primary" repository used by issue-oriented endpoints
/// ([`Self::fetch_issue`], [`Self::list_issues`]). The `repos` vector lists
/// every repository the bulk PR fetcher will iterate over and always contains
/// the primary repo as the first entry when one is set.
pub struct GitHubClient {
    pub(crate) client: reqwest::Client,
    pub(crate) token: Option<String>,
    /// Primary `owner` for issue-oriented endpoints.
    pub(crate) owner: String,
    /// Primary `repo` for issue-oriented endpoints.
    pub(crate) repo: String,
    /// Every `(owner, repo)` pair the PR fetcher will scan, in order. Never
    /// empty in single-repo mode; may contain many entries in org / multi-repo
    /// mode (see [`Self::new_for_prs`]).
    pub(crate) repos: Vec<(String, String)>,
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
pub(crate) fn commit_shas_for_pull(p: &ApiPull) -> Result<String> {
    match (&p.merge_commit_sha, p.merged_at.is_some()) {
        (Some(s), true) => Ok(serde_json::to_string(&vec![s.clone()])?),
        _ => Ok("[]".to_string()),
    }
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
    /// - [`crate::collect::errors::CollectError::Config`] if `repo` is missing or malformed.
    /// - [`crate::collect::errors::CollectError::Http`] if the underlying `reqwest::Client`
    ///   cannot be built.
    pub fn new(config: &GithubConfig) -> Result<Self> {
        use crate::collect::errors::CollectError;
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
    /// Test: covered by `multi_repo_constructor_*` in `client_tests.rs`.
    ///
    /// # Errors
    ///
    /// - [`crate::collect::errors::CollectError::Config`] if `repos` is empty.
    /// - [`crate::collect::errors::CollectError::Http`] if the underlying `reqwest::Client`
    ///   cannot be built.
    pub fn new_for_prs(config: &GithubConfig, repos: Vec<(String, String)>) -> Result<Self> {
        use crate::collect::errors::CollectError;
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
    /// Test: `new_for_reviews_builds_without_dummy_slugs` in `client_tests.rs`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::collect::errors::CollectError::Http`] if the `reqwest::Client`
    /// cannot be built.
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
    /// Returns [`crate::collect::errors::CollectError::Http`] on transport or
    /// non-success status, and [`crate::collect::errors::CollectError::Json`]
    /// on payload parse failures.
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
                    fetched_at: Utc::now().to_rfc3339(),
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
    /// Why: `ON CONFLICT … DO UPDATE` keeps existing `id` so FK-linked
    /// `pr_reviewers` survive re-collection; `INSERT OR REPLACE` wiped them (#752).
    /// A bare `DO UPDATE` still overwrote `state`/`merged_at`/`commit_shas`
    /// unconditionally on every conflict, so a background job re-ingesting an
    /// OLDER snapshot could downgrade an already-`merged` PR back to `open`
    /// (issue #821). The `WHERE excluded.fetched_at > pull_requests.fetched_at`
    /// guard rejects such stale writes: the conflicting row is left untouched
    /// and the statement still succeeds with no error.
    /// What: new rows insert all columns, including `fetched_at`; existing
    /// rows update `title`/`author`/`state`/`merged_at`/`commit_shas`/`fetched_at`
    /// only when the incoming `fetched_at` is strictly newer; `id` and
    /// `created_at` are never overwritten.
    /// Test: reviewer_store tests cover FK-preservation;
    /// `store_pull_requests_stale_write_guard_rejects_older_fetched_at` and
    /// `store_pull_requests_applies_genuinely_newer_fetched_at` (in
    /// `client_tests.rs`) cover the guard itself.
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
                "INSERT INTO pull_requests \
                 (provider,repository,pr_number,title,author,state,created_at,merged_at,commit_shas,fetched_at) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) \
                 ON CONFLICT(provider,repository,pr_number) DO UPDATE SET \
                   title=excluded.title,author=excluded.author,state=excluded.state,\
                   merged_at=excluded.merged_at,commit_shas=excluded.commit_shas,\
                   fetched_at=excluded.fetched_at \
                 WHERE excluded.fetched_at > pull_requests.fetched_at",
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
                    pr.fetched_at,
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
    /// [`crate::collect::errors::CollectError`].
    ///
    /// # Errors
    ///
    /// - [`crate::collect::errors::CollectError::Http`] on transport or non-`404`
    ///   non-success HTTP responses.
    /// - [`crate::collect::errors::CollectError::Json`] on payload parse failures.
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
    /// - [`crate::collect::errors::CollectError::Http`] on transport / non-success
    ///   HTTP responses after retries are exhausted.
    /// - [`crate::collect::errors::CollectError::Json`] on payload parse failures.
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
    /// - [`crate::collect::errors::CollectError::Http`] on transport / non-success
    ///   HTTP responses after retries are exhausted.
    /// - [`crate::collect::errors::CollectError::Json`] on payload parse failures.
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
    /// - [`crate::collect::errors::CollectError::Http`] on transport / non-success
    ///   HTTP responses after retries are exhausted.
    /// - [`crate::collect::errors::CollectError::Json`] on payload parse failures.
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
#[path = "client_tests.rs"]
mod tests;
