//! BaseClient: authenticated HTTP wrapper over the Telegram Bot API.
//!
//! Why: Every Bot API method (`sendMessage`, `getMe`, `getChat`, …) needs the
//! same token resolution, URL assembly, JSON-envelope unwrapping, and
//! rate-limit handling. Centralising it here (mirroring
//! `slack::api::client::BaseClient`) means each future tool handler only encodes
//! its own request/response shape. Unlike Slack, the Bot API carries the token
//! in the URL path (`/bot<token>/<method>`), not a bearer header, so the token
//! is spliced into the request URL rather than an `Authorization` header.
//! What: Holds a `reqwest::Client`, a resolved bot token (or `None`), and the
//! API host base. `new()` resolves the token via the shared credential
//! resolver; [`BaseClient::call_method`] performs a hardened POST that surfaces
//! auth failures as a typed error (never a silent anonymous retry) and honours
//! `429` retry hints (`parameters.retry_after` in the body, or the `Retry-After`
//! header) with a bounded backoff.
//! Test: `new_succeeds_without_token` (constructor, CI-safe) here;
//! `retry_after_*` here; the full request path (200 / 401 / `ok:false` / 429) is
//! covered in `tests/telegram_client_http.rs`.

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::StatusCode;
use serde::Serialize;
use serde_json::Value;

use crate::telegram::api::constants::{
    DEFAULT_RETRY_AFTER, MAX_RATE_LIMIT_RETRIES, MAX_RETRY_AFTER, TELEGRAM_API_BASE,
    TELEGRAM_PROVIDER,
};
use crate::telegram::api::error::TelegramError;

/// Authenticated Telegram Bot API client.
///
/// Why: One handle per process; live request methods splice the bot token into
/// the URL on every call and unwrap Telegram's `{"ok": bool, ...}` envelope in
/// one place.
/// What: Wraps a `reqwest::Client`, the resolved token (`None` when
/// unconfigured — construction still succeeds so `tools/list` and the MCP
/// handshake work without secrets), and the API host base (overridable for
/// tests).
/// Test: `new_succeeds_without_token`; request behaviour in
/// `tests/telegram_client_http.rs`.
pub struct BaseClient {
    /// Shared HTTP client / connection pool.
    http: reqwest::Client,
    /// Resolved Telegram bot token, or `None` when unconfigured.
    token: Option<String>,
    /// API host root the `bot<token>/<method>` segment is appended to.
    /// Defaults to [`TELEGRAM_API_BASE`].
    base_url: String,
}

impl BaseClient {
    /// Construct with a shared HTTP client and a resolved Telegram token.
    ///
    /// Why: The binary builds exactly one client at startup. Keeping it
    /// infallible-on-missing-token (returns `Ok` with `token: None`) means the
    /// server boots for `tools/list` without credentials; the `MissingToken`
    /// error is deferred to the first call that actually needs auth.
    /// What: Builds the `reqwest::Client` and resolves the bot token through
    /// `trusty_common::inference::credentials::resolve_key(TELEGRAM_PROVIDER)` —
    /// process env (`TELEGRAM_BOT_TOKEN`) → `.env.local` → secure store, in that
    /// order. Returns `Err` only if the HTTP client fails to build.
    /// Test: `new_succeeds_without_token`; env-tier pickup in
    /// `tests/telegram_client_http.rs`.
    pub fn new() -> Result<Self> {
        let http = build_http_client()?;
        let token = trusty_common::inference::credentials::resolve_key(TELEGRAM_PROVIDER);
        Ok(Self {
            http,
            token,
            base_url: TELEGRAM_API_BASE.to_string(),
        })
    }

