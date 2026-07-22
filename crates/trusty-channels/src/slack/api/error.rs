//! Typed errors for live Slack Web API calls.
//!
//! Why: `BaseClient`'s request path has several distinct failure modes a caller
//! must be able to react to differently — an auth failure is unrecoverable and
//! must never be silently retried as anonymous, a rate-limit is transient, a
//! Slack `ok:false` envelope is an application error, and a transport error is
//! infrastructural. A single `anyhow` blob would collapse all of these into an
//! opaque string; a `thiserror` enum keeps them matchable (library convention).
//! What: [`SlackError`] enumerates every failure `BaseClient::call_method` can
//! surface. `Transport` carries the underlying `reqwest::Error`; the rest are
//! semantic classifications derived from the HTTP status or Slack envelope.
//! Test: exercised end-to-end against a mocked Slack server in
//! `tests/client_http.rs` (`auth_*`, `rate_limit_*`, `ok_false_*`).

use std::time::Duration;

/// A failure raised while calling a Slack Web API method.
///
/// Why: callers (future tool handlers in #2639/#2640) branch on the failure
/// class — surface `Auth` to the operator, back off on `RateLimited`, report
/// `Api`/`UnexpectedStatus` as a tool error. Keeping these variants distinct is
/// the whole point of not returning `anyhow::Error` from the request path.
/// What: a structured error enum; `Transport` is `#[from]` so `?` on a
/// `reqwest` call converts automatically.
/// Test: each variant is asserted in `tests/client_http.rs`.
#[derive(Debug, thiserror::Error)]
pub enum SlackError {
    /// No token was configured, so a live call cannot be attempted. Construction
    /// of `BaseClient` still succeeds without a token (so `tools/list` works);
    /// this is only raised when a method that *requires* auth is invoked.
    #[error(
        "no Slack token configured: set {} (process env or .env.local), \
         or store it via the credential resolver",
        crate::slack::api::constants::SLACK_TOKEN_ENV
    )]
    MissingToken,

    /// A user-scope-only method (`search.messages`) was invoked but no Slack
    /// *user* token is configured. Never falls back to the bot token — Slack
    /// rejects a bot token for `search.messages` with a confusing
    /// `not_allowed_token_type` error, so we fail fast with an actionable hint.
    #[error("Slack user token required for search; set SLACK_USER_TOKEN")]
    MissingUserToken,

    /// Slack rejected the credentials. Raised on an HTTP 401 **or** a `200` body
    /// carrying an auth-class `ok:false` error (`invalid_auth`, `not_authed`,
    /// …). Never retried — a bad token will not become good on retry, and
    /// silently retrying as anonymous would mask the misconfiguration.
    #[error("Slack authentication failed (status {status}): {reason}")]
    Auth {
        /// HTTP status observed (401, or 200 when the signal was `ok:false`).
        status: u16,
        /// Slack's error slug or a short description of the auth failure.
        reason: String,
    },

    /// Rate limiting persisted past the bounded retry budget. Carries the number
    /// of retries attempted and the last `Retry-After` the server advertised.
    #[error(
        "Slack rate limit not cleared after {retries} retries (last Retry-After: {retry_after:?})"
    )]
    RateLimited {
        /// How many retries were attempted before giving up.
        retries: u32,
        /// The `Retry-After` duration advertised on the final 429.
        retry_after: Duration,
    },

    /// Slack returned HTTP 200 with `ok:false` for a non-auth reason (e.g.
    /// `channel_not_found`, `not_in_channel`). Carries Slack's error slug.
    #[error("Slack API error: {0}")]
    Api(String),

    /// An HTTP status neither success, 401, nor 429 — e.g. a 5xx or an
    /// unexpected 4xx. Carries the numeric status for the caller to log.
    #[error("unexpected Slack HTTP status: {0}")]
    UnexpectedStatus(u16),

    /// The transport failed (DNS, TLS, connect, or a body-read error).
    #[error("Slack HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The response body was not the expected JSON envelope.
    #[error("could not decode Slack response body: {0}")]
    Decode(String),

    /// A private-file download (`url_private_download`, used by
    /// `slack_read_file` / `slack_read_canvas`) exceeded
    /// [`crate::slack::api::constants::MAX_DOWNLOAD_BYTES`]. Carries the
    /// enforced limit so the caller's error message is actionable.
    #[error("file exceeds the {limit}-byte download cap; use its permalink to fetch it directly")]
    DownloadTooLarge {
        /// The enforced byte cap ([`crate::slack::api::constants::MAX_DOWNLOAD_BYTES`]).
        limit: usize,
    },
}
