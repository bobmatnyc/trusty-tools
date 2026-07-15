//! Slack Web API endpoint constants and request-hardening tunables.
//!
//! Why: Centralise base URLs, the credential-provider identifier, and the
//! rate-limit budget so future service/method modules don't drift on path
//! roots or retry policy, and we can swap to an enterprise/Gov Slack endpoint
//! in one place if needed.
//! What: `const`s for the Slack Web API root, the credential-resolver provider
//! key, its canonical env var, and the bounded-backoff limits honoured by
//! `client::BaseClient`. No string interpolation here — callers append method
//! paths (e.g. `chat.postMessage`).
//! Test: `base_url_parses` asserts the root parses as a valid `reqwest::Url`;
//! the retry constants are exercised via `tests/client_http.rs`.

use std::time::Duration;

/// Slack Web API root. Methods are appended, e.g. `{SLACK_API_BASE}/chat.postMessage`.
pub const SLACK_API_BASE: &str = "https://slack.com/api";

/// Provider identifier passed to
/// `trusty_common::inference::credentials::resolve_key` to obtain the Slack bot
/// token. Slack is not an inference provider, but the resolver's provider→key
/// mapping (`env_var_for`) is the supported, non-parallel path for any token:
/// this key resolves the process-env → `.env.local` → secure-store precedence
/// against [`SLACK_TOKEN_ENV`]. Keep it in sync with the `"slack"` arm of
/// `credentials::resolver::env_var_for`.
pub const SLACK_PROVIDER: &str = "slack";

/// Canonical environment variable holding the Slack bot token. This is the same
/// name the credential resolver's `env_var_for("slack")` returns, so setting it
/// in the process env or `.env.local` is honoured by `resolve_key`.
pub const SLACK_TOKEN_ENV: &str = "SLACK_BOT_TOKEN";

/// Provider identifier for the Slack **user** token. Slack's `search.messages`
/// (and other user-scope-only methods) reject a bot token, so the client
/// resolves a second, independent token under this key. Kept separate from
/// [`SLACK_PROVIDER`] so a missing user token never disables bot-token tools.
/// Keep it in sync with the `"slack-user"` arm of
/// `credentials::resolver::env_var_for` (issue #2640).
pub const SLACK_USER_PROVIDER: &str = "slack-user";

/// Canonical environment variable holding the Slack user token. Same name the
/// credential resolver's `env_var_for("slack-user")` returns, so setting it in
/// the process env or `.env.local` is honoured by `resolve_key`.
pub const SLACK_USER_TOKEN_ENV: &str = "SLACK_USER_TOKEN";

/// Maximum number of automatic retries after a `429 Too Many Requests` before
/// giving up with [`crate::slack::api::error::SlackError::RateLimited`]. Bounds
/// the worst-case latency of a rate-limited call.
pub const MAX_RATE_LIMIT_RETRIES: u32 = 3;

/// Upper bound applied to any advertised `Retry-After`. A hostile or buggy
/// server cannot force an unbounded sleep; waits are clamped to this ceiling.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// Fallback wait used when a `429` omits a parseable `Retry-After` header.
pub const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_parses() {
        let url = reqwest::Url::parse(SLACK_API_BASE).expect("SLACK_API_BASE is a valid URL");
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("slack.com"));
    }
}
