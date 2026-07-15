//! Telegram Bot API endpoint constants and request-hardening tunables.
//!
//! Why: Centralise the API root, the credential-provider identifier, and the
//! rate-limit budget so future method modules don't drift on path roots or
//! retry policy. The Telegram Bot API embeds the token in the URL path
//! (`/bot<token>/<method>`) rather than an auth header, so the root here is the
//! host prefix only — the `bot<token>` segment is assembled by `client`.
//! What: `const`s for the Bot API host root, the credential-resolver provider
//! key, its canonical env var, and the bounded-backoff limits honoured by
//! `client::BaseClient`. No string interpolation here — `client` appends the
//! `bot<token>/<method>` segments.
//! Test: `base_url_parses` asserts the root parses as a valid `reqwest::Url`;
//! the retry constants are exercised via `tests/telegram_client_http.rs`.

use std::time::Duration;

/// Telegram Bot API host root. The client appends `/bot<token>/<method>`, e.g.
/// `{TELEGRAM_API_BASE}/bot123:ABC/sendMessage`.
pub const TELEGRAM_API_BASE: &str = "https://api.telegram.org";

/// Provider identifier passed to
/// `trusty_common::inference::credentials::resolve_key` to obtain the Telegram
/// bot token. Telegram is not an inference provider, but the resolver's
/// provider→key mapping (`env_var_for`) is the supported, non-parallel path for
/// any token: this key resolves the process-env → `.env.local` → secure-store
/// precedence against [`TELEGRAM_TOKEN_ENV`]. Keep it in sync with the
/// `"telegram"` arm of `credentials::resolver::env_var_for`.
pub const TELEGRAM_PROVIDER: &str = "telegram";

/// Canonical environment variable holding the Telegram bot token. This is the
/// same name the credential resolver's `env_var_for("telegram")` returns, so
/// setting it in the process env or `.env.local` is honoured by `resolve_key`.
pub const TELEGRAM_TOKEN_ENV: &str = "TELEGRAM_BOT_TOKEN";

/// Maximum number of automatic retries after a `429 Too Many Requests` before
/// giving up with [`crate::telegram::api::error::TelegramError::RateLimited`].
/// Bounds the worst-case latency of a rate-limited call.
pub const MAX_RATE_LIMIT_RETRIES: u32 = 3;

/// Upper bound applied to any advertised retry delay. A hostile or buggy server
/// cannot force an unbounded sleep; waits are clamped to this ceiling.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Fallback wait used when a `429` omits a parseable `retry_after` (neither in
/// the JSON `parameters` object nor the `Retry-After` header).
pub const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_parses() {
        let url = reqwest::Url::parse(TELEGRAM_API_BASE).expect("TELEGRAM_API_BASE is a valid URL");
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("api.telegram.org"));
    }
}
