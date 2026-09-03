//! Bitbucket Cloud REST v2.0 client — pull-request collection only.
//!
//! Mirrors the structure of [`crate::collect::github::client`] so the two
//! providers can be reviewed side-by-side. The notable differences:
//!
//! - **Pagination** is cursor-style: each response carries an absolute
//!   `next` URL. We follow `next` until it is `None` rather than walking
//!   a page counter.
//! - **Auth** supports either Bearer token (workspace / repo access token)
//!   or Basic auth (`username` + App Password). Tokens win when both are set.
//! - **Per-PR commit fetch** — the PR list payload only carries the (often
//!   abbreviated) merge commit hash, which cannot be joined against
//!   `commits.sha` and discards the PR's actual commit composition (#841).
//!   [`BitbucketClient::fetch_pr_commits`] hits the dedicated per-PR commits
//!   endpoint, paginating with the same `next`-cursor convention, so
//!   `commit_shas` carries the full list — one extra round-trip (or more,
//!   under pagination) per PR, same tradeoff GitHub's equivalent
//!   `fetch_pr_commits` makes.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use rusqlite::params;
use tracing::{debug, warn};

use crate::collect::bitbucket::types::{BbCommit, BbPaged, BbPullRequest};
use crate::collect::errors::{CollectError, Result};
use crate::collect::pr_provider::PrProvider;
use crate::core::config::BitbucketConfig;
use crate::core::db::Database;
use crate::core::models::{PrState, PullRequest};

/// HTTP `User-Agent` string sent on every request.
const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";
/// Default Bitbucket Cloud REST base URL.
const BITBUCKET_API_BASE: &str = "https://api.bitbucket.org/2.0";
/// Page size for paginated list endpoints (Bitbucket Cloud caps at 50).
pub(crate) const PAGE_SIZE: u32 = 50;
/// Maximum retry attempts for transient failures (5xx, 429).
const MAX_RETRIES: u32 = 3;
/// Base delay (in milliseconds) for exponential backoff: 1s, 2s, 4s.
const RETRY_BASE_MS: u64 = 1000;

/// Authentication credentials for the Bitbucket Cloud REST API.
///
/// Bearer wins when both modes are populated — repo / workspace access
/// tokens supersede legacy App Passwords.
// #5770: no `Debug` derive, deliberately — both variants carry a live
// Bitbucket credential, and nothing formats one, so the derive was pure risk.
#[derive(Clone)]
enum BbAuth {
    /// `Authorization: Bearer <token>` (workspace or repository access token).
    Bearer(String),
    /// `Authorization: Basic <base64(username:app_password)>`.
    Basic { username: String, password: String },
}

