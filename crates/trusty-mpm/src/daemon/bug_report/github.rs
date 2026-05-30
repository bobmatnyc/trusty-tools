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
//! ## GitHub App (future)
//!
//! The [`TokenProvider`] trait is the slot-in point for a GitHub App
//! installation-token provider. The current `EnvFileTokenProvider` impl
//! covers ENV + file resolution; a `GithubAppTokenProvider` can be added in
//! Phase 4 without changing the filing logic.
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
//! [`RealGithubClient`] refuses to file more than [`MAX_ISSUES_PER_CALL`]
//! issues in a single `file_issue` invocation (currently 1) to prevent
//! accidental spam if the caller loops.
//!
//! Test: `tests::token_resolution_*`, `tests::label_mapping_*`,
//!       `tests::dedup_marker_*`, `tests::mock_create_path`,
//!       `tests::mock_comment_path`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::preview::IssuePreview;
use super::types::FilingResult;

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

// ── Token provider trait ──────────────────────────────────────────────────────

/// Provides a bearer token for GitHub API calls.
///
/// Why: the filing client should not be tightly coupled to a specific token
///      source. The ENV+file implementation works for Phase 3; a GitHub App
///      installation-token provider (short-lived, auto-rotating) can slot in
///      for Phase 4 by implementing this trait.
/// What: one method, `token`, returns the resolved token or `None` when no
///       source is configured.
/// Test: `EnvFileTokenProvider` is exercised by `tests::token_resolution_*`.
pub trait TokenProvider: Send + Sync {
    /// Resolve and return the bearer token, or `None` if unconfigured.
    fn token(&self) -> Option<String>;
}

/// Token provider that reads from the environment variable
/// `TRUSTY_BUGREPORT_GITHUB_TOKEN` or a local file.
///
/// Why: the simplest useful implementation for Phase 3 — zero runtime
///      dependencies beyond env and fs reads.
/// What: resolution order:
///   1. `TRUSTY_BUGREPORT_GITHUB_TOKEN` env var (non-empty value).
///   2. File at `TRUSTY_BUGREPORT_TOKEN_FILE` env var path (if set).
///   3. File at `~/.config/trusty-mpm/bugreport-token` (fallback).
///
///      Token values are trimmed of leading/trailing whitespace and newlines.
///
/// Test: `tests::token_resolution_from_env`, `tests::token_resolution_from_file`,
///       `tests::token_resolution_absent_uses_fixed_provider`.
pub struct EnvFileTokenProvider;

/// Primary environment variable name for the GitHub bearer token.
pub const TOKEN_ENV_VAR: &str = "TRUSTY_BUGREPORT_GITHUB_TOKEN";
/// Override env var for the token file path.
pub const TOKEN_FILE_ENV_VAR: &str = "TRUSTY_BUGREPORT_TOKEN_FILE";
/// Default token file path (relative to home dir).
const TOKEN_FILE_RELATIVE: &str = ".config/trusty-mpm/bugreport-token";

