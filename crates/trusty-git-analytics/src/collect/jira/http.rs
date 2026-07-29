//! JSON request/response plumbing for the JIRA client (issue #3966,
//! PR #4067 review round 2).
//!
//! Extracted from `client.rs` so that file stays under the 500-SLOC
//! production cap, and so the throttling classification below has one
//! definition rather than one per verb.

use std::time::Duration;

use reqwest::{Client, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::collect::env_expand::expand_env_var;
use crate::collect::errors::{CollectError, Result};

/// `(username, token)` for HTTP Basic Auth.
pub type Credentials = (String, String);

/// Ceiling applied to a server-supplied `Retry-After`.
///
/// Why cap at all: Jira Cloud occasionally answers a 429 with tens of
/// seconds, and four attempts at that value would park a per-ticket request
/// for minutes. The run-level backoff budget in [`super::retry`] is the real
/// bound on sustained throttling; this simply keeps one request from eating
/// the whole budget on its own.
const MAX_HONOURED_RETRY_AFTER: Duration = Duration::from_secs(30);

/// `POST` a JSON body and decode the response.
///
/// Factored out so the retry wrapper can re-run the whole request/decode
/// round-trip — a `reqwest::RequestBuilder` is single-use, so the retry
/// closure must rebuild it on every attempt.
///
/// # Errors
///
/// [`CollectError::Throttled`] on 429/503 (carrying any `Retry-After`),
/// [`CollectError::Http`] on other non-success statuses or transport
/// failures, [`CollectError::Json`] on a decode failure.
pub async fn post_json<T: DeserializeOwned>(
    client: &Client,
    credentials: Option<&Credentials>,
    url: &str,
    body: &Value,
) -> Result<T> {
    let mut req = client.post(url).json(body);
    if let Some((user, token)) = credentials {
        req = req.basic_auth(user, Some(token));
    }
    decode(req.send().await?).await
}

/// `GET` a URL and decode the response. See [`post_json`].
///
/// # Errors
///
/// As [`post_json`].
pub async fn get_json<T: DeserializeOwned>(
    client: &Client,
    credentials: Option<&Credentials>,
    url: &str,
) -> Result<T> {
    let mut req = client.get(url);
    if let Some((user, token)) = credentials {
        req = req.basic_auth(user, Some(token));
    }
    decode(req.send().await?).await
}

/// Turn a response into either a decoded payload or a classified error,
/// reading `Retry-After` **before** `error_for_status()` discards it.
///
/// PR #4067 round 1 claimed the header was unreachable because
/// `error_for_status()` consumes the response. That was wrong: the response
/// is in hand here, and its headers are readable before any conversion.
async fn decode<T: DeserializeOwned>(resp: Response) -> Result<T> {
    let status = resp.status();
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
        return Err(CollectError::Throttled {
            status: status.as_u16(),
            retry_after: retry_after(&resp),
        });
    }
    let resp = resp.error_for_status()?;
    Ok(resp.json().await?)
}

/// Parse a `Retry-After` delay in delta-seconds form, clamped to
/// [`MAX_HONOURED_RETRY_AFTER`].
///
/// The HTTP-date form is not parsed: Jira Cloud sends delta-seconds, and a
/// missing value simply falls back to the exponential schedule, which is the
/// same behaviour as before this change.
fn retry_after(resp: &Response) -> Option<Duration> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs).min(MAX_HONOURED_RETRY_AFTER))
}

/// Expand a `${ENV_VAR}` credential reference, rejecting an expansion that
/// resolves to nothing.
///
/// Why: [`expand_env_var`] returns the empty string for an unset variable, so
/// `token: "${JIRA_TOKEN}"` with `JIRA_TOKEN` missing from a cron environment
/// produced an empty password and a bare HTTP 401 — a slow diagnosis in
/// exactly the unattended context this is built for. (A unified credential
/// resolver is tracked separately as issue #4037; this is the local guard,
/// not that.)
///
/// No secret can leak through the error message: it is only reachable when
/// the expansion is empty, which means `raw` is either an empty literal or an
/// unresolved `${PLACEHOLDER}`. A real credential expands to itself and never
/// reaches this branch.
///
/// # Errors
///
/// [`CollectError::Config`] naming the config field and the unresolved value.
///
/// Test: `unset_credential_env_var_is_a_config_error`,
/// `absent_credentials_are_not_an_error`.
pub fn expand_credential(field: &str, raw: &str) -> Result<String> {
    let expanded = expand_env_var(raw);
    if expanded.is_empty() {
        return Err(CollectError::Config(format!(
            "{field} is empty after expansion (config value: `{raw}`) — set the \
             referenced environment variable, or remove the field to run \
             unauthenticated"
        )));
    }
    Ok(expanded)
}