/// Resolve Bitbucket auth credentials from config plus an injected env lookup.
///
/// Why: the original logic read `std::env::var` directly, which made the auth
/// unit tests mutate process-global environment variables. Under the
/// multi-threaded test harness those mutations raced across tests, flaking
/// `app_password_falls_back_to_env_when_config_absent` in CI (issue #1653).
/// Threading the environment in as a closure removes the ambient-state
/// coupling: production passes `std::env::var`; tests pass an in-memory map,
/// so no test ever touches the real environment and the race cannot occur.
/// What: applies the documented precedence — (1) Bearer `token` (config,
/// then `BITBUCKET_TOKEN`), else (2) Basic auth `username` + `app_password`
/// (config with `${VAR}` expansion via the same `env` lookup, then
/// `BITBUCKET_APP_PASSWORD`). Config values written as `${MY_SECRET}` are
/// expanded against `env` before use (closes #842). Returns
/// [`CollectError::Config`] when no usable credential is available.
/// Test: `client::tests` — `resolve_auth_*` cases inject env maps directly
/// (no `std::env` mutation), covering bearer, basic, env-expansion,
/// precedence, env-fallback, and the missing-credential error branches.
fn resolve_auth(config: &BitbucketConfig, env: impl Fn(&str) -> Option<String>) -> Result<BbAuth> {
    // Expand `${VAR}` placeholders using the injected env lookup so config
    // and env resolution share one source of truth (no `std::env` here).
    let expand = |raw: &str| -> String {
        if let Some(var) = raw
            .strip_prefix("${")
            .and_then(|s| s.strip_suffix('}'))
            .filter(|v| !v.is_empty())
        {
            env(var).unwrap_or_default()
        } else {
            raw.to_string()
        }
    };
    let clean = |s: String| -> Option<String> {
        let t = s.trim().to_string();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    };

    let token = config
        .token
        .as_deref()
        .map(&expand)
        .and_then(clean)
        .or_else(|| env("BITBUCKET_TOKEN").and_then(clean));

    if let Some(t) = token {
        return Ok(BbAuth::Bearer(t));
    }

    let username = config
        .username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            CollectError::Config(
                "bitbucket auth missing: provide `token` or `username` + `app_password`".into(),
            )
        })?
        .to_string();

    // `app_password` may be written as `${MY_SECRET}` in YAML; without
    // expansion the literal `${MY_SECRET}` is sent as the Basic-auth password,
    // yielding a silent 401 (closes #842). Falls back to the
    // `BITBUCKET_APP_PASSWORD` env var, mirroring `token` above.
    let password = config
        .app_password
        .as_deref()
        .map(&expand)
        .and_then(clean)
        .or_else(|| env("BITBUCKET_APP_PASSWORD").and_then(clean))
        .ok_or_else(|| {
            CollectError::Config(
                "bitbucket auth missing: app_password (or BITBUCKET_APP_PASSWORD) \
                 required when token is unset"
                    .into(),
            )
        })?;

    Ok(BbAuth::Basic { username, password })
}

/// Async Bitbucket Cloud REST client.
pub struct BitbucketClient {
    client: reqwest::Client,
    auth: BbAuth,
    /// `(workspace, repo_slug)` pairs this client collects pull requests for.
    ///
    /// Empty for a client built by [`BitbucketClient::new_for_discovery`],
    /// whose only job is to page `GET /2.0/repositories/{workspace}` (#5220).
    repos: Vec<(String, String)>,
    api_base: String,
    /// Transient-failure retry budget. Always [`MAX_RETRIES`] in production;
    /// tests lower it so a permanent-429 case does not sleep for 7 seconds.
    max_retries: u32,
    /// Bounds this client hit, in operator-facing wording (#6084 shape).
    ///
    /// A workspace listing that stopped at the page cap returns a repository
    /// set that reads exactly like a complete one; the notice recorded here is
    /// what [`crate::collect::pr_pipeline`] turns into a visible run fault.
    notices: Mutex<Vec<String>>,
}