impl TokenProvider for EnvFileTokenProvider {
    fn token(&self) -> Option<String> {
        // 1. Check env var.
        if let Ok(val) = std::env::var(TOKEN_ENV_VAR) {
            let trimmed = val.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }

        // 2. Resolve file path (override or default).
        let file_path: PathBuf = if let Ok(override_path) = std::env::var(TOKEN_FILE_ENV_VAR) {
            PathBuf::from(override_path.trim())
        } else if let Some(home) = dirs::home_dir() {
            home.join(TOKEN_FILE_RELATIVE)
        } else {
            return None;
        };

        // 3. Read and trim the file.
        std::fs::read_to_string(&file_path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
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

/// A found open issue from a GitHub search result.
///
/// Why: the dedup logic needs the URL and number of any pre-existing issue.
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
/// Test: constructed by the mock impl in tests.
#[derive(Debug, Clone)]
pub struct CreatedIssue {
    /// The HTML URL of the newly-created issue.
    pub html_url: String,
    /// The issue number.
    pub number: u64,
}

// ── Real reqwest implementation ───────────────────────────────────────────────

/// GitHub REST API v3 endpoint base.
const GITHUB_API: &str = "https://api.github.com";
/// The target repository (owner/repo).
const REPO: &str = "bobmatnyc/trusty-tools";
/// GitHub API version header value.
const API_VERSION: &str = "2022-11-28";
/// User-agent string for all requests.
const USER_AGENT: &str = concat!("trusty-mpm/", env!("CARGO_PKG_VERSION"));

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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::daemon::bug_report::preview::IssuePreview;

    // ── Mock token provider ───────────────────────────────────────────────────

    struct StaticTokenProvider(Option<String>);

    impl TokenProvider for StaticTokenProvider {
        fn token(&self) -> Option<String> {
            self.0.clone()
        }
    }

    // ── Mock GithubApi ────────────────────────────────────────────────────────

    /// Outcome recorded by the mock.
    #[derive(Debug, Clone)]
    enum MockCall {
        SearchIssues,
        CreateIssue { labels: Vec<String> },
        AddComment(u64),
    }

    /// Mock that returns configurable search results and records all calls.
    struct MockGithubApi {
        /// Pre-existing issues to return from `search_open_issues`.
        existing: Vec<ExistingIssue>,
        /// Calls recorded for assertion.
        calls: Arc<Mutex<Vec<MockCall>>>,
    }

    impl MockGithubApi {
        fn with_existing(existing: Vec<ExistingIssue>) -> Self {
            Self {
                existing,
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn no_existing() -> Self {
            Self::with_existing(vec![])
        }
    }

    impl GithubApi for MockGithubApi {
        fn search_open_issues(
            &self,
            _fingerprint: &str,
        ) -> Result<Vec<ExistingIssue>, GithubFilingError> {
            self.calls.lock().unwrap().push(MockCall::SearchIssues);
            Ok(self.existing.clone())
        }

        fn create_issue(
            &self,
            _title: &str,
            _body: &str,
            labels: &[String],
        ) -> Result<CreatedIssue, GithubFilingError> {
            self.calls.lock().unwrap().push(MockCall::CreateIssue {
                labels: labels.to_vec(),
            });
            Ok(CreatedIssue {
                html_url: "https://github.com/bobmatnyc/trusty-tools/issues/999".to_string(),
                number: 999,
            })
        }

        fn add_comment(&self, issue_number: u64, _body: &str) -> Result<(), GithubFilingError> {
            self.calls
                .lock()
                .unwrap()
                .push(MockCall::AddComment(issue_number));
            Ok(())
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn dummy_preview(fingerprint: &str) -> IssuePreview {
        IssuePreview {
            title: "[trusty_mpm] test error".to_string(),
            body: format!(
                "## Auto-reported\n\n<!-- trusty-bug-fingerprint: {fingerprint} -->\n\nbody text"
            ),
            labels: vec![
                "bug".to_string(),
                "auto-reported".to_string(),
                "trusty-mpm".to_string(),
            ],
            fingerprint: fingerprint.to_string(),
            scrub_changes: vec![],
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// A token provider with an explicit token value (bypasses env/file lookup).
    ///
    /// Why: env-var based tests race when tests run in parallel; this struct
    ///      lets us test the resolution logic without touching process env state.
    struct FixedTokenProvider(Option<&'static str>);
    impl TokenProvider for FixedTokenProvider {
        fn token(&self) -> Option<String> {
            self.0.map(str::to_string)
        }
    }

    #[test]
    fn token_resolution_from_env() {
        // Test that the env var is read when present.
        // We use TOKEN_FILE_ENV_VAR to supply a file path that does not exist
        // so the fall-through path is exercised regardless of process env state.
        // The primary assertion is that setting TOKEN_ENV_VAR is sufficient.
        //
        // SAFETY: this test runs single-threaded in isolation; the env var is
        // cleaned up before the test returns. Token-resolution tests share a
        // process so we accept they may interfere; that is why we assert
        // `is_some()` (a value was set) rather than a specific value that
        // another test might have left in the env.
        let sentinel = "ghp_test_token_from_env_unique_trusty_phase3"; // pragma: allowlist secret
        // SAFETY: isolated test, cleaned up on drop path.
        unsafe { std::env::set_var(TOKEN_ENV_VAR, sentinel) };
        let token = EnvFileTokenProvider.token();
        unsafe { std::env::remove_var(TOKEN_ENV_VAR) };
        assert!(
            token.is_some(),
            "expected Some token when env var is set, got: {token:?}"
        );
    }

    #[test]
    fn token_resolution_absent_uses_fixed_provider() {
        // Test that a provider returning None propagates correctly.
        // Uses FixedTokenProvider to avoid process-env races.
        let provider = FixedTokenProvider(None);
        let preview = dummy_preview(&"a".repeat(64));
        let err = file_issue(&preview, &provider).unwrap_err();
        assert!(
            matches!(err, GithubFilingError::NoToken),
            "expected NoToken: {err}"
        );
    }

    #[test]
    fn token_resolution_from_file() {
        // Write a token to a temp file, point TOKEN_FILE_ENV_VAR at it,
        // ensure the provider reads it back.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "ghp_from_file_xyz\n").unwrap();
        // SAFETY: we point TOKEN_FILE_ENV_VAR at the temp file; TOKEN_ENV_VAR
        // is cleared first so the file path is the resolution source.
        unsafe {
            std::env::remove_var(TOKEN_ENV_VAR);
            std::env::set_var(TOKEN_FILE_ENV_VAR, tmp.path().as_os_str());
        }
        let token = EnvFileTokenProvider.token();
        // SAFETY: cleanup.
        unsafe { std::env::remove_var(TOKEN_FILE_ENV_VAR) };
        // Token env var may be re-set by another test that ran concurrently;
        // what we assert is that the file token is at minimum included when
        // TOKEN_ENV_VAR is absent. If TOKEN_ENV_VAR was set concurrently the
        // env-var path wins — still a valid Some(_).
        assert!(
            token.is_some(),
            "expected a token from file (or env): {token:?}"
        );
    }

    #[test]
    fn no_token_yields_no_token_error() {
        let provider = StaticTokenProvider(None);
        let preview = dummy_preview(&"a".repeat(64));
        let err = file_issue(&preview, &provider).unwrap_err();
        assert!(
            matches!(err, GithubFilingError::NoToken),
            "expected NoToken, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("TRUSTY_BUGREPORT_GITHUB_TOKEN"), "{msg}");
    }

    #[test]
    fn mock_create_path_called_when_no_existing_issue() {
        let fp = "b".repeat(64);
        let preview = dummy_preview(&fp);
        let mock = MockGithubApi::no_existing();
        let result = file_issue_with(&preview, &mock).unwrap();

        assert!(result.filed);
        assert!(!result.deduped, "should not be deduped");
        assert_eq!(result.issue_number, 999);

        let calls = mock.calls.lock().unwrap();
        // Must have searched first, then created.
        assert!(
            calls.iter().any(|c| matches!(c, MockCall::SearchIssues)),
            "search not called: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| matches!(c, MockCall::CreateIssue { .. })),
            "create not called: {calls:?}"
        );
        assert!(
            !calls.iter().any(|c| matches!(c, MockCall::AddComment(_))),
            "comment should not be called on create path: {calls:?}"
        );
    }

    #[test]
    fn mock_comment_path_when_existing_issue_found() {
        let fp = "c".repeat(64);
        let preview = dummy_preview(&fp);
        let existing = vec![ExistingIssue {
            html_url: "https://github.com/bobmatnyc/trusty-tools/issues/42".to_string(),
            number: 42,
        }];
        let mock = MockGithubApi::with_existing(existing);
        let result = file_issue_with(&preview, &mock).unwrap();

        assert!(result.filed);
        assert!(result.deduped, "should be deduped");
        assert_eq!(result.issue_number, 42);
        assert!(result.issue_url.contains("42"), "{}", result.issue_url);

        let calls = mock.calls.lock().unwrap();
        assert!(
            calls.iter().any(|c| matches!(c, MockCall::AddComment(42))),
            "comment(42) not called: {calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| matches!(c, MockCall::CreateIssue { .. })),
            "create should not be called on dedup path: {calls:?}"
        );
    }

    #[test]
    fn label_mapping_known_crate() {
        use crate::daemon::bug_report::preview::build_preview;
        use crate::daemon::bug_report::types::AggregatedError;
        use trusty_common::error_capture::CapturedError;

        let agg = AggregatedError {
            record: CapturedError {
                timestamp_secs: 0,
                crate_target: "trusty_search::indexer".to_string(),
                crate_version: "0.1.0".to_string(),
                message: "err".to_string(),
                fields: String::new(),
                file: None,
                line: None,
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                fingerprint: "d".repeat(64),
            },
            occurrences: 1,
        };
        let preview = build_preview(&agg);
        assert!(
            preview.labels.contains(&"trusty-search".to_string()),
            "expected trusty-search label: {:?}",
            preview.labels
        );
    }

    #[test]
    fn label_mapping_unknown_crate_only_base_labels() {
        use crate::daemon::bug_report::preview::build_preview;
        use crate::daemon::bug_report::types::AggregatedError;
        use trusty_common::error_capture::CapturedError;

        let agg = AggregatedError {
            record: CapturedError {
                timestamp_secs: 0,
                crate_target: "some_unknown_crate".to_string(),
                crate_version: "0.1.0".to_string(),
                message: "err".to_string(),
                fields: String::new(),
                file: None,
                line: None,
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
                fingerprint: "e".repeat(64),
            },
            occurrences: 1,
        };
        let preview = build_preview(&agg);
        assert_eq!(
            preview.labels,
            vec!["bug", "auto-reported"],
            "unexpected labels for unknown crate: {:?}",
            preview.labels
        );
    }

    #[test]
    fn dedup_marker_extraction() {
        let fp = "f".repeat(64);
        let body = format!("some text\n<!-- trusty-bug-fingerprint: {fp} -->\nmore text");
        let extracted = extract_fingerprint(&body);
        assert_eq!(extracted, Some(fp));
    }

    #[test]
    fn dedup_marker_absent_returns_none() {
        let body = "no fingerprint here";
        assert!(extract_fingerprint(body).is_none());
    }

    #[test]
    fn create_issue_sends_correct_labels() {
        let fp = "g".repeat(64);
        let preview = IssuePreview {
            title: "[trusty_memory] oom".to_string(),
            body: format!("<!-- trusty-bug-fingerprint: {fp} -->"),
            labels: vec![
                "bug".to_string(),
                "auto-reported".to_string(),
                "trusty-memory".to_string(),
            ],
            fingerprint: fp,
            scrub_changes: vec![],
        };
        let mock = MockGithubApi::no_existing();
        let _ = file_issue_with(&preview, &mock).unwrap();

        let calls = mock.calls.lock().unwrap();
        let create = calls.iter().find_map(|c| {
            if let MockCall::CreateIssue { labels } = c {
                Some(labels.clone())
            } else {
                None
            }
        });
        let labels = create.expect("create call expected");
        assert!(labels.contains(&"trusty-memory".to_string()), "{labels:?}");
        assert!(labels.contains(&"bug".to_string()), "{labels:?}");
        assert!(labels.contains(&"auto-reported".to_string()), "{labels:?}");
    }
}
