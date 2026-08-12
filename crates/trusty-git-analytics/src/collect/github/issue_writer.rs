//! GitHub issue WRITE methods — search, create, comment, and thread upsert.
//!
//! Why: `tga profile --github-issue` publishes a contributor's profile to a
//! GitHub issue that must accumulate over time. Re-running it has to add a
//! comment to the contributor's existing thread, not open a second issue —
//! otherwise a quarterly run leaves four unrelated issues and the trend the
//! profile exists to show is spread across all of them. #5465.
//!
//! What: [`GitHubClient::search_issues`], [`GitHubClient::create_issue`], and
//! [`GitHubClient::create_issue_comment`] are the three REST calls;
//! [`GitHubClient::upsert_issue_thread`] composes them into find-or-create.
//! [`issue_search_query`], [`thread_marker_anchor`], and
//! [`find_thread_by_marker`] are the pure halves, testable with no network.
//!
//! Every ambiguity resolves to an ERROR, never to "create". A duplicate issue
//! on a live tracker is not undone by re-running, so the marker match is
//! anchored rather than a substring test, the search walks its pages rather
//! than reading page one, and a result set larger than the budget is reported
//! as inconclusive.
//!
//! These live beside `client.rs` rather than inside it because that file is
//! already at 289 of its 500 SLOC and this repo splits at the PR that grows the
//! file, not in a follow-up. Every method is an inherent `impl GitHubClient`,
//! so callers see one client.
//!
//! Auth: writes reuse the client's existing `Authorization: Bearer <token>`
//! header — the same personal access token the read path already sends (see
//! `repo_resolver::build_http_client`). Nothing here knows where that token came
//! from, so moving the write path onto a GitHub App installation token means
//! changing how the header is built, not changing these methods.
//!
//! Test: `issue_writer_tests.rs`.

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::collect::errors::{CollectError, Result};
use crate::collect::github::client::GitHubClient;
use crate::collect::github::types::GitHubIssue;

// ─── Wire types ───────────────────────────────────────────────────────────────

/// One page of `GET /search/issues`.
#[derive(Debug, Deserialize)]
struct IssueSearchPage {
    #[serde(default)]
    items: Vec<GitHubIssue>,
    /// Matches GitHub says exist, which is what tells a paged walk whether it
    /// has actually seen all of them.
    #[serde(default)]
    total_count: u64,
}

/// Request body for `POST /repos/{owner}/{repo}/issues`.
#[derive(Debug, Serialize)]
struct CreateIssueBody<'a> {
    title: &'a str,
    body: &'a str,
    labels: &'a [String],
}

/// Request body for `POST /repos/{owner}/{repo}/issues/{number}/comments`.
#[derive(Debug, Serialize)]
struct CreateCommentBody<'a> {
    body: &'a str,
}

// ─── Upsert result ────────────────────────────────────────────────────────────

/// What [`GitHubClient::upsert_issue_thread`] did.
///
/// Why: a caller reporting "posted to <url>" reads the same either way, but
/// "opened a new thread" and "appended to the existing one" are different
/// outcomes — and the second run of a profile that keeps reporting `created`
/// means the marker match is broken.
/// What: the issue's number and web URL, plus whether this call created it.
/// Test: `upsert_creates_when_no_thread_exists`,
/// `upsert_comments_on_the_existing_thread_instead_of_opening_a_second`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IssueUpsert {
    /// Issue number (the `N` in `#N`).
    pub number: u64,
    /// Web URL of the issue thread.
    pub html_url: String,
    /// `true` when this call opened the issue, `false` when it commented.
    pub created: bool,
}

// ─── Pure helpers ─────────────────────────────────────────────────────────────

/// Build the `q` value for the thread-lookup search.
///
/// Why: the search has to be scoped to one repository AND one label, or a
/// marker as common as an email address matches issues in unrelated repos and
/// the upsert comments on a stranger's thread.
/// What: `repo:<owner>/<repo> label:<label> in:title <marker> type:issue`.
/// The caller passes this to [`GitHubClient::search_issues`], which
/// percent-encodes it as a query parameter — so no encoding happens here.
/// Test: `issue_search_query_scopes_to_repo_and_label`.
pub fn issue_search_query(owner: &str, repo: &str, label: &str, marker: &str) -> String {
    format!("repo:{owner}/{repo} label:{label} in:title {marker} type:issue")
}

/// The exact title substring that identifies `marker`'s thread.
///
/// Why: a bare substring test admits INCLUSION — `"[dev-profile] Bob Jones
/// <bob.jones@x.com>".contains("jones@x.com")` is `true`, so a search that
/// returned both Bob's and Jones's issues could append one contributor's
/// review to the other's thread. Writes go to a live tracker and a wrong
/// comment cannot be undone by re-running. Angle brackets close it because
/// they cannot appear inside an email address, so `<a@x>` matches `<a@x>` and
/// nothing that merely ends with it.
/// What: `<marker>`. [`issue_title`](crate::profile::issue_title) always
/// produces a title containing exactly this, and
/// [`GitHubClient::upsert_issue_thread`] refuses a title that does not.
/// Test: `find_thread_by_marker_rejects_an_email_that_contains_the_marker`.
pub fn thread_marker_anchor(marker: &str) -> String {
    format!("<{marker}>")
}

