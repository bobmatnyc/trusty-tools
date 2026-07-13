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
//! Test: request/response handling is NOT exercised in unit tests (network is
//!       mocked at the `GithubApi` trait boundary). Integration tests that use
//!       a real token are gated `#[ignore]`. The timeout bound itself (#2517)
//!       IS unit tested — `tests::http_client_bounds_a_stalled_connection`.

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
/// Ceiling on establishing the TCP connection to `api.github.com`.
///
/// Why (#2517): [`RealGithubClient::http_client`] used to build a
/// `reqwest::blocking::Client` with no timeout at all — a stalled GitHub API
/// endpoint would hang the bug-filing pipeline's `spawn_blocking` task
/// indefinitely. Mirrors the `CONNECT_TIMEOUT_SECS` precedent in
/// `core::sm::providers::{anthropic, openrouter}` for an external API call
/// over the public internet.
/// What: passed to `reqwest::blocking::ClientBuilder::connect_timeout`.
/// Test: `tests::http_client_bounds_a_stalled_connection`.
const GITHUB_CONNECT_TIMEOUT_SECS: u64 = 10;
/// Ceiling on one GitHub REST API request/response round trip.
///
/// Why (#2517): issue search/create/comment calls normally complete in a
/// couple of seconds; 30s is generous slack for a loaded API or slow network
/// path while still bounding a hung request instead of leaving bug-filing
/// unbounded forever.
/// What: passed to `reqwest::blocking::ClientBuilder::timeout`.
/// Test: `tests::http_client_bounds_a_stalled_connection`.
const GITHUB_REQUEST_TIMEOUT_SECS: u64 = 30;

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

/// Build the headered, bounded `reqwest::blocking::Client` [`RealGithubClient::http_client`] uses.
///
/// Why (#2517): factored out as a pure function of its bounds (mirrors
/// `client::http_client::config::build_client`) so the timeout behavior is
/// unit-testable against tiny durations without waiting out the real
/// [`GITHUB_CONNECT_TIMEOUT_SECS`]/[`GITHUB_REQUEST_TIMEOUT_SECS`] production
/// values.
/// What: sets `Accept`, `Authorization: Bearer <token>`, `X-GitHub-Api-Version`,
/// `User-Agent`, and the given connect/request timeout bounds.
/// Test: `tests::http_client_bounds_a_stalled_connection`.
fn build_github_client(
    token: &str,
    connect_timeout: std::time::Duration,
    request_timeout: std::time::Duration,
) -> Result<reqwest::blocking::Client, GithubFilingError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        "application/vnd.github+json".parse().map_err(
            |e: reqwest::header::InvalidHeaderValue| GithubFilingError::Transport(e.to_string()),
        )?,
    );
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {token}")
            .parse()
            .map_err(|e: reqwest::header::InvalidHeaderValue| {
                GithubFilingError::Transport(e.to_string())
            })?,
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
        .connect_timeout(connect_timeout)
        .timeout(request_timeout)
        .build()
        .map_err(|e| GithubFilingError::Transport(e.to_string()))
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
    ///      the boilerplate. Bounded connect/request timeouts (#2517) so a
    ///      stalled GitHub API endpoint cannot hang the bug-filing pipeline
    ///      forever — this client used to have NO timeout at all.
    /// What: sets `Accept`, `Authorization: Bearer`, `X-GitHub-Api-Version`,
    ///       `User-Agent`, [`GITHUB_CONNECT_TIMEOUT_SECS`], and
    ///       [`GITHUB_REQUEST_TIMEOUT_SECS`] on a new blocking client.
    /// Test: indirectly exercised whenever `GithubApi` methods are called;
    ///       `tests::http_client_bounds_a_stalled_connection` pins the
    ///       timeout bound itself against a stalled listener.
    fn http_client(&self) -> Result<reqwest::blocking::Client, GithubFilingError> {
        build_github_client(
            &self.token,
            std::time::Duration::from_secs(GITHUB_CONNECT_TIMEOUT_SECS),
            std::time::Duration::from_secs(GITHUB_REQUEST_TIMEOUT_SECS),
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#2517): `http_client`/`build_github_client` used to build a
    /// `reqwest::blocking::Client` with no timeout at all — a stalled GitHub
    /// API endpoint would hang the bug-filing pipeline's `spawn_blocking`
    /// task indefinitely. Drives a real request against a `TcpListener` that
    /// accepts but never answers (mirrors
    /// `client::http_client::config::tests::build_client_bounds_a_stalled_connection`),
    /// using tiny test-only bounds so the assertion doesn't have to wait out
    /// the real 10s/30s production values.
    /// What: builds a client via [`build_github_client`] with 200ms connect /
    /// 300ms request bounds, issues a GET against the stalled listener (off
    /// the test thread, since this is a blocking client), and asserts the
    /// call errors well within a generous CI margin.
    #[test]
    fn http_client_bounds_a_stalled_connection() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stalling listener");
        let addr = listener.local_addr().expect("read local_addr");

        let client = build_github_client(
            "test-token",
            std::time::Duration::from_millis(200),
            std::time::Duration::from_millis(300),
        )
        .expect("build client");
        let url = format!("http://{addr}/");

        let start = std::time::Instant::now();
        let result = client.get(&url).send();
        let elapsed = start.elapsed();

        // `listener` stays alive (dropped at function end) for the whole
        // request so the connection stalls rather than being refused outright.
        assert!(result.is_err(), "expected the stalled request to time out");
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "request took {elapsed:?}, expected it to be bounded by the ~300ms timeout"
        );
    }

    #[test]
    fn timeout_constants_are_finite() {
        // Why (#2517): pins the production constants so a future edit can't
        // silently widen them back toward "unbounded" without a visible test
        // failure.
        assert_eq!(GITHUB_CONNECT_TIMEOUT_SECS, 10);
        assert_eq!(GITHUB_REQUEST_TIMEOUT_SECS, 30);
    }
}
