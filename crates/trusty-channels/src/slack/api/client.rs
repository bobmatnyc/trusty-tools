//! BaseClient: authenticated HTTP wrapper over the Slack Web API.
//!
//! Why: Every Slack method (`chat.postMessage`, `conversations.history`, …)
//! needs the same token resolution, bearer-auth, JSON-envelope unwrapping, and
//! rate-limit handling. Centralising it here (mirroring
//! `trusty-gworkspace::api::client::BaseClient`) means each future tool handler
//! only encodes its own request/response shape.
//! What: Holds a `reqwest::Client`, a resolved bearer token (or `None`), and
//! the API base URL. `new()` resolves the token via the shared credential
//! resolver; [`BaseClient::call_method`] performs a hardened POST that surfaces
//! auth failures as a typed error (never a silent anonymous retry) and honours
//! `429 Retry-After` with a bounded backoff.
//! Test: `new_succeeds_without_token` (constructor, CI-safe) here;
//! `retry_after_from_headers_*` here; the full request path (200 / 401 /
//! `ok:false` / 429) is covered in `tests/client_http.rs`.

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::StatusCode;
use serde::Serialize;
use serde_json::Value;

use crate::slack::api::constants::{
    DEFAULT_RETRY_AFTER, MAX_RATE_LIMIT_RETRIES, MAX_RETRY_AFTER, SLACK_API_BASE, SLACK_PROVIDER,
};
use crate::slack::api::error::SlackError;

/// Slack `ok:false` error slugs that indicate an authentication failure. These
/// arrive as HTTP 200 bodies (Slack's usual auth-error channel) and must be
/// treated exactly like an HTTP 401 — a clear, non-retryable auth error.
const AUTH_ERROR_SLUGS: &[&str] = &[
    "invalid_auth",
    "not_authed",
    "account_inactive",
    "token_revoked",
    "token_expired",
    "no_permission",
];

/// Authenticated Slack Web API client.
///
/// Why: One handle per process; live request methods send the bearer token on
/// every call and unwrap Slack's `{"ok": bool, ...}` envelope in one place.
/// What: Wraps a `reqwest::Client`, the resolved token (`None` when
/// unconfigured — construction still succeeds so `tools/list` and the MCP
/// handshake work without secrets), and the API base URL (overridable for
/// tests / enterprise endpoints).
/// Test: `new_succeeds_without_token`; request behaviour in `tests/client_http.rs`.
pub struct BaseClient {
    /// Shared HTTP client / connection pool.
    http: reqwest::Client,
    /// Resolved Slack bearer token, or `None` when unconfigured.
    token: Option<String>,
    /// API root every method path is appended to. Defaults to [`SLACK_API_BASE`].
    base_url: String,
}

impl BaseClient {
    /// Construct with a shared HTTP client and a resolved Slack token.
    ///
    /// Why: The binary builds exactly one client at startup. Keeping it
    /// infallible-on-missing-token (returns `Ok` with `token: None`) means the
    /// server boots for `tools/list` without credentials; the `MissingToken`
    /// error is deferred to the first call that actually needs auth.
    /// What: Builds the `reqwest::Client` and resolves the bot token through
    /// `trusty_common::inference::credentials::resolve_key(SLACK_PROVIDER)` —
    /// process env (`SLACK_BOT_TOKEN`) → `.env.local` → secure store, in that
    /// order. Returns `Err` only if the HTTP client fails to build.
    /// Test: `new_succeeds_without_token`; env-tier pickup in `tests/client_http.rs`.
    pub fn new() -> Result<Self> {
        let http = build_http_client()?;
        let token = trusty_common::inference::credentials::resolve_key(SLACK_PROVIDER);
        Ok(Self {
            http,
            token,
            base_url: SLACK_API_BASE.to_string(),
        })
    }

    /// Construct against an explicit base URL and token.
    ///
    /// Why: lets tests point the client at a local mock server, and leaves room
    /// to target an enterprise/Gov Slack endpoint without a code change.
    /// What: same as [`BaseClient::new`] but takes the base URL and token
    /// directly instead of resolving them.
    /// Test: the constructor used by every case in `tests/client_http.rs`.
    pub fn with_endpoint(base_url: impl Into<String>, token: Option<String>) -> Result<Self> {
        Ok(Self {
            http: build_http_client()?,
            token,
            base_url: base_url.into(),
        })
    }

