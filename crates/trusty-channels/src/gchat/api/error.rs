//! Typed errors for the Google Chat API layer, plus the redaction helpers
//! every error message passes through.
//!
//! Why: callers branch on the failure class — a bad key file is an operator
//! fix, a token-endpoint refusal is a credential problem, a Chat or Pub/Sub
//! non-2xx carries a status to act on. No variant may carry key material or an
//! access token, so messages built from response bodies are redacted first.
//! What: [`GchatError`] for request-level failures, [`EventParseError`] for a
//! single Pub/Sub message whose payload is not a Chat event, and crate-private
//! helpers that classify transport errors and extract a redacted message.
//! Test: `create_message_non_2xx_is_typed_and_never_contains_the_token`,
//! `key_file_mode_0644_is_refused_before_any_request`,
//! `error_message_redacts_before_truncating`.

use std::path::PathBuf;

use serde_json::Value;

use crate::gchat::api::constants::ERROR_MESSAGE_MAX_CHARS;

/// A failure raised while loading credentials or calling a Google API.
///
/// Why: keeps the failure classes matchable instead of one opaque string.
/// What: no variant holds a raw `reqwest::Error`, a key, a JWT assertion or an
/// access token; `message` fields are redacted and truncated.
/// Test: each variant is asserted in `tests/gchat_http.rs`.
#[derive(Debug, thiserror::Error)]
pub enum GchatError {
    /// The key file's permission bits are not exactly `0600`. Raised before
    /// the file is read and before any network call.
    #[error("service-account key file {path} has mode {mode:o}; it must be 600")]
    KeyFilePermissions {
        /// The key-file path the caller supplied.
        path: PathBuf,
        /// The observed permission bits (`mode & 0o777`).
        mode: u32,
    },

    /// The key file could not be opened or read.
    #[error("cannot read service-account key file {path}: {reason}")]
    KeyFileRead {
        /// The key-file path the caller supplied.
        path: PathBuf,
        /// The I/O error kind; never file content.
        reason: String,
    },

    /// The key file is not a usable service-account key. The reason is a
    /// fixed string, because a JSON parse error can echo file content.
    #[error("invalid service-account key file {path}: {reason}")]
    KeyFileInvalid {
        /// The key-file path the caller supplied.
        path: PathBuf,
        /// A fixed description of what is wrong.
        reason: &'static str,
    },

    /// Signing the JWT assertion failed.
    #[error("cannot sign the service-account assertion: {0}")]
    Signing(String),

    /// The OAuth token endpoint refused the grant.
    #[error("token endpoint refused the grant (HTTP {status}): {message}")]
    TokenEndpoint {
        /// HTTP status returned by the token endpoint.
        status: u16,
        /// `error: error_description`, redacted and truncated.
        message: String,
    },

    /// A Chat or Pub/Sub call returned a non-2xx status.
    #[error("{api} API returned HTTP {status}: {message}")]
    Http {
        /// Which API answered: `chat` or `pubsub`.
        api: &'static str,
        /// HTTP status code.
        status: u16,
        /// Google's `error.message`, redacted and truncated.
        message: String,
    },

    /// The request never produced a response (DNS, TLS, connect, timeout).
    #[error("{api} transport error: {reason}")]
    Transport {
        /// Which endpoint was called: `oauth2`, `chat` or `pubsub`.
        api: &'static str,
        /// A short classification of the failure.
        reason: String,
    },

    /// A 2xx response body was not the expected JSON.
    #[error("cannot decode {api} response: {reason}")]
    Decode {
        /// Which endpoint answered.
        api: &'static str,
        /// What failed to decode.
        reason: String,
    },

    /// A space or subscription name has the wrong shape. Checked before any
    /// network call so a caller cannot splice extra path segments into a URL.
    #[error("invalid resource name {name:?}: expected {expected}")]
    InvalidName {
        /// The rejected name.
        name: String,
        /// The expected format.
        expected: &'static str,
    },
}

/// Why one Pub/Sub message did not yield a Chat event.
///
/// Why: a malformed message must fail alone, not the whole pulled batch.
/// What: carried per message in `PulledMessage::event`.
/// Test: `pull_decodes_events_and_isolates_malformed_messages`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EventParseError {
    /// The Pub/Sub `data` field is not valid standard base64.
    #[error("message data is not valid base64")]
    Base64,
    /// The decoded bytes are not a JSON Chat event.
    #[error("message data is not a JSON Chat event: {0}")]
    Json(String),
    /// The event lacks a field this layer needs.
    #[error("Chat event is missing required field `{0}`")]
    MissingField(&'static str),
}

/// Classify a `reqwest::Error` into a short reason without its URL or source.
pub(crate) fn transport_error(api: &'static str, e: &reqwest::Error) -> GchatError {
    let reason = if e.is_timeout() {
        "timeout"
    } else if e.is_connect() {
        "connect error"
    } else if e.is_body() || e.is_decode() {
        "response body error"
    } else {
        "request error"
    };
    GchatError::Transport {
        api,
        reason: reason.to_string(),
    }
}

/// Extract Google's error text from a response body, redact every secret in
/// `secrets`, then truncate.
///
/// Why: a body may echo request content; redacting after truncation could
/// leave a token prefix behind, so redaction always runs first.
/// What: reads `error.message` (Google API errors) or `error` +
/// `error_description` (OAuth errors), else the raw body.
/// Test: `error_message_redacts_before_truncating`.
pub(crate) fn error_message(body: &str, secrets: &[&str]) -> String {
    let extracted = match serde_json::from_str::<Value>(body) {
        Ok(v) => match v.get("error") {
            Some(Value::Object(obj)) => obj
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string(),
            Some(Value::String(code)) => match v.get("error_description").and_then(Value::as_str) {
                Some(desc) => format!("{code}: {desc}"),
                None => code.clone(),
            },
            _ => body.to_string(),
        },
        Err(_) => body.to_string(),
    };
    let mut redacted = extracted;
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        redacted = redacted.replace(secret, "[REDACTED]");
    }
    redacted.chars().take(ERROR_MESSAGE_MAX_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_redacts_before_truncating() {
        // The secret straddles the truncation point: truncating first would
        // keep a prefix of it that the replace could no longer match.
        let secret = "ya29.secret-token-value"; // pragma: allowlist secret
        let padding = "x".repeat(ERROR_MESSAGE_MAX_CHARS - 5);
        let body = format!("{{\"error\":{{\"message\":\"{padding}{secret}\"}}}}");
        let msg = error_message(&body, &[secret]);
        assert!(!msg.contains("ya29"), "token prefix leaked: {msg}");

        let oauth = r#"{"error":"invalid_grant","error_description":"bad jwt"}"#;
        assert_eq!(error_message(oauth, &[]), "invalid_grant: bad jwt");
    }
}