/// Pick the thread whose title carries `marker`, anchored.
///
/// Why: GitHub's search is a token index, not an exact matcher — it returns
/// issues that merely share a token with the marker, so the returned page has
/// to be re-checked locally. That check must not admit substring inclusion; see
/// [`thread_marker_anchor`].
/// What: returns the first issue whose `title` contains
/// [`thread_marker_anchor`]`(marker)`, or `None`.
/// Test: `find_thread_by_marker_ignores_a_near_miss`,
/// `find_thread_by_marker_rejects_an_email_that_contains_the_marker`.
pub fn find_thread_by_marker<'a>(
    items: &'a [GitHubIssue],
    marker: &str,
) -> Option<&'a GitHubIssue> {
    // #5465: anchored, never a bare `contains` — one email can be a substring
    // of another and the write is not undoable.
    let anchor = thread_marker_anchor(marker);
    items.iter().find(|i| i.title.contains(&anchor))
}

// ─── Write methods ────────────────────────────────────────────────────────────

impl GitHubClient {
    /// Run a GitHub issue search and return the matched issues.
    ///
    /// Why: the upsert needs to know whether a contributor already has a thread,
    /// and `/search/issues` is the only endpoint that answers that in one call
    /// across an entire repository's history.
    /// What: `GET /search/issues?q=<query>&per_page=<per_page>&page=<page>`,
    /// returning that page's issues and the `total_count` GitHub reports.
    /// The query is percent-encoded by [`reqwest::Url::parse_with_params`], so
    /// callers pass it raw. Search is not a write, but it lives here because
    /// the upsert is the only thing that uses it.
    ///
    /// # Errors
    ///
    /// - [`CollectError::GithubApi`] on a non-success status, carrying GitHub's
    ///   own message — a write denied for token scope says so in the body.
    /// - [`CollectError::Http`] on transport failure or a malformed URL.
    /// - [`CollectError::Json`] on a payload that is not a search page.
    ///
    /// Test: `upsert_comments_on_the_existing_thread_instead_of_opening_a_second`,
    /// `upsert_walks_past_page_one_to_find_the_thread`.
    pub async fn search_issues(
        &self,
        query: &str,
        per_page: u32,
        page: u32,
    ) -> Result<(Vec<GitHubIssue>, u64)> {
        let per_page = per_page.to_string();
        let page = page.to_string();
        let url = reqwest::Url::parse_with_params(
            &format!("{}/search/issues", self.api_base()),
            &[
                ("q", query),
                ("per_page", per_page.as_str()),
                ("page", page.as_str()),
            ],
        )
        .map_err(|e| CollectError::Config(format!("cannot build GitHub search URL: {e}")))?;

        debug!(url = %url, "GET (issue search)");
        let resp = self.http_client().get(url.clone()).send().await?;
        let body: IssueSearchPage = read_json(resp, url.as_str()).await?;
        Ok((body.items, body.total_count))
    }

