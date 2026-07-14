//! Slack Web API endpoint constants.
//!
//! Why: Centralise base URLs so future service/method modules don't drift on
//! path roots and we can swap to an enterprise/Gov Slack endpoint in one place
//! if needed.
//! What: `const &str` for the Slack Web API root and the env var used to
//! resolve a bot/user token. No string interpolation here — callers append
//! method paths (e.g. `chat.postMessage`).
//! Test: `base_url_parses` asserts the root parses as a valid `reqwest::Url`.

/// Slack Web API root. Methods are appended, e.g. `{SLACK_API_BASE}/chat.postMessage`.
pub const SLACK_API_BASE: &str = "https://slack.com/api";

/// Environment variable consulted for a Slack token when credential
/// resolution is wired up. The final auth source is deferred (see
/// `client::BaseClient`); this constant documents the intended env fallback.
pub const SLACK_TOKEN_ENV: &str = "SLACK_BOT_TOKEN";

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
