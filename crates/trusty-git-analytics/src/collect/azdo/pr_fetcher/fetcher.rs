//! `AdoPrFetcher` — HTTP client for ADO pull-request metadata.
//!
//! Why: keeps the network-IO layer separate from the DB-persistence layer so
//! each can be tested in isolation. The fetcher owns its own `reqwest::Client`
//! so it can be used independently of the larger work-item client.
//! What: constructs a minimal authenticated HTTP client, issues
//! `GET {org}/{project}/_apis/git/pullrequests/{id}?api-version=7.1` for each
//! configured project in turn, and drives `run` / `run_with_options` as the
//! top-level collection entry-point.
//! Test: multi-project HTTP behavior covered by the wiremock tests in
//! `pr_fetcher/tests.rs`.

use rusqlite::Connection;
use tracing::{debug, info, warn};

use crate::collect::azdo::errors::AzdoError;
use crate::collect::azdo::pr_fetcher::db::{
    extract_pr_ids, get_existing_pr_numbers, upsert_pr, upsert_pr_reviewer,
};
use crate::collect::azdo::pr_fetcher::types::{AdoPullRequest, PrRaw};
use crate::core::config::AzureDevOpsConfig;
use crate::core::errors::Result as CoreResult;

/// Percent-encode a single path segment (project name).
///
/// Why: ADO project names may contain spaces or other characters that must be
/// encoded before embedding them in a URL path segment.
/// What: percent-encodes every byte that is not an RFC 3986 unreserved character.
/// Test: covered implicitly by `fetch_pr_*` tests that embed project names in URLs.
pub(super) fn encode_segment(s: &str) -> String {
    fn is_unreserved(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
    }
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Minimal ADO PR fetcher. Owns its own `reqwest::Client` so it can be used
/// without keeping the larger work-item client alive.
pub struct AdoPrFetcher {
    pub(super) config: AzureDevOpsConfig,
    pub(super) client: reqwest::Client,
}

impl AdoPrFetcher {
    /// Construct a new fetcher.
    ///
    /// # Errors
    ///
    /// * [`AzdoError::Config`] if `config.projects()` is empty (both
    ///   `project` and `projects` blank/omitted). This is the load-bearing
    ///   invariant that prevents a misconfigured fetcher from being
    ///   constructed — without it, a config with `fetch_prs: true` but no
    ///   `project`/`projects` would silently produce `Ok(None)` from every
    ///   `fetch_pr` call (follow-up to issue #91). URL- and PAT-shape
    ///   checks are delegated to [`ConfigValidator`](crate::core::config::ConfigValidator)
    ///   preflight.
    /// * [`AzdoError::Request`] if the underlying `reqwest::Client`
    ///   cannot be built.
    pub fn new(config: AzureDevOpsConfig) -> std::result::Result<Self, AzdoError> {
        if config.projects().is_empty() {
            return Err(AzdoError::Config(
                "pm.azure_devops.project (or .projects) must not be empty".into(),
            ));
        }

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static(concat!("tga/", env!("CARGO_PKG_VERSION"))),
        );
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(AzdoError::Request)?;
        Ok(Self { config, client })
    }

    fn org_url(&self) -> &str {
        self.config.organization_url.trim_end_matches('/')
    }

    /// Fetch a single PR by ID via the project-scoped endpoint, trying each
    /// configured project in turn until a 200 hit (issue #91).
    ///
    /// Calls `GET {org}/{project}/_apis/git/pullrequests/{pr_id}?api-version=7.1`
    /// for each project from [`AzureDevOpsConfig::projects`]. Returns the PR
    /// paired with the project name it was found in, or `Ok(None)` if every
    /// configured project returns 404.
    ///
    /// Why iterate: ADO PR IDs are project-scoped, so a PR in project B will
    /// 404 against project A. Single-project configs (one project in
    /// `projects()`) issue exactly one request — no overhead. Multi-project
    /// configs stop at first hit (first-hit-wins) to avoid N×P requests.
    ///
    /// # Errors
    ///
    /// * [`AzdoError::Unauthorized`] / [`AzdoError::Forbidden`] on 401/403 from
    ///   any project (auth errors are fatal — we don't keep guessing).
    /// * [`AzdoError::Http`] on any other non-success status.
    /// * [`AzdoError::Request`] on transport failure.
    /// * [`AzdoError::Parse`] on payload parse failure.
    pub async fn fetch_pr(
        &self,
        pr_id: i64,
    ) -> std::result::Result<Option<(AdoPullRequest, String)>, AzdoError> {
        for project in self.config.projects() {
            let url = format!(
                "{}/{}/_apis/git/pullrequests/{pr_id}?api-version=7.1",
                self.org_url(),
                encode_segment(project),
            );
            debug!(url = %url, pr_id, project = %project, "GET ADO PR");

            let resp = self
                .client
                .get(&url)
                .basic_auth("", Some(&self.config.pat))
                .send()
                .await
                .map_err(AzdoError::Request)?;

            match resp.status().as_u16() {
                200 => {
                    let raw: PrRaw = resp
                        .json()
                        .await
                        .map_err(|e| AzdoError::Parse(e.to_string()))?;
                    let pr: AdoPullRequest = raw.into();
                    return Ok(Some((pr, project.to_string())));
                }
                404 => {
                    debug!(pr_id, project = %project, "404 in project; trying next");
                    continue;
                }
                401 => return Err(AzdoError::Unauthorized),
                403 => return Err(AzdoError::Forbidden),
                s => {
                    let message = resp.text().await.unwrap_or_default();
                    return Err(AzdoError::Http { status: s, message });
                }
            }
        }
        Ok(None)
    }

    /// Fetch a batch of PRs serially.
    ///
    /// Serial fetching is intentional: the upstream issue notes that ~7.4
    /// PRs/sec is sufficient for typical analytics windows, and serial calls
    /// keep error handling simple (one bad ID can't poison a parallel batch).
    /// Errors from individual PRs are logged and skipped; the caller gets only
    /// the successful results. Each result is paired with the project name
    /// the PR was found in (issue #91).
    pub async fn fetch_prs(&self, ids: &[i64]) -> Vec<(AdoPullRequest, String)> {
        let mut out = Vec::with_capacity(ids.len());
        for &id in ids {
            match self.fetch_pr(id).await {
                Ok(Some(pair)) => out.push(pair),
                Ok(None) => {
                    debug!(pr_id = id, "ADO PR not found (404), skipping");
                }
                Err(e) => {
                    warn!(pr_id = id, error = %e, "ADO PR fetch failed");
                }
            }
        }
        out
    }

    /// Top-level driver: extract PR IDs from `commit_messages`, skip any
    /// already persisted under provider `'azdo'`, fetch the rest, and write
    /// the PRs and their reviewers to the database.
    ///
    /// Equivalent to [`AdoPrFetcher::run_with_options`] with
    /// `force_refresh = false`. Retained for callers that do not need to
    /// bypass the deduplication cache.
    ///
    /// Returns the number of PR rows newly written / refreshed.
    ///
    /// # Errors
    ///
    /// Returns [`crate::core::TgaError::DbError`] for SQL failures. HTTP failures on
    /// individual PRs are logged and do not abort the whole run.
    pub async fn run<I, S>(&self, conn: &Connection, commit_messages: I) -> CoreResult<usize>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.run_with_options(conn, commit_messages, false).await
    }

    /// Top-level driver with an explicit cache-bypass option.
    ///
    /// Extracts PR IDs from `commit_messages`, optionally skips IDs already
    /// persisted under provider `'azdo'`, fetches the rest, and writes the
    /// PRs and their reviewers to the database.
    ///
    /// When `force_refresh` is `true`, the [`get_existing_pr_numbers`]
    /// deduplication step is bypassed so every referenced PR is re-fetched
    /// and re-upserted.
    ///
    /// Returns the number of PR rows newly written / refreshed.
    ///
    /// # Errors
    ///
    /// Returns [`crate::core::TgaError::DbError`] for SQL failures. HTTP failures on
    /// individual PRs are logged and do not abort the whole run.
    pub async fn run_with_options<I, S>(
        &self,
        conn: &Connection,
        commit_messages: I,
        force_refresh: bool,
    ) -> CoreResult<usize>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let ids = extract_pr_ids(commit_messages);
        if ids.is_empty() {
            info!("No 'Merged PR N:' references found; skipping ADO PR fetch");
            return Ok(0);
        }
        let projects = self.config.projects();
        let to_fetch: Vec<i64> = if force_refresh {
            info!(
                count = ids.len(),
                "force-refresh-prs: bypassing PR-ID dedup cache"
            );
            ids
        } else if projects.len() == 1 {
            let existing = get_existing_pr_numbers(conn, "azdo", projects[0])?;
            ids.into_iter()
                .filter(|id| !existing.contains(id))
                .collect()
        } else {
            debug!(
                projects_len = projects.len(),
                "Multi-project ADO config: skipping cross-project PR cache to avoid masking collisions"
            );
            ids
        };
        if to_fetch.is_empty() {
            info!("All referenced ADO PRs already cached; skipping fetch");
            return Ok(0);
        }
        info!(count = to_fetch.len(), "Fetching ADO PRs");

        let prs = self.fetch_prs(&to_fetch).await;
        let mut stored = 0usize;
        for (pr, project) in &prs {
            let pr_db_id = upsert_pr(conn, pr, project)?;
            for reviewer in &pr.reviewers {
                upsert_pr_reviewer(conn, pr_db_id, reviewer)?;
            }
            stored += 1;
        }
        info!(stored, "Persisted ADO PRs");
        Ok(stored)
    }
}
