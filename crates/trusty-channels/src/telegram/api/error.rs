//! Typed errors for live Telegram Bot API calls.
//!
//! Why: `BaseClient`'s request path has several distinct failure modes a caller
//! must react to differently — an auth failure is unrecoverable and must never
//! be silently retried anonymously, a rate-limit is transient, a Telegram
//! `ok:false` envelope is an application error, and a transport error is
//! infrastructural. A single `anyhow` blob would collapse all of these into an
//! opaque string; a `thiserror` enum keeps them matchable (library convention),
//! mirroring `slack::api::error::SlackError`.
//! What: [`TelegramError`] enumerates every failure `BaseClient::call_method`
//! can surface. `Transport` carries only a URL-free classification string — see
//! its own doc for why the raw `reqwest::Error` must never be held here; the
//! rest are semantic classifications derived from the HTTP status or Bot API
//! `{"ok": false, "error_code": N, "description": "..."}` envelope.
//! Test: exercised end-to-end against a mocked Bot API server in
//! `tests/telegram_client_http.rs` (`auth_*`, `rate_limit_*`, `api_*`,
//! `transport_error_display_and_debug_never_contain_the_bot_token`).

use std::time::Duration;

/// A failure raised while calling a Telegram Bot API method.
///
/// Why: callers (the tool handlers, once live) branch on the failure class —
/// surface `Auth` to the operator, back off on `RateLimited`, report
/// `Api`/`UnexpectedStatus` as a tool error. Keeping these variants distinct is
/// the whole point of not returning `anyhow::Error` from the request path.
/// What: a structured error enum. `Transport` deliberately does NOT hold (or
/// derive `#[from]`) a raw `reqwest::Error` — see its own doc.
/// Test: each variant is asserted in `tests/telegram_client_http.rs`.
#[derive(Debug, thiserror::Error)]
pub enum TelegramError {
    /// No token was configured, so a live call cannot be attempted.
    /// Construction of `BaseClient` still succeeds without a token (so
    /// `tools/list` works); this is only raised when a method that *requires*
    /// auth is invoked.
    #[error(
        "no Telegram token configured: set {} (process env or .env.local), \
         or store it via the credential resolver",
        crate::telegram::api::constants::TELEGRAM_TOKEN_ENV
    )]
    MissingToken,

    /// Telegram rejected the credentials. Raised on an HTTP 401 **or** a body
    /// carrying `error_code: 401` (`Unauthorized`). Never retried — a bad token
    /// will not become good on retry, and silently retrying anonymously would
    /// mask the misconfiguration.
    #[error("Telegram authentication failed (status {status}): {reason}")]
    Auth {
        /// HTTP status observed (401, or the body's `error_code` when signalled
        /// there).
        status: u16,
        /// Telegram's `description` or a short description of the auth failure.
        reason: String,
    },

    /// Rate limiting persisted past the bounded retry budget. Carries the number
    /// of retries attempted and the last `retry_after` the server advertised.
    #[error(
        "Telegram rate limit not cleared after {retries} retries (last retry_after: {retry_after:?})"
    )]
    RateLimited {
        /// How many retries were attempted before giving up.
        retries: u32,
        /// The `retry_after` duration advertised on the final 429.
        retry_after: Duration,
    },

    /// Telegram returned `ok:false` for a non-auth reason (e.g.
    /// `chat not found`, `Bad Request: ...`). Carries the `description` text.
    #[error("Telegram API error: {0}")]
    Api(String),

    /// An HTTP status neither success, 401, nor 429 — e.g. a 5xx or an
    /// unexpected 4xx. Carries the numeric status for the caller to log.
    #[error("unexpected Telegram HTTP status: {0}")]
    UnexpectedStatus(u16),

    /// The transport failed (DNS, TLS, connect, or a body-read error).
    ///
    /// Why: the Telegram Bot API embeds the bot token IN THE URL PATH
    /// (`/bot<token>/<method>`), unlike Slack's bearer-header auth. `reqwest`
    /// 0.12 attaches the request URL to send/timeout/connect errors via
    /// `with_url`, and `reqwest::Error`'s `Display` **and** its derived `Debug`
    /// both render that URL verbatim — so holding (or `#[from]`-converting) the
    /// raw `reqwest::Error` here would leak the live bot token into any log or
    /// `?`-propagated error message. This variant instead carries only a short,
    /// URL-free classification derived from the error's `is_timeout()` /
    /// `is_connect()` / `is_body()` / `is_decode()` predicates and status (see
    /// `client::classify_transport_error`) — never the error itself, never its
    /// `Display`/`Debug`/`to_string()`.
    /// What: a plain `String` reason with no request context attached.
    /// Test: `transport_error_display_and_debug_never_contain_the_bot_token`.
    #[error("Telegram HTTP transport error: {reason}")]
    Transport {
        /// URL-free classification (e.g. "timeout", "connect error").
        reason: String,
    },

    /// The response body was not the expected JSON envelope.
    #[error("could not decode Telegram response body: {0}")]
    Decode(String),
}