    /// Construct against an explicit base URL and token.
    ///
    /// Why: lets tests point the client at a local mock server without touching
    /// the network or a real bot token.
    /// What: same as [`BaseClient::new`] but takes the base URL and token
    /// directly instead of resolving them.
    /// Test: the constructor used by every case in
    /// `tests/telegram_client_http.rs`.
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
    /// Test: `tests/telegram_client_http.rs::base_client_new_resolves_env_token`.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// Call a Telegram Bot API `method` with a JSON `body`, returning the
    /// decoded response envelope on success.
    ///
    /// Why: the single hardened request primitive every future tool handler
    /// reuses — one place for token assembly, error classification, and
    /// rate-limit backoff, so no handler re-implements (or forgets) them.
    /// What: POSTs `{base_url}/bot{token}/{method}`. Maps HTTP 401 and body
    /// `error_code:401` to [`TelegramError::Auth`] (never retried as anonymous);
    /// honours `429` retry hints with a bounded backoff up to
    /// [`MAX_RATE_LIMIT_RETRIES`], then returns [`TelegramError::RateLimited`];
    /// returns [`TelegramError::Api`] for other `ok:false` bodies and the
    /// decoded `Value` when `ok:true`.
    /// Test: `tests/telegram_client_http.rs` (`send_ok`, `auth_401`,
    /// `auth_ok_false`, `rate_limit_retries_then_succeeds`,
    /// `rate_limit_exhausted`).
    pub async fn call_method<B>(&self, method: &str, body: &B) -> Result<Value, TelegramError>
    where
        B: Serialize + ?Sized,
    {
        let token = self.token.as_deref().ok_or(TelegramError::MissingToken)?;
        let url = format!(
            "{}/bot{}/{}",
            self.base_url.trim_end_matches('/'),
            token,
            method
        );

        let mut retries: u32 = 0;
        loop {
            let response = self.http.post(&url).json(body).send().await.map_err(|e| {
                TelegramError::Transport {
                    reason: classify_transport_error(&e),
                }
            })?;
            let status = response.status();

            if status == StatusCode::UNAUTHORIZED {
                return Err(TelegramError::Auth {
                    status: status.as_u16(),
                    reason: "HTTP 401 Unauthorized".to_string(),
                });
            }

            if status == StatusCode::TOO_MANY_REQUESTS {
                let headers = response.headers().clone();
                let body: Value = response.json().await.unwrap_or(Value::Null);
                let retry_after = retry_after_from_response(&headers, &body);
                if retries >= MAX_RATE_LIMIT_RETRIES {
                    return Err(TelegramError::RateLimited {
                        retries,
                        retry_after,
                    });
                }
                retries += 1;
                tokio::time::sleep(retry_after).await;
                continue;
            }

            if !status.is_success() {
                return Err(TelegramError::UnexpectedStatus(status.as_u16()));
            }

            let value: Value = response
                .json()
                .await
                .map_err(|e| TelegramError::Decode(e.to_string()))?;
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
        .user_agent(concat!(
            "trusty-channels-telegram/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .context("build reqwest client")
}

/// Classify a transport-level `reqwest::Error` into a short, URL-free reason.
///
/// Why: the Bot API token lives in the request URL path
/// (`/bot<token>/<method>`), and `reqwest::Error`'s `Display`/`Debug` both
/// render the attached request URL verbatim (`with_url`, reqwest 0.12). Never
/// call `.to_string()`, `{:?}`, or otherwise format the error itself — build
/// the reason purely from its boolean predicates and status so the token can
/// never reach a log line or a `?`-propagated message.
/// What: returns one of a small set of static/short reasons based on
/// `is_timeout()` / `is_connect()` / `is_body()` / `is_decode()` / `status()`,
/// falling back to a generic "request error" label.
/// Test: `transport_error_display_and_debug_never_contain_the_bot_token`.
fn classify_transport_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timeout".to_string()
    } else if e.is_connect() {
        "connect error".to_string()
    } else if e.is_body() {
        "body error".to_string()
    } else if e.is_decode() {
        "decode error".to_string()
    } else if let Some(status) = e.status() {
        format!("HTTP error (status {status})")
    } else {
        "request error".to_string()
    }
}

/// Interpret a decoded Telegram response envelope.
///
/// Why: the Bot API signals failures with `{"ok": false, "error_code": N,
/// "description": "..."}`, so success is not implied by the HTTP status alone.
/// What: returns `Ok(value)` when `ok` is `true`; maps `error_code:401` to
/// [`TelegramError::Auth`] and every other `ok:false` to [`TelegramError::Api`]
/// carrying the `description`.
/// Test: `tests/telegram_client_http.rs::auth_ok_false`, `::api_error_ok_false`.
fn interpret_envelope(status: u16, value: Value) -> Result<Value, TelegramError> {
    if value.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(value);
    }
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("unknown_error")
        .to_string();
    let error_code = value.get("error_code").and_then(Value::as_u64);
    if error_code == Some(401) {
        return Err(TelegramError::Auth {
            status,
            reason: description,
        });
    }
    Err(TelegramError::Api(description))
}

/// Resolve a `429` retry delay from the response, preferring the Bot API's
/// idiomatic body hint over the header.
///
/// Why: honour Telegram's advertised backoff, but never let a malformed or
/// hostile value force an unbounded (or zero-cost busy-loop) wait.
/// What: reads `parameters.retry_after` (integer seconds) from the JSON body
/// first — the canonical Bot API signal — then the `Retry-After` header, then
/// falls back to [`DEFAULT_RETRY_AFTER`]; the result is clamped to
/// `[0, MAX_RETRY_AFTER]`.
/// Test: `retry_after_from_body_hint`, `retry_after_from_header`,
/// `retry_after_defaults`.
fn retry_after_from_response(headers: &HeaderMap, body: &Value) -> Duration {
    if let Some(secs) = body
        .get("parameters")
        .and_then(|p| p.get("retry_after"))
        .and_then(Value::as_u64)
    {
        return Duration::from_secs(secs).min(MAX_RETRY_AFTER);
    }
    if let Some(secs) = headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
    {
        return Duration::from_secs(secs).min(MAX_RETRY_AFTER);
    }
    DEFAULT_RETRY_AFTER
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_succeeds_without_token() {
        // Constructing must not require Telegram credentials so the MCP
        // handshake and tools/list work in CI without secrets.
        let client = BaseClient::new().expect("construct base client");
        let _ = client.has_token();
    }

    #[test]
    fn retry_after_from_body_hint() {
        let body = json!({ "parameters": { "retry_after": 2 } });
        assert_eq!(
            retry_after_from_response(&HeaderMap::new(), &body),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn retry_after_from_header() {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, "3".parse().unwrap());
        assert_eq!(
            retry_after_from_response(&h, &Value::Null),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn retry_after_clamps() {
        let body = json!({ "parameters": { "retry_after": 99999 } });
        assert_eq!(
            retry_after_from_response(&HeaderMap::new(), &body),
            MAX_RETRY_AFTER
        );
    }

    #[test]
    fn retry_after_defaults() {
        // No body hint and no header → default.
        assert_eq!(
            retry_after_from_response(&HeaderMap::new(), &Value::Null),
            DEFAULT_RETRY_AFTER
        );
    }
}
