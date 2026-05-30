//! GitHub issue-filing client with fingerprint-based deduplication.
//!
//! Why: Phase 3 of the bug-reporting system files or increments GitHub issues
//!      in `bobmatnyc/trusty-tools` using a shared bot token (or later a GitHub
//!      App installation token). The [`GithubApi`] trait decouples the real
//!      reqwest implementation from a mock used in tests, so all filing logic
//!      can be exercised without network access.
//!
//! ## Authentication
//!
//! Tokens are resolved at call time from, in order:
//!   1. An explicit `token` argument supplied by the caller.
//!   2. The `TRUSTY_BUGREPORT_GITHUB_TOKEN` environment variable.
//!   3. A file at `~/.config/trusty-mpm/bugreport-token` (or the path in
//!      `TRUSTY_BUGREPORT_TOKEN_FILE`).
//!
//! If no token is found, `file_issue` returns
//! `Err(GithubFilingError::NoToken)` — nothing is filed, and the caller
//! surfaces an actionable error message to the user.
//!
//! ## GitHub App (Phase 4)
//!
//! The [`TokenProvider`] trait (and `EnvFileTokenProvider`) are defined in
//! [`super::token`]. Phase 4 adds `GithubAppTokenProvider` there. The filing
//! logic here accepts any `dyn TokenProvider` without change.
//!
//! ## Deduplication
//!
//! Before creating a new issue the client searches GitHub for an open issue
//! whose body contains the hidden marker
//! `<!-- trusty-bug-fingerprint: <fp> -->`. If found, it posts a
//! "+1 occurrence" comment on the existing issue. If not found, it creates
//! a new issue with the marker embedded in the body.
//!
//! ## Rate-limit guard
//!
//! [`github_client::RealGithubClient`] refuses to file more than
//! [`MAX_ISSUES_PER_CALL`] issues in a single `file_issue` invocation
//! (currently 1) to prevent accidental spam if the caller loops.
//!
//! ## Module layout
//!
//! - This file (`github.rs`): [`GithubApi`] trait, error type, shared structs,
//!   and the orchestration / dedup logic ([`file_issue`], [`file_issue_with`],
//!   [`extract_fingerprint`]).
//! - [`super::github_client`]: `RealGithubClient` — the `reqwest::blocking`
//!   transport that implements [`GithubApi`].
//! - [`super::github_tests`]: unit tests (mock-based, no network).
//!
//! Test: `tests::token_resolution_*`, `tests::label_mapping_*`,
//!       `tests::dedup_marker_*`, `tests::mock_create_path`,
//!       `tests::mock_comment_path`.

use super::preview::IssuePreview;
use super::token::TokenProvider;
use super::types::FilingResult;
use crate::daemon::bug_report::github_client::RealGithubClient;

/// Maximum issues created in a single `file_issue` call (anti-spam).
///
/// Why: a defensive upper bound prevents accidental bulk filing if a caller
///      loops. Phase 4 will add a richer rate-limit with a local stamp file.
/// What: hardcoded to 1 — one `report_bug` call files at most one issue.
const MAX_ISSUES_PER_CALL: usize = 1;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors returned by the GitHub filing client.
///
/// Why: typed errors let the MCP tool and HTTP handler produce targeted,
///      actionable messages rather than opaque strings.
/// What: each variant represents a distinct failure mode with enough context
///       to form a user-facing message.
/// Test: `tests::no_token_yields_no_token_error`.
#[derive(Debug, thiserror::Error)]
pub enum GithubFilingError {
    /// No bearer token is configured anywhere.
    ///
    /// Why: filing must fail gracefully (not panic, not silently) when the user
    ///      has not configured a token, and must print an actionable message.
    #[error(
        "no bug-report token configured; set TRUSTY_BUGREPORT_GITHUB_TOKEN \
         or write the token to ~/.config/trusty-mpm/bugreport-token \
         (or TRUSTY_BUGREPORT_TOKEN_FILE)"
    )]
    NoToken,

    /// The GitHub API returned a non-2xx status.
    #[error("GitHub API error {status}: {body}")]
    ApiError { status: u16, body: String },

    /// The HTTP transport failed (network error, TLS, etc.).
    #[error("HTTP transport error: {0}")]
    Transport(String),

    /// Response body could not be parsed.
    #[error("failed to parse GitHub API response: {0}")]
    Parse(String),
}

// ── GithubApi trait ───────────────────────────────────────────────────────────

/// Minimal GitHub REST API surface needed by the filing client.
///
/// Why: a trait boundary allows unit tests to inject a mock that records calls
///      and returns canned responses without any network access, satisfying the
///      hard requirement "NO real GitHub calls in tests".
/// What: two methods — `search_open_issues` (find by fingerprint marker) and
///       `create_issue` / `add_comment` (the create vs. increment paths). The
///       real implementation uses `reqwest`; the mock used in tests uses
///       in-memory `Vec`s.
/// Test: `tests::mock_create_path`, `tests::mock_comment_path`.
pub trait GithubApi: Send + Sync {
    /// Search for open issues whose body contains the fingerprint marker.
    ///
    /// Why: dedup requires querying GitHub for any existing open issue before
    ///      creating a new one.
    /// What: calls `GET /search/issues?q=repo:bobmatnyc/trusty-tools+is:issue
    ///       +is:open+"trusty-bug-fingerprint: <fp>"`. Returns the list of
    ///       matching issue URLs and numbers, or an empty vec when none found.
    ///       Returns `Err` on network / API failure.
    /// Test: `tests::mock_create_path` injects an empty result; `tests::mock_comment_path`
    ///       injects a pre-existing issue.
    fn search_open_issues(
        &self,
        fingerprint: &str,
    ) -> Result<Vec<ExistingIssue>, GithubFilingError>;

