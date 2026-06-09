//! GitHub integration: App auth, PR diff/metadata fetch, push firewall, webhook
//! HMAC verification.
//!
//! Why: all GitHub-facing code lives in this submodule so the error types,
//! the HTTP client, and the firewall constant share a single namespace.
//! (spec REV-400–REV-404, source-analysis §4)
//!
//! What: re-exports the public items from each submodule (`auth`, `pr`,
//! `firewall`, `webhook`) and defines the shared `GithubError` enum and
//! `GithubClient` wrapper used by all helpers.
//!
//! Test: each submodule carries its own tests; this module's
//! `github_error_display` test verifies error message formatting.

pub mod auth;
pub mod firewall;
pub mod posting;
pub mod pr;
pub mod webhook;

pub use auth::{AuthStrategy, RunMode, mint_app_jwt, resolve_token_for_mode};
pub use firewall::{GH_ALLOW_PUSH, assert_no_push_operation};
pub use posting::{PostedReview, post_pr_review};
pub use pr::{PrMetadata, PrRef, PrUser, fetch_pr_diff, fetch_pr_metadata};
pub use webhook::verify_webhook_signature;

// ─── Shared error type ────────────────────────────────────────────────────────

/// Errors produced by all GitHub integration helpers.
///
/// Why: a shared typed enum lets callers distinguish auth failures, transport
/// failures, and API errors without inspecting error message strings.
/// What: covers the four error classes that arise in practice — missing
/// credentials, auth failures (bad PEM / JWT signing), transport failures
/// (network), and GitHub API non-2xx responses.  `PushFirewall` is the sentinel
/// returned by `assert_no_push_operation` (spec REV-403).
/// Test: `github_error_display` verifies message formatting.
#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    /// No GitHub token or App credentials are configured.
    #[error("no GitHub token configured; set GITHUB_TOKEN or GitHub App credentials")]
    MissingToken,

    /// App authentication failure (bad PEM, JWT signing error).
    #[error("GitHub App auth error: {0}")]
    Auth(String),

    /// HTTP transport failure (DNS, connect, TLS, timeout).
    #[error("GitHub request failed: {0}")]
    Transport(String),

    /// GitHub returned a non-2xx status.
    #[error("GitHub API returned {status}: {body}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Response body text.
        body: String,
    },

    /// Push/write firewall — any attempt to perform a git-write operation was
    /// blocked.  (spec REV-403, non-configurable)
    #[error(
        "push operation blocked by firewall (GH_ALLOW_PUSH=false, spec REV-403). \
         Write operations are permanently disabled."
    )]
    PushFirewall,
}

// ─── Shared HTTP client wrapper ───────────────────────────────────────────────

/// Shared reqwest client for all GitHub API calls.
///
/// Why: a single client reuses the connection pool and carries the default
/// headers (User-Agent) used by all GitHub helpers.
/// What: wraps `reqwest::Client` with a `user_agent` string and a convenience
/// constructor.  The `http` field is public so helper functions in submodules
/// can add per-request headers as needed.
/// Test: used transitively by all submodule tests.
pub struct GithubClient {
    /// Underlying reqwest client.
    pub http: reqwest::Client,
    /// User-Agent header value.  GitHub rejects requests without a User-Agent.
    pub user_agent: String,
}

impl GithubClient {
    /// Create a `GithubClient` with default settings.
    ///
    /// Why: most callers do not need to customise timeout or TLS settings.
    /// What: builds a `reqwest::Client` with a 30-second request timeout.
    /// Returns `Err(GithubError::Transport)` if the TLS backend cannot be
    /// initialised — surfaces the failure to the caller rather than panicking
    /// at daemon startup (closes #953).
    /// Test: used by auth and pr module tests.
    pub fn new() -> Result<Self, GithubError> {
        Self::with_timeout(std::time::Duration::from_secs(30))
    }

    /// Create a `GithubClient` with a custom request timeout.
    ///
    /// Why: tests use short timeouts (e.g. 200ms) to verify transport-error
    /// handling quickly.
    /// What: builds a `reqwest::Client` with the specified timeout.  Returns
    /// `Err(GithubError::Transport)` if the TLS backend cannot be initialised
    /// — surfaces the failure instead of panicking at startup (closes #953).
    /// Test: used by pr module transport-error tests.
    pub fn with_timeout(timeout: std::time::Duration) -> Result<Self, GithubError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| GithubError::Transport(format!("failed to build HTTP client: {e}")))?;
        Ok(Self {
            http,
            user_agent: "trusty-review".to_string(),
        })
    }
}

impl Default for GithubClient {
    /// Construct with default settings; panics only on TLS-backend init failure.
    ///
    /// Why: `Default` cannot return `Result`; kept for test code and contexts
    /// where the caller already knows TLS is available.  Production paths should
    /// use `GithubClient::new()` and propagate the error.
    /// What: delegates to `Self::new().expect(…)`.
    /// Test: `github_client_default_constructs`.
    fn default() -> Self {
        Self::new().expect("reqwest::Client::build failed — TLS backend unavailable")
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_error_display_missing_token() {
        let err = GithubError::MissingToken;
        let s = err.to_string();
        assert!(
            s.contains("GITHUB_TOKEN"),
            "MissingToken message should mention GITHUB_TOKEN: {s}"
        );
    }

    #[test]
    fn github_error_display_auth() {
        let err = GithubError::Auth("bad PEM".to_string());
        assert!(err.to_string().contains("bad PEM"));
    }

    #[test]
    fn github_error_display_api() {
        let err = GithubError::Api {
            status: 404,
            body: "not found".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("404"));
        assert!(s.contains("not found"));
    }

    #[test]
    fn github_error_display_push_firewall() {
        let err = GithubError::PushFirewall;
        let s = err.to_string();
        assert!(
            s.contains("GH_ALLOW_PUSH=false"),
            "PushFirewall message must reference the constant: {s}"
        );
        assert!(
            s.contains("REV-403"),
            "PushFirewall message must reference spec REV-403: {s}"
        );
    }

    #[test]
    fn github_client_default_constructs() {
        let _client = GithubClient::default();
    }
}
