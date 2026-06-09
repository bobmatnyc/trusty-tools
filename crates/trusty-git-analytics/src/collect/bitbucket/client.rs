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
//! - **Per-PR commit fetch is skipped** — Bitbucket returns the merge
//!   commit hash directly in the PR list payload, matching what the GitHub
//!   client stores today. This keeps `commit_shas` parity without N extra
//!   round-trips.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use rusqlite::params;
use tracing::{debug, warn};

use crate::collect::bitbucket::types::{BbPaged, BbPullRequest};
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
const PAGE_SIZE: u32 = 50;
/// Maximum retry attempts for transient failures (5xx, 429).
const MAX_RETRIES: u32 = 3;
/// Base delay (in milliseconds) for exponential backoff: 1s, 2s, 4s.
const RETRY_BASE_MS: u64 = 1000;

/// Authentication credentials for the Bitbucket Cloud REST API.
///
/// Bearer wins when both modes are populated — repo / workspace access
/// tokens supersede legacy App Passwords.
#[derive(Debug, Clone)]
enum BbAuth {
    /// `Authorization: Bearer <token>` (workspace or repository access token).
    Bearer(String),
    /// `Authorization: Basic <base64(username:app_password)>`.
    Basic { username: String, password: String },
}

/// Async Bitbucket Cloud REST client.
pub struct BitbucketClient {
    client: reqwest::Client,
    auth: BbAuth,
    workspace: String,
    repo_slug: String,
    api_base: String,
}

impl BitbucketClient {
    /// Build a client from a [`BitbucketConfig`].
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

        let token = config
            .token
            .as_deref()
            .map(crate::collect::env_expand::expand_env_var)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("BITBUCKET_TOKEN")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|s| !s.is_empty())
            });

        let auth = if let Some(t) = token {
            BbAuth::Bearer(t)
        } else {
            let username = config
                .username
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    CollectError::Config(
                        "bitbucket auth missing: provide `token` or `username` + `app_password`"
                            .into(),
                    )
                })?
                .to_string();
            // Why: `app_password` may be written as `${MY_SECRET}` in YAML;
            // without expansion the literal `${MY_SECRET}` is sent as the
            // Basic-auth password, yielding a silent 401 (closes #842).
            // What: expands `${VAR}` placeholders from the environment before
            // trimming, then falls back to `BITBUCKET_APP_PASSWORD` env var —
            // mirroring the identical treatment applied to `token` above.
            // Test: `app_password_env_var_expanded` in this module.
            let password = config
                .app_password
                .as_deref()
                .map(crate::collect::env_expand::expand_env_var)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    std::env::var("BITBUCKET_APP_PASSWORD")
                        .ok()
                        .map(|v| v.trim().to_string())
                        .filter(|s| !s.is_empty())
                })
                .ok_or_else(|| {
                    CollectError::Config(
                        "bitbucket auth missing: app_password (or BITBUCKET_APP_PASSWORD) \
                         required when token is unset"
                            .into(),
                    )
                })?;
            BbAuth::Basic { username, password }
        };

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
            workspace,
            repo_slug,
            api_base,
        })
    }

    /// Apply the configured auth to a request builder.
    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            BbAuth::Bearer(t) => rb.bearer_auth(t),
            BbAuth::Basic { username, password } => rb.basic_auth(username, Some(password)),
        }
    }

    /// Fetch every pull request, following `next` cursors until exhausted.
    ///
    /// State filter is `OPEN,MERGED,DECLINED,SUPERSEDED` — i.e. everything.
    /// Bitbucket's default is open-only, which would drop merged history.
    ///
    /// # Errors
    ///
    /// Returns [`CollectError::Http`] on transport or non-success HTTP
    /// responses, and [`CollectError::Json`] on payload parse failures.
    pub async fn fetch_pull_requests(&self) -> Result<Vec<PullRequest>> {
        let initial = format!(
            "{}/repositories/{}/{}/pullrequests\
             ?state=OPEN&state=MERGED&state=DECLINED&state=SUPERSEDED\
             &pagelen={PAGE_SIZE}",
            self.api_base, self.workspace, self.repo_slug
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
            let repository = format!("{}/{}", self.workspace, self.repo_slug);
            for pr in page.values {
                out.push(map_pr(pr, &repository));
            }
            next_url = page.next;
        }
        Ok(out)
    }

    /// Persist a batch of [`PullRequest`] rows into the database.
    ///
    /// Why: `ON CONFLICT … DO UPDATE` keeps existing `id` so FK-linked `pr_reviewers`
    /// survive re-collection; `INSERT OR REPLACE` cascade-deleted them (#752).
    /// What: `provider='bitbucket'`; `repository="<workspace>/<repo_slug>"`;
    /// deduplicates on `(provider,repository,pr_number)` (0012/#88); existing rows
    /// update `title`/`author`/`state`/`merged_at`/`commit_shas`. Test: see
    /// reviewer_store integration tests for FK-preservation coverage.
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
                 (provider,repository,pr_number,title,author,state,created_at,merged_at,commit_shas) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) \
                 ON CONFLICT(provider,repository,pr_number) DO UPDATE SET \
                   title=excluded.title,author=excluded.author,state=excluded.state,\
                   merged_at=excluded.merged_at,commit_shas=excluded.commit_shas",
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
                ],
            )?;
            count += 1;
        }
        Ok(count)
    }

    /// GET with exponential backoff on transient failures (429, 5xx).
    async fn retry_request(&self, url: &str) -> Result<reqwest::Response> {
        let mut last_err: Option<reqwest::Error> = None;
        for attempt in 0..=MAX_RETRIES {
            debug!(url = %url, attempt, "GET bitbucket (with retry)");
            match self.authed(self.client.get(url)).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let transient =
                        status.as_u16() == 429 || (500..=599).contains(&status.as_u16());
                    if !transient || attempt == MAX_RETRIES {
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
                    if attempt == MAX_RETRIES {
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
    }
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
}

/// Why: keeps `client.rs` under the 500-line file cap; all Bitbucket client
/// unit tests live in the sibling `tests.rs` file.
/// What: `#[path]` overrides Rust's default resolution so a `mod tests;`
/// inside a file module points to the sibling file rather than a subdirectory.
/// Test: see `tests.rs` for the full suite.
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