    /// Open a new issue on `owner/repo`.
    ///
    /// Why: the first profile run for a contributor has no thread to append to.
    /// What: `POST /repos/{owner}/{repo}/issues` with `title`, `body`, and
    /// `labels`; returns the created issue.
    ///
    /// # Errors
    ///
    /// As [`Self::search_issues`]. A token without `issues: write` on the target
    /// repository fails here with HTTP 403 and GitHub's explanation.
    ///
    /// Test: `upsert_creates_when_no_thread_exists`.
    pub async fn create_issue(
        &self,
        owner: &str,
        repo: &str,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<GitHubIssue> {
        let url = format!("{}/repos/{owner}/{repo}/issues", self.api_base());
        debug!(url = %url, title, "POST (create issue)");

        let resp = self
            .http_client()
            .post(&url)
            .json(&CreateIssueBody {
                title,
                body,
                labels,
            })
            .send()
            .await?;
        read_json(resp, &url).await
    }

    /// Append a comment to an existing issue.
    ///
    /// Why: a contributor's profile thread grows by comment, so each run's
    /// report sits under the previous one and the history stays in one place.
    /// What: `POST /repos/{owner}/{repo}/issues/{number}/comments`.
    ///
    /// # Errors
    ///
    /// As [`Self::create_issue`].
    ///
    /// Test: `upsert_comments_on_the_existing_thread_instead_of_opening_a_second`.
    pub async fn create_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        body: &str,
    ) -> Result<()> {
        let url = format!(
            "{}/repos/{owner}/{repo}/issues/{number}/comments",
            self.api_base()
        );
        debug!(url = %url, "POST (issue comment)");

        let resp = self
            .http_client()
            .post(&url)
            .json(&CreateCommentBody { body })
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &url, resp.text().await.ok()));
        }
        Ok(())
    }

    /// Find the thread carrying `marker` and comment on it, or open it.
    ///
    /// Why: this is the whole point of the write path — one issue per
    /// contributor, appended to on every run. Creating a second issue would
    /// scatter the longitudinal record the profile exists to accumulate.
    /// What: searches with [`issue_search_query`], WALKING pages until the
    /// anchored [`find_thread_by_marker`] hits or the results are exhausted,
    /// then comments on the hit or creates the issue with `label` applied.
    ///
    /// Every way this can end without a definite answer opens a duplicate
    /// issue on a live tracker, which no re-run undoes — so each is an error
    /// instead:
    ///
    /// - a failed search page, rather than reading a 5xx as "no thread exists";
    /// - a `title` that does not carry [`thread_marker_anchor`]`(marker)`,
    ///   since the issue this call created would then be invisible to the next
    ///   run and every run would create another;
    /// - more matches than [`SEARCH_PAGE_BUDGET`] pages can cover, since the
    ///   thread may be among the ones never seen.
    ///
    /// # Errors
    ///
    /// As [`Self::create_issue`], plus [`CollectError::Config`] for an
    /// unanchored title and [`CollectError::GithubSearchInconclusive`] for the
    /// budget case.
    ///
    /// Test: `upsert_creates_when_no_thread_exists`,
    /// `upsert_comments_on_the_existing_thread_instead_of_opening_a_second`,
    /// `upsert_walks_past_page_one_to_find_the_thread`,
    /// `upsert_refuses_a_title_that_the_next_run_could_not_find`.
    pub async fn upsert_issue_thread(
        &self,
        owner: &str,
        repo: &str,
        label: &str,
        title: &str,
        marker: &str,
        body: &str,
    ) -> Result<IssueUpsert> {
        let anchor = thread_marker_anchor(marker);
        if !title.contains(&anchor) {
            return Err(CollectError::Config(format!(
                "issue title '{title}' does not contain '{anchor}'; the thread it \
                 opens would be invisible to the next run, which would open another"
            )));
        }

        let query = issue_search_query(owner, repo, label, marker);
        let mut scanned = 0usize;
        let mut total = 0u64;

        // #5465: one contributor's marker shares tokens with every colleague on
        // the same email domain, so the real thread can sit past page one.
        for page in 1..=SEARCH_PAGE_BUDGET {
            let (candidates, reported) = self.search_issues(&query, SEARCH_PAGE_SIZE, page).await?;
            total = reported;
            scanned += candidates.len();

            if let Some(existing) = find_thread_by_marker(&candidates, marker) {
                self.create_issue_comment(owner, repo, existing.number, body)
                    .await?;
                info!(
                    number = existing.number,
                    url = %existing.html_url,
                    "appended profile to the existing issue thread"
                );
                return Ok(IssueUpsert {
                    number: existing.number,
                    html_url: existing.html_url.clone(),
                    created: false,
                });
            }

            if (candidates.len() as u32) < SEARCH_PAGE_SIZE {
                break;
            }
        }

        if (scanned as u64) < total {
            return Err(CollectError::GithubSearchInconclusive {
                query,
                scanned,
                total,
            });
        }

        let created = self
            .create_issue(owner, repo, title, body, &[label.to_string()])
            .await?;
        info!(
            number = created.number,
            url = %created.html_url,
            "opened a new issue thread for this contributor"
        );
        Ok(IssueUpsert {
            number: created.number,
            html_url: created.html_url,
            created: true,
        })
    }
}

// ─── Response handling ────────────────────────────────────────────────────────

/// Issues requested per search page — GitHub's maximum.
const SEARCH_PAGE_SIZE: u32 = 100;

/// Search pages the thread lookup will walk.
///
/// Why: `in:title alice@example.com` is a TOKEN match, so it also matches every
/// colleague at `example.com` — in an org with hundreds of profiled
/// contributors the real thread can sit well past page one, and stopping early
/// would open a duplicate. GitHub's search API caps at 1000 results, so ten
/// pages of 100 is the whole reachable set; beyond it the search is
/// inconclusive rather than empty.
const SEARCH_PAGE_BUDGET: u32 = 10;

/// Longest GitHub error body kept in an error message.
const MAX_ERROR_BODY: usize = 400;

/// Deserialize a success response, or turn a failure into [`CollectError::GithubApi`].
///
/// Why: `error_for_status()` discards the body, and GitHub puts the actionable
/// part of a write failure there ("Resource not accessible by personal access
/// token"). Losing it is what turns an auth-scope problem into a bare 403.
async fn read_json<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
    endpoint: &str,
) -> Result<T> {
    let status = resp.status();
    if !status.is_success() {
        return Err(api_error(status.as_u16(), endpoint, resp.text().await.ok()));
    }
    Ok(resp.json().await?)
}

/// Build a [`CollectError::GithubApi`], truncating an oversized body.
fn api_error(status: u16, endpoint: &str, body: Option<String>) -> CollectError {
    let mut message = body.unwrap_or_else(|| "(no body)".to_string());
    if message.len() > MAX_ERROR_BODY {
        message.truncate(MAX_ERROR_BODY);
        message.push('…');
    }
    CollectError::GithubApi {
        status,
        endpoint: endpoint.to_string(),
        message,
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "issue_writer_tests.rs"]
mod tests;
