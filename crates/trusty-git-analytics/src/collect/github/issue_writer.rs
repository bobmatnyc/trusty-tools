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
//! [`issue_search_query`] and [`find_thread_by_marker`] are the pure halves,
//! testable with no network at all.
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

/// Pick the thread whose title carries `marker`.
///
/// Why: GitHub's search is a text index, not an exact matcher — it happily
/// returns issues that merely share a token with the marker. Confirming the
/// marker is literally in the title is what keeps one contributor's profile
/// from being appended to another's thread.
/// What: returns the first issue whose `title` contains `marker` as a
/// substring, or `None`.
/// Test: `find_thread_by_marker_ignores_a_near_miss`.
pub fn find_thread_by_marker<'a>(
    items: &'a [GitHubIssue],
    marker: &str,
) -> Option<&'a GitHubIssue> {
    items.iter().find(|i| i.title.contains(marker))
}

// ─── Write methods ────────────────────────────────────────────────────────────

impl GitHubClient {
    /// Run a GitHub issue search and return the matched issues.
    ///
    /// Why: the upsert needs to know whether a contributor already has a thread,
    /// and `/search/issues` is the only endpoint that answers that in one call
    /// across an entire repository's history.
    /// What: `GET /search/issues?q=<query>&per_page=<per_page>`. The query is
    /// percent-encoded by [`reqwest::Url::parse_with_params`], so callers pass
    /// it raw. Search is not a write, but it lives here because the upsert is
    /// the only thing that uses it.
    ///
    /// # Errors
    ///
    /// - [`CollectError::GithubApi`] on a non-success status, carrying GitHub's
    ///   own message — a write denied for token scope says so in the body.
    /// - [`CollectError::Http`] on transport failure or a malformed URL.
    /// - [`CollectError::Json`] on a payload that is not a search page.
    ///
    /// Test: `upsert_comments_on_the_existing_thread_instead_of_opening_a_second`.
    pub async fn search_issues(&self, query: &str, per_page: u32) -> Result<Vec<GitHubIssue>> {
        let per_page = per_page.to_string();
        let url = reqwest::Url::parse_with_params(
            &format!("{}/search/issues", self.api_base()),
            &[("q", query), ("per_page", per_page.as_str())],
        )
        .map_err(|e| CollectError::Config(format!("cannot build GitHub search URL: {e}")))?;

        debug!(url = %url, "GET (issue search)");
        let resp = self.http_client().get(url.clone()).send().await?;
        let page: IssueSearchPage = read_json(resp, url.as_str()).await?;
        Ok(page.items)
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
    /// What: searches with [`issue_search_query`], confirms the match with
    /// [`find_thread_by_marker`], then comments on the hit or creates the issue
    /// with `label` applied. `title` must contain `marker` for the NEXT run to
    /// find what this one created.
    ///
    /// # Errors
    ///
    /// As [`Self::create_issue`]. A search failure aborts rather than falling
    /// through to create — a transient 5xx must not be read as "no thread
    /// exists" and open a duplicate.
    ///
    /// Test: `upsert_creates_when_no_thread_exists`,
    /// `upsert_comments_on_the_existing_thread_instead_of_opening_a_second`.
    pub async fn upsert_issue_thread(
        &self,
        owner: &str,
        repo: &str,
        label: &str,
        title: &str,
        marker: &str,
        body: &str,
    ) -> Result<IssueUpsert> {
        let query = issue_search_query(owner, repo, label, marker);
        let candidates = self.search_issues(&query, SEARCH_PAGE_SIZE).await?;

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

/// Issues requested per search page. One contributor has one thread; 30 is
/// already far more candidates than a marker match should ever produce.
const SEARCH_PAGE_SIZE: u32 = 30;

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