    /// Whether a token was resolved (a live call can be attempted).
    ///
    /// Why: callers and tests need to know if auth is configured without
    /// exposing the secret value itself.
    /// What: `true` iff a non-`None` token is held.
    /// Test: `tests/client_http.rs::base_client_new_resolves_env_token`.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// Call a Slack Web API `method` with a JSON `body`, returning the decoded
    /// response envelope on success.
    ///
    /// Why: the single hardened request primitive every future tool handler
    /// reuses — one place for auth, error classification, and rate-limit
    /// backoff, so no handler re-implements (or forgets) them.
    /// What: POSTs `{base_url}/{method}` with a bearer token. Maps HTTP 401 and
    /// auth-class `ok:false` bodies to [`SlackError::Auth`] (never retried as
    /// anonymous); honours `429 Retry-After` with a bounded backoff up to
    /// [`MAX_RATE_LIMIT_RETRIES`], then returns [`SlackError::RateLimited`];
    /// returns [`SlackError::Api`] for other `ok:false` bodies and the decoded
    /// `Value` when `ok:true`.
    /// Test: `tests/client_http.rs` (`send_ok`, `auth_401`, `auth_ok_false`,
    /// `rate_limit_retries_then_succeeds`, `rate_limit_exhausted`).
    pub async fn call_method<B>(&self, method: &str, body: &B) -> Result<Value, SlackError>
    where
        B: Serialize + ?Sized,
    {
        let token = self.token.as_deref().ok_or(SlackError::MissingToken)?;
        let url = format!("{}/{}", self.base_url.trim_end_matches('/'), method);

        let mut retries: u32 = 0;
        loop {
            let response = self
                .http
                .post(&url)
                .bearer_auth(token)
                .json(body)
                .send()
                .await?;
            let status = response.status();

            if status == StatusCode::UNAUTHORIZED {
                return Err(SlackError::Auth {
                    status: status.as_u16(),
                    reason: "HTTP 401 Unauthorized".to_string(),
                });
            }

            if status == StatusCode::TOO_MANY_REQUESTS {
                let retry_after = retry_after_from_headers(response.headers());
                if retries >= MAX_RATE_LIMIT_RETRIES {
                    return Err(SlackError::RateLimited {
                        retries,
                        retry_after,
                    });
                }
                retries += 1;
                tokio::time::sleep(retry_after).await;
                continue;
            }

            if !status.is_success() {
                return Err(SlackError::UnexpectedStatus(status.as_u16()));
            }

            let value: Value = response
                .json()
                .await
                .map_err(|e| SlackError::Decode(e.to_string()))?;
            return interpret_envelope(status.as_u16(), value);
        }
    }
}

/// Build the shared `reqwest::Client` used by every `BaseClient`.
///
/// Why: one place for the user-agent and (future) timeout/pool tuning.
/// What: sets a versioned user-agent; returns the transport build error as
/// `anyhow` context.
/// Test: covered implicitly by `new_succeeds_without_token`.
fn build_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("trusty-channels-slack/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build reqwest client")
}

/// Interpret a decoded Slack response envelope.
///
/// Why: Slack signals most failures with HTTP 200 + `{"ok": false, "error":
/// "..."}`, so success is not implied by the status code alone.
/// What: returns `Ok(value)` when `ok` is `true`; maps auth-class error slugs
/// to [`SlackError::Auth`] and every other `ok:false` to [`SlackError::Api`].
/// Test: `auth_ok_false_maps_to_auth_error`, `api_ok_false_maps_to_api_error`.
fn interpret_envelope(status: u16, value: Value) -> Result<Value, SlackError> {
    if value.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(value);
    }
    let slug = value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unknown_error")
        .to_string();
    if AUTH_ERROR_SLUGS.contains(&slug.as_str()) {
        return Err(SlackError::Auth {
            status,
            reason: slug,
        });
    }
    Err(SlackError::Api(slug))
}

/// Parse a `Retry-After` header into a bounded delay.
///
/// Why: honour Slack's advertised backoff, but never let a malformed or hostile
/// value force an unbounded (or zero-cost busy-loop) wait.
/// What: reads `Retry-After` as integer seconds, clamps to
/// `[0, MAX_RETRY_AFTER]`, and falls back to [`DEFAULT_RETRY_AFTER`] when the
/// header is missing or unparseable.
/// Test: `retry_after_from_headers_reads_seconds`,
/// `retry_after_from_headers_clamps`, `retry_after_from_headers_defaults`.
fn retry_after_from_headers(headers: &HeaderMap) -> Duration {
    let Some(secs) = headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
    else {
        return DEFAULT_RETRY_AFTER;
    };
    Duration::from_secs(secs).min(MAX_RETRY_AFTER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_succeeds_without_token() {
        // Constructing must not require Slack credentials so the MCP handshake
        // and tools/list work in CI without secrets.
        let client = BaseClient::new().expect("construct base client");
        let _ = client.has_token();
    }

    #[test]
    fn retry_after_from_headers_reads_seconds() {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(retry_after_from_headers(&h), Duration::from_secs(2));
    }

    #[test]
    fn retry_after_from_headers_clamps() {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, "99999".parse().unwrap());
        assert_eq!(retry_after_from_headers(&h), MAX_RETRY_AFTER);
    }

    #[test]
    fn retry_after_from_headers_defaults() {
        // Missing header → default; unparseable ("date form") → default.
        assert_eq!(
            retry_after_from_headers(&HeaderMap::new()),
            DEFAULT_RETRY_AFTER
        );
        let mut h = HeaderMap::new();
        h.insert(
            RETRY_AFTER,
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after_from_headers(&h), DEFAULT_RETRY_AFTER);
    }
}
