//! `reqwest`-backed GitHub REST API v3 transport for the bug-filing pipeline.
//!
//! Why: separating the HTTP transport from the orchestration logic in `github.rs`
//!      keeps each file well under the 500-line hard cap and gives a single,
//!      focused place to evolve the network layer (timeouts, retry, async) without
//!      touching the dedup / rate-limit logic.
//! What: defines the constants (`GITHUB_API`, `REPO`, …), private serde types
//!       (`SearchItem`, `SearchResponse`, `IssueResponse`, `CreateIssueBody`,
//!       `CreateCommentBody`), and [`RealGithubClient`] which implements
//!       [`super::github::GithubApi`] via `reqwest::blocking`.
//! Test: NOT exercised in unit tests (network is mocked). Integration tests that
//!       use a real token are gated `#[ignore]`.

use serde::{Deserialize, Serialize};

use super::github::{CreatedIssue, ExistingIssue, GithubApi, GithubFilingError};

// ── API constants ─────────────────────────────────────────────────────────────

/// GitHub REST API v3 endpoint base.
const GITHUB_API: &str = "https://api.github.com";
/// The target repository (owner/repo).
const REPO: &str = "bobmatnyc/trusty-tools";
/// GitHub API version header value.
const API_VERSION: &str = "2022-11-28";
/// User-agent string for all requests.
const USER_AGENT: &str = concat!("trusty-mpm/", env!("CARGO_PKG_VERSION"));

// ── Private serde types ───────────────────────────────────────────────────────

/// GitHub search API response item.
#[derive(Debug, Deserialize)]
struct SearchItem {
    html_url: String,
    number: u64,
}

/// GitHub search API response envelope.
#[derive(Debug, Deserialize)]
struct SearchResponse {
    items: Vec<SearchItem>,
}

/// GitHub issue create/get response.
#[derive(Debug, Deserialize)]
struct IssueResponse {
    html_url: String,
    number: u64,
}

/// GitHub issue create request body.
#[derive(Debug, Serialize)]
struct CreateIssueBody<'a> {
    title: &'a str,
    body: &'a str,
    labels: &'a [String],
}

/// GitHub comment create request body.
#[derive(Debug, Serialize)]
struct CreateCommentBody<'a> {
    body: &'a str,
}

// ── RealGithubClient ──────────────────────────────────────────────────────────

/// Production GitHub API client using `reqwest` (blocking).
///
/// Why: the filing pipeline runs outside an async context (MCP tools dispatch
///      synchronously on Tokio's task thread via `tokio::task::spawn_blocking`)
///      and the blocking reqwest client is simpler for a one-shot call.
///      A tokio-native async variant can be added in Phase 4 if throughput
///      becomes a concern.
/// What: holds the bearer token; implements [`GithubApi`] via `reqwest::blocking`.
/// Test: NOT exercised in unit tests (network is mocked). Integration tests that
///       use a real token are gated `#[ignore]`.
pub struct RealGithubClient {
    token: String,
}

impl RealGithubClient {
    /// Build a client from an explicit token.
    ///
    /// Why: the filing function resolves the token once before constructing the
    ///      client, so the client does not need its own provider reference.
    /// What: stores the token for `Authorization: Bearer` headers.
    /// Test: constructed by `file_issue` after token resolution succeeds.
    pub fn new(token: String) -> Self {
        Self { token }
    }

    /// Build a default `reqwest::blocking::Client` with the required headers.
    ///
    /// Why: centralises header construction so each API method does not repeat
    ///      the boilerplate.
    /// What: sets `Accept`, `Authorization: Bearer`, `X-GitHub-Api-Version`, and
    ///       `User-Agent` on a new blocking client.
    /// Test: indirectly exercised whenever `GithubApi` methods are called.
    fn http_client(&self) -> Result<reqwest::blocking::Client, GithubFilingError> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            "application/vnd.github+json".parse().map_err(
                |e: reqwest::header::InvalidHeaderValue| {
                    GithubFilingError::Transport(e.to_string())
                },
            )?,
        );
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", self.token).parse().map_err(
                |e: reqwest::header::InvalidHeaderValue| {
                    GithubFilingError::Transport(e.to_string())
                },
            )?,
        );
        headers.insert(
            "X-GitHub-Api-Version",
            API_VERSION
                .parse()
                .map_err(|e: reqwest::header::InvalidHeaderValue| {
                    GithubFilingError::Transport(e.to_string())
                })?,
        );
        reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .build()
            .map_err(|e| GithubFilingError::Transport(e.to_string()))
    }
}

impl GithubApi for RealGithubClient {
    fn search_open_issues(
        &self,
        fingerprint: &str,
    ) -> Result<Vec<ExistingIssue>, GithubFilingError> {
        let client = self.http_client()?;
        // The marker is quoted in the query so GitHub performs a phrase search.
        let query =
            format!(r#"repo:{REPO} is:issue is:open "trusty-bug-fingerprint: {fingerprint}""#);
        let url = format!("{GITHUB_API}/search/issues");
        let resp = client
            .get(&url)
            .query(&[("q", &query)])
            .send()
            .map_err(|e| GithubFilingError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GithubFilingError::ApiError { status, body });
        }

        let search: SearchResponse = resp
            .json()
            .map_err(|e| GithubFilingError::Parse(e.to_string()))?;

        Ok(search
            .items
            .into_iter()
            .map(|item| ExistingIssue {
                html_url: item.html_url,
                number: item.number,
            })
            .collect())
    }

    fn create_issue(
        &self,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<CreatedIssue, GithubFilingError> {
        let client = self.http_client()?;
        let url = format!("{GITHUB_API}/repos/{REPO}/issues");
        let payload = CreateIssueBody {
            title,
            body,
            labels,
        };
        let resp = client
            .post(&url)
            .json(&payload)
            .send()
            .map_err(|e| GithubFilingError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GithubFilingError::ApiError { status, body });
        }

        let issue: IssueResponse = resp
            .json()
            .map_err(|e| GithubFilingError::Parse(e.to_string()))?;

        Ok(CreatedIssue {
            html_url: issue.html_url,
            number: issue.number,
        })
    }

    fn add_comment(&self, issue_number: u64, body: &str) -> Result<(), GithubFilingError> {
        let client = self.http_client()?;
        let url = format!("{GITHUB_API}/repos/{REPO}/issues/{issue_number}/comments");
        let payload = CreateCommentBody { body };
        let resp = client
            .post(&url)
            .json(&payload)
            .send()
            .map_err(|e| GithubFilingError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().unwrap_or_default();
            return Err(GithubFilingError::ApiError { status, body });
        }
        Ok(())
    }
}