impl BitbucketClient {
    /// Build a single-repository client from a [`BitbucketConfig`].
    ///
    /// Credential precedence:
    /// 1. Bearer `token` (config, then `BITBUCKET_TOKEN` env).
    /// 2. Basic auth: `username` + `app_password` (config, then
    ///    `BITBUCKET_APP_PASSWORD` env).
    ///
    /// # Errors
    ///
    /// - [`CollectError::Config`] if `workspace` / `repo_slug` are missing
    ///   or no usable auth mode is available.
    /// - [`CollectError::Http`] if the underlying `reqwest::Client` cannot
    ///   be built.
    pub fn new(config: &BitbucketConfig) -> Result<Self> {
        let workspace = config
            .workspace
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| CollectError::Config("bitbucket.workspace is required".into()))?
            .to_string();
        let repo_slug = config
            .repo_slug
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| CollectError::Config("bitbucket.repo_slug is required".into()))?
            .to_string();

        Self::build(config, vec![(workspace, repo_slug)])
    }

    /// Build a client over an explicit repository set (#5220).
    ///
    /// Why: workspace discovery answers with many `(workspace, repo_slug)`
    /// pairs, and the collector must fetch pull requests for all of them — the
    /// same shape `GitHubClient::new_for_prs` already takes.
    /// What: same credential and base-URL resolution as [`Self::new`], with the
    /// repository set supplied by the caller instead of read from
    /// `workspace`/`repo_slug`.
    /// Test: `fetch_pull_requests_covers_every_configured_repo`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Config`] if `repos` is empty or no usable auth mode is
    ///   available.
    /// - [`CollectError::Http`] if the `reqwest::Client` cannot be built.
    pub fn new_for_repos(config: &BitbucketConfig, repos: Vec<(String, String)>) -> Result<Self> {
        if repos.is_empty() {
            return Err(CollectError::Config(
                "bitbucket: no repositories to collect — set `repo_slug` or `workspaces`".into(),
            ));
        }
        Self::build(config, repos)
    }

    /// Build a client that carries credentials but no repository set (#5220).
    ///
    /// Why: workspace discovery runs BEFORE any repository is known, and the
    /// common-entry-point rule says it must not construct a second HTTP client
    /// of its own — it borrows this one, headers, auth, retry and all.
    /// What: [`Self::build`] with an empty repository set. Calling
    /// [`Self::fetch_pull_requests`] on the result yields no pull requests,
    /// which is correct: it collects nothing.
    /// Test: `workspace_discovery_follows_next_cursor`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::new_for_repos`], minus the empty-repository-set case.
    pub fn new_for_discovery(config: &BitbucketConfig) -> Result<Self> {
        Self::build(config, Vec::new())
    }

    /// The one place a Bitbucket `reqwest::Client` is constructed.
    fn build(config: &BitbucketConfig, repos: Vec<(String, String)>) -> Result<Self> {
        // Resolve credentials through the live process environment. The
        // env-coupled logic lives in `resolve_auth` so tests can exercise it
        // with an injected lookup instead of mutating `std::env` (issue #1653).
        let auth = resolve_auth(config, |name| std::env::var(name).ok())?;

        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(30))
            .build()?;

        let api_base = config
            .api_base_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_else(|| BITBUCKET_API_BASE.to_string());

        Ok(Self {
            client,
            auth,
            repos,
            api_base,
            max_retries: MAX_RETRIES,
            notices: Mutex::new(Vec::new()),
        })
    }

    /// The REST root every URL this client builds is rooted at.
    pub(crate) fn api_base(&self) -> &str {
        &self.api_base
    }

    /// Carry forward the truncation notices an earlier pass recorded (#5220).
    ///
    /// Why: workspace discovery runs on its own short-lived client, so a
    /// page-cap notice it recorded would die with that client. The collector
    /// hands the notices to the client that actually reaches
    /// [`crate::collect::pr_pipeline`], which is the only place they become a
    /// visible run fault. Same purpose as the shared `RunBudget` the GitHub
    /// clients pass around (#6565), without a second budget type.
    /// What: appends `notices` to this client's ledger, which
    /// [`Self::fetch_notices`] returns.
    /// Test: `a_truncated_workspace_listing_reaches_the_run_stats`.
    #[must_use]
    pub fn with_notices(self, notices: Vec<String>) -> Self {
        if let Ok(mut ledger) = self.notices.lock() {
            ledger.extend(notices);
        }
        self
    }

    /// Record that a bound trimmed a result set (#6084).
    ///
    /// A poisoned lock drops the notice rather than panicking — losing one
    /// operator message must not abort a collection run.
    pub(crate) fn note_truncation(&self, message: impl Into<String>) {
        if let Ok(mut ledger) = self.notices.lock() {
            ledger.push(message.into());
        }
    }

    /// Every truncation recorded so far, in the order it happened.
    pub(crate) fn recorded_notices(&self) -> Vec<String> {
        self.notices
            .lock()
            .map(|l| l.clone())
            .unwrap_or_else(|_| Vec::new())
    }

    /// Authenticated GET with the shared backoff, for the sibling
    /// `workspace_discovery` module (#5220).
    ///
    /// Why: discovery lives in its own file to keep `client.rs` under the SLOC
    /// cap, but must reuse this client's headers, credentials and retry policy
    /// rather than growing a second HTTP path.
    /// What: delegates to [`Self::retry_request`]. The response is returned
    /// unclassified — the caller decides what a non-success status means.
    /// Test: `workspace_discovery_follows_next_cursor`.
    ///
    /// # Errors
    ///
    /// [`CollectError::Http`] once the transport has failed `max_retries` times.
    pub(crate) async fn get_with_retry(&self, url: &str) -> Result<reqwest::Response> {
        self.retry_request(url).await
    }

    /// Lower the retry budget so a test that always answers 429 finishes now.
    ///
    /// Why: [`Self::retry_request`] sleeps 1s, 2s then 4s before giving up, so
    /// the rate-limit arm of the Fail-Open Check would cost 7 real seconds to
    /// prove. This seam is `#[cfg(test)]` so no production path can reach it.
    /// What: replaces [`Self::max_retries`], which defaults to [`MAX_RETRIES`].
    /// Test: `workspace_discovery_names_a_rate_limit`.
    #[cfg(test)]
    pub(crate) fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Apply the configured auth to a request builder.
    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            BbAuth::Bearer(t) => rb.bearer_auth(t),
            BbAuth::Basic { username, password } => rb.basic_auth(username, Some(password)),
        }
    }

    /// Fetch every pull request of every configured repository.
    ///
    /// Why (#5220): a discovered workspace hands over many repositories, and
    /// one 404 among them must not discard the other 199. This mirrors the
    /// per-repo partial-success precedent the GitHub client set in #87.
    /// What: calls [`Self::fetch_repo_pull_requests`] per repository, keeping
    /// what succeeded. A repository that fails is logged and skipped — unless
    /// EVERY repository failed, in which case the first error is returned so
    /// the run never reports an empty result it did not observe.
    /// Test: `fetch_pull_requests_covers_every_configured_repo`,
    /// `one_failing_repo_does_not_discard_the_others`.
    ///
    /// # Errors
    ///
    /// Returns [`CollectError::Http`] on transport or non-success HTTP
    /// responses, and [`CollectError::Json`] on payload parse failures.
    pub async fn fetch_pull_requests(&self) -> Result<Vec<PullRequest>> {
        let mut out: Vec<PullRequest> = Vec::new();
        let mut first_err: Option<CollectError> = None;
        let mut ok = 0usize;
        for (workspace, repo_slug) in &self.repos {
            match self.fetch_repo_pull_requests(workspace, repo_slug).await {
                Ok(prs) => {
                    ok += 1;
                    out.extend(prs);
                }
                Err(e) => {
                    warn!(
                        repository = %format!("{workspace}/{repo_slug}"),
                        error = %e,
                        "Bitbucket PR fetch failed for one repository; continuing"
                    );
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        if ok == 0 {
            if let Some(e) = first_err {
                return Err(e);
            }
        }
        Ok(out)
    }

    /// Fetch every pull request of one repository, following `next` cursors
    /// until exhausted.
    ///
    /// State filter is `OPEN,MERGED,DECLINED,SUPERSEDED` — i.e. everything.
    /// Bitbucket's default is open-only, which would drop merged history.
    ///
    /// # Errors
    ///
    /// Returns [`CollectError::Http`] on transport or non-success HTTP
    /// responses, and [`CollectError::Json`] on payload parse failures.
    async fn fetch_repo_pull_requests(
        &self,
        workspace: &str,
        repo_slug: &str,
    ) -> Result<Vec<PullRequest>> {
        let initial = format!(
            "{}/repositories/{workspace}/{repo_slug}/pullrequests\
             ?state=OPEN&state=MERGED&state=DECLINED&state=SUPERSEDED\
             &pagelen={PAGE_SIZE}",
            self.api_base
        );

        let mut out: Vec<PullRequest> = Vec::new();
        let mut next_url: Option<String> = Some(initial);
        while let Some(url) = next_url.take() {
            debug!(url = %url, "GET (bitbucket)");
            let resp = self.retry_request(&url).await?;

            if let Some(rem) = resp
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u32>().ok())
            {
                if rem < 5 {
                    warn!(remaining = rem, "Bitbucket rate limit nearly exhausted");
                }
            }

            let resp = resp.error_for_status()?;
            let page: BbPaged<BbPullRequest> = resp.json().await?;
            let repository = format!("{workspace}/{repo_slug}");
            for pr in page.values {
                let pr_number = pr.id;
                let mut mapped = map_pr(pr, &repository);
                match self
                    .fetch_pr_commits_in(workspace, repo_slug, pr_number)
                    .await
                {
                    Ok(shas) => {
                        mapped.commit_shas = encode_commit_shas(&shas)?;
                    }
                    Err(e) => {
                        // Graceful degradation (mirrors the per-repo partial-success
                        // precedent in the GitHub client, issue #87): one PR's
                        // commits endpoint hiccuping must not drop the whole PR,
                        // nor abort the rest of the batch. `mapped.commit_shas`
                        // already holds the merge-commit fallback from `map_pr`.
                        warn!(
                            pr_number,
                            error = %e,
                            "Bitbucket PR commit fetch failed; keeping merge-commit fallback"
                        );
                    }
                }
                out.push(mapped);
            }
            next_url = page.next;
        }
        Ok(out)
    }

    /// Fetch every commit SHA attached to a pull request, following `next`
    /// cursors until exhausted.
    ///
    /// Why: the PR list endpoint only carries the merge commit hash (often
    /// abbreviated), which cannot be joined against `commits.sha` by equality
    /// and discards the PR's real commit composition (issue #841). This is
    /// the per-PR enrichment call `fetch_pull_requests` makes for every PR it
    /// returns.
    /// What: delegates to [`Self::fetch_pr_commits_in`] against this client's
    /// first configured repository — for a client built by [`Self::new`] that
    /// is the `workspace`/`repo_slug` pair from the config, unchanged.
    /// Test: `fetch_pr_commits_follows_next_cursor`,
    /// `fetch_pull_requests_persists_full_commit_list`.
    ///
    /// # Errors
    ///
    /// [`CollectError::Config`] when the client has no configured repository —
    /// only reachable through [`Self::new_for_discovery`], which collects
    /// nothing. Otherwise as [`Self::fetch_pr_commits_in`].
    pub async fn fetch_pr_commits(&self, pr_number: u64) -> Result<Vec<String>> {
        // #5220: the signature stays one argument. A discovered workspace made
        // the client multi-repository, and changing the arity here would have
        // been a breaking API change for a caller that has exactly one.
        let (workspace, repo_slug) = self.repos.first().ok_or_else(|| {
            CollectError::Config(
                "bitbucket: this client has no configured repository to read commits from".into(),
            )
        })?;
        self.fetch_pr_commits_in(workspace, repo_slug, pr_number)
            .await
    }

    /// [`Self::fetch_pr_commits`] against a caller-named repository (#5220).
    ///
    /// Why: one client now covers every repository a workspace discovery
    /// returned, so the PR walk names the repository it is enriching rather
    /// than reading a single pair off the client.
    /// What: `GET /2.0/repositories/{workspace}/{repo_slug}/pullrequests/{pr_number}/commits`,
    /// following `next` cursors exactly like [`Self::fetch_pull_requests`].
    /// Test: `fetch_pull_requests_covers_every_configured_repo`,
    /// `fetch_pull_requests_persists_full_commit_list`.
    ///
    /// # Errors
    ///
    /// Returns [`CollectError::Http`] on transport or non-success HTTP
    /// responses, and [`CollectError::Json`] on payload parse failures.
    pub async fn fetch_pr_commits_in(
        &self,
        workspace: &str,
        repo_slug: &str,
        pr_number: u64,
    ) -> Result<Vec<String>> {
        let initial = format!(
            "{}/repositories/{workspace}/{repo_slug}/pullrequests/{pr_number}/commits\
             ?pagelen={PAGE_SIZE}",
            self.api_base
        );

        let mut out: Vec<String> = Vec::new();
        let mut next_url: Option<String> = Some(initial);
        while let Some(url) = next_url.take() {
            debug!(url = %url, "GET (bitbucket pr commits)");
            let resp = self.retry_request(&url).await?.error_for_status()?;
            let page: BbPaged<BbCommit> = resp.json().await?;
            out.extend(page.values.into_iter().map(|c| c.hash));
            next_url = page.next;
        }
        Ok(out)
    }

    /// Persist a batch of [`PullRequest`] rows into the database.
    ///
    /// Why: `ON CONFLICT … DO UPDATE` keeps existing `id` so FK-linked `pr_reviewers`
    /// survive re-collection; `INSERT OR REPLACE` cascade-deleted them (#752).
    /// A bare `DO UPDATE` still overwrote `state`/`merged_at`/`commit_shas`
    /// unconditionally on every conflict, so a background job re-ingesting an
    /// OLDER snapshot could downgrade an already-`merged` PR back to `open`
    /// (issue #821). The `WHERE excluded.fetched_at > pull_requests.fetched_at`
    /// guard rejects such stale writes: the conflicting row is left untouched
    /// and the statement still succeeds with no error.
    /// What: `provider='bitbucket'`; `repository="<workspace>/<repo_slug>"`;
    /// deduplicates on `(provider,repository,pr_number)` (0012/#88); existing rows
    /// update `title`/`author`/`state`/`merged_at`/`commit_shas`/`fetched_at` only
    /// when the incoming `fetched_at` is strictly newer. Test: see
    /// reviewer_store integration tests for FK-preservation coverage;
    /// `store_pull_requests_stale_write_guard_rejects_older_fetched_at` and
    /// `store_pull_requests_applies_genuinely_newer_fetched_at` (in `tests.rs`)
    /// cover the guard itself.
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
                    "bitbucket",
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

    /// GET with exponential backoff on transient failures (429, 5xx).
    async fn retry_request(&self, url: &str) -> Result<reqwest::Response> {
        let mut last_err: Option<reqwest::Error> = None;
        for attempt in 0..=self.max_retries {
            debug!(url = %url, attempt, "GET bitbucket (with retry)");
            match self.authed(self.client.get(url)).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let transient =
                        status.as_u16() == 429 || (500..=599).contains(&status.as_u16());
                    if !transient || attempt == self.max_retries {
                        return Ok(resp);
                    }
                    let delay = RETRY_BASE_MS * (1u64 << attempt);
                    warn!(
                        status = %status,
                        attempt,
                        delay_ms = delay,
                        "Bitbucket returned transient status; retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                Err(e) => {
                    if attempt == self.max_retries {
                        return Err(CollectError::Http(e));
                    }
                    let delay = RETRY_BASE_MS * (1u64 << attempt);
                    warn!(error = %e, attempt, delay_ms = delay,
                          "Bitbucket transport error; retrying");
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
        }
        Err(CollectError::Http(
            last_err.expect("retry loop preserved error"),
        ))
    }
}

/// Map a Bitbucket PR shape into the project's internal [`PullRequest`].
///
/// `repository` is provided by the caller (formatted `"workspace/repo_slug"`)
/// so that the row carries enough identity to participate in the
/// `(provider, repository, pr_number)` unique constraint added in migration
/// v12 (#88).
///
/// State mapping: `MERGED` → [`PrState::Merged`]; `OPEN` → [`PrState::Open`];
/// `DECLINED` and `SUPERSEDED` collapse to [`PrState::Closed`] because the
/// shared schema has no richer variants. The collapse is lossy but documented
/// in the configuration reference.
///
/// `commit_shas` here is seeded from the list payload's (often abbreviated)
/// `merge_commit` hash only as a fallback; [`BitbucketClient::fetch_pull_requests`]
/// overwrites it with the full per-PR commit list from
/// [`BitbucketClient::fetch_pr_commits`] whenever that call succeeds (#841).
fn map_pr(pr: BbPullRequest, repository: &str) -> PullRequest {
    let state = match pr.state.as_str() {
        "MERGED" => PrState::Merged,
        "OPEN" => PrState::Open,
        _ => PrState::Closed,
    };

    let created_at = parse_ts(&pr.created_on).unwrap_or_else(Utc::now);
    let merged_at = if matches!(state, PrState::Merged) {
        pr.updated_on.as_deref().and_then(parse_ts)
    } else {
        None
    };

    let commit_shas = match pr.merge_commit.as_ref().map(|c| c.hash.clone()) {
        Some(h) if !h.is_empty() => serde_json::to_string(&vec![h]).unwrap_or_else(|_| "[]".into()),
        _ => "[]".into(),
    };

    let author = pr
        .author
        .as_ref()
        .map(|a| a.best_name())
        .unwrap_or_default();

    PullRequest {
        id: 0,
        pr_number: pr.id,
        repository: repository.to_string(),
        title: pr.title,
        author,
        state,
        created_at,
        merged_at,
        commit_shas,
        fetched_at: Utc::now().to_rfc3339(),
        // #5734: Bitbucket's list payload does carry `source.branch.name` and
        // `description`, but neither is deserialized yet. `None` states that
        // this provider makes no claim, rather than asserting an empty branch.
        head_ref: None,
        body_ticket_id: None,
    }
}

/// Serialize a full commit SHA list into the JSON-array string stored in
/// `pull_requests.commit_shas`.
///
/// Why: shares the on-disk convention with the GitHub provider's
/// `commit_shas_for_pull` (a JSON array of SHA strings) so downstream
/// consumers — e.g. the `commits.sha` join — don't need per-provider
/// parsing.
/// What: `serde_json::to_string(shas)`. `Vec<String>` serialization cannot
/// fail in practice, but the `Result` return keeps the call site symmetric
/// with the rest of this module's fallible JSON handling.
/// Test: `fetch_pull_requests_persists_full_commit_list`.
fn encode_commit_shas(shas: &[String]) -> Result<String> {
    Ok(serde_json::to_string(shas)?)
}

/// Parse a Bitbucket ISO8601 timestamp into UTC.
///
/// Bitbucket sometimes returns offsets like `+00:00` and sometimes `Z`;
/// chrono's RFC3339 parser handles both. On parse failure the caller falls
/// back to `Utc::now()` so a single malformed row doesn't poison the batch.
fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| Utc.from_utc_datetime(&dt.naive_utc()))
}

#[async_trait]
impl PrProvider for BitbucketClient {
    fn name(&self) -> &str {
        "bitbucket"
    }

    async fn fetch_pull_requests(&self) -> Result<Vec<PullRequest>> {
        BitbucketClient::fetch_pull_requests(self).await
    }

    fn store_pull_requests(
        &self,
        db: &Database,
        prs: &[PullRequest],
    ) -> crate::core::Result<usize> {
        BitbucketClient::store_pull_requests(self, db, prs)
    }

    /// #5220: surfaces the workspace-discovery page cap the same way the
    /// GitHub client surfaces its listing caps (#6084).
    fn fetch_notices(&self) -> Vec<String> {
        self.recorded_notices()
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