    /// Create a new GitHub issue.
    ///
    /// Why: the "no existing issue" path in the dedup logic.
    /// What: calls `POST /repos/bobmatnyc/trusty-tools/issues` with title,
    ///       body, and labels. Returns the created issue's URL and number.
    /// Test: `tests::mock_create_path`.
    fn create_issue(
        &self,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<CreatedIssue, GithubFilingError>;

    /// Add a "+1 occurrence" comment to an existing issue.
    ///
    /// Why: the dedup path — re-file as a comment rather than creating a
    ///      duplicate issue.
    /// What: calls `POST /repos/bobmatnyc/trusty-tools/issues/{number}/comments`
    ///       with a short message.
    /// Test: `tests::mock_comment_path`.
    fn add_comment(&self, issue_number: u64, body: &str) -> Result<(), GithubFilingError>;
}

// ── Shared result types ───────────────────────────────────────────────────────

/// A found open issue from a GitHub search result.
///
/// Why: the dedup logic needs the URL and number of any pre-existing issue.
/// What: carries the HTML URL and numeric issue ID returned by the search API.
/// Test: constructed by the mock impl in tests.
#[derive(Debug, Clone)]
pub struct ExistingIssue {
    /// The HTML URL of the existing issue.
    pub html_url: String,
    /// The issue number.
    pub number: u64,
}

/// The result of creating a new GitHub issue.
///
/// Why: the filing client must return the URL and number of the created issue
///      so the caller can surface them in the MCP response.
/// What: carries the HTML URL and numeric issue ID returned by the create API.
/// Test: constructed by the mock impl in tests.
#[derive(Debug, Clone)]
pub struct CreatedIssue {
    /// The HTML URL of the newly-created issue.
    pub html_url: String,
    /// The issue number.
    pub number: u64,
}

// ── Top-level filing function ─────────────────────────────────────────────────

/// File or increment a GitHub issue for the given preview.
///
/// Why: this is the single call that wires token resolution, dedup search,
///      create-vs-comment decision, and anti-spam guard together. Both the
///      MCP `report_bug` confirm path and the HTTP `POST /api/v1/report-bug`
///      confirm path call this function.
/// What:
///   1. Resolves the bearer token via `provider.token()`. Returns
///      `Err(GithubFilingError::NoToken)` immediately if absent.
///   2. Constructs a `RealGithubClient` and calls [`file_issue_with`].
///
/// Test: `tests::no_token_yields_no_token_error` (pure-logic, no network).
pub fn file_issue(
    preview: &IssuePreview,
    provider: &dyn TokenProvider,
) -> Result<FilingResult, GithubFilingError> {
    let token = provider.token().ok_or(GithubFilingError::NoToken)?;
    let client = RealGithubClient::new(token);
    file_issue_with(preview, &client)
}

/// File or increment a GitHub issue using the supplied [`GithubApi`] impl.
///
/// Why: the trait indirection is the seam that lets tests inject a mock without
///      touching `file_issue` (which does the token resolution).
/// What: applies the anti-spam guard (refuses > [`MAX_ISSUES_PER_CALL`] issues
///       per call, which is 1), searches for an existing open issue matching the
///       fingerprint, then either posts a "+1" comment (dedup path) or creates
///       a new issue (create path).
/// Test: `tests::mock_create_path`, `tests::mock_comment_path`.
pub fn file_issue_with(
    preview: &IssuePreview,
    api: &dyn GithubApi,
) -> Result<FilingResult, GithubFilingError> {
    // Anti-spam: this function may only create 1 issue per invocation.
    // (The guard is trivially satisfied here because we call create at most
    // once; it documents the invariant for future multi-fingerprint callers.)
    let _limit = MAX_ISSUES_PER_CALL;

    // Build the dedup comment date string.
    let date_str = chrono::Utc::now().format("%Y-%m-%d").to_string();

    // Search for an existing open issue with this fingerprint.
    let existing = api.search_open_issues(&preview.fingerprint)?;

    if let Some(issue) = existing.into_iter().next() {
        // Dedup path: post a comment on the existing issue.
        let comment_body = format!(
            "+1 occurrence (`{}`, {}/{})\n\nFingerprint: `{}`",
            preview.fingerprint.get(..8).unwrap_or(&preview.fingerprint),
            date_str,
            chrono::Utc::now().format("%H:%M UTC"),
            preview.fingerprint,
        );
        api.add_comment(issue.number, &comment_body)?;
        Ok(FilingResult {
            filed: true,
            deduped: true,
            issue_url: issue.html_url,
            issue_number: issue.number,
        })
    } else {
        // Create path: no existing issue found.
        let created = api.create_issue(&preview.title, &preview.body, &preview.labels)?;
        Ok(FilingResult {
            filed: true,
            deduped: false,
            issue_url: created.html_url,
            issue_number: created.number,
        })
    }
}

/// Extract the fingerprint from an issue body that contains the hidden marker.
///
/// Why: the dedup search returns issue bodies; this helper lets callers verify
///      or extract fingerprints from bodies — useful for logging and in tests.
/// What: scans `body` for `<!-- trusty-bug-fingerprint: <fp> -->` and returns
///       the 64-character fingerprint string, or `None` if the marker is absent.
/// Test: `tests::dedup_marker_extraction`.
#[must_use]
pub fn extract_fingerprint(body: &str) -> Option<String> {
    let prefix = "<!-- trusty-bug-fingerprint: ";
    let suffix = " -->";
    let start = body.find(prefix)? + prefix.len();
    let end = body[start..].find(suffix)? + start;
    Some(body[start..end].trim().to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
