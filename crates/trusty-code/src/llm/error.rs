//! Error types for the LLM client.
//!
//! Why: Structured errors let callers pattern-match on failure modes (network
//! error vs. API-level error vs. deserialisation failure) without parsing
//! strings.
//! What: `LlmError` covers the four failure categories the client can produce;
//! it implements `std::error::Error` via `thiserror`.
//! Test: `error::tests::*` — error display strings, `From` conversions.

use thiserror::Error;

/// Error type returned by `LlmClient` operations.
///
/// Why: Library code must expose structured errors rather than `anyhow::Error`
/// so consumers can programmatically handle recoverable failures (e.g.
/// rate-limit → retry, invalid API key → fast-fail).
/// What: Covers network transport errors, non-200 HTTP responses with an
/// optional API-level error body, and response deserialisation failures.
/// Test: `llm_error_display`.
#[derive(Debug, Error)]
pub enum LlmError {
    /// The HTTP transport layer failed (connection refused, TLS error, timeout).
    ///
    /// Why: Distinct from API errors so callers can apply retry logic to
    /// transient network failures without retrying on e.g. invalid-key errors.
    #[error("HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// The API returned a non-2xx status code.
    ///
    /// Why: Callers need both the status code and the raw body to decide
    /// whether to retry (e.g. 429 Too Many Requests) or give up (e.g. 401
    /// Unauthorized).
    #[error("API error {status}: {body}")]
    ApiError {
        /// HTTP status code.
        status: u16,
        /// Raw response body (may be JSON or plain text).
        body: String,
    },

    /// The response body could not be deserialised into `ChatResponse`.
    ///
    /// Why: Separates schema-mismatch problems (usually provider bugs or
    /// version drift) from transport and API-level issues.
    #[error("response deserialisation failed: {source}\nbody: {body}")]
    Deserialise {
        /// The underlying serde error.
        source: serde_json::Error,
        /// The raw body that failed to parse (aids debugging).
        body: String,
    },

    /// A required configuration value is missing.
    ///
    /// Why: Failing at construction time with a clear message is better than
    /// receiving a cryptic 401 from the API.
    #[error("missing configuration: {0}")]
    MissingConfig(String),
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `LlmError::ApiError` formats the status code and body in the display string.
    ///
    /// Why: Consumers and logs rely on the `Display` impl; verify it contains
    /// the key fields.
    /// What: Construct an `ApiError`, call `to_string()`, assert substrings.
    /// Test: this test.
    #[test]
    fn api_error_display_includes_status_and_body() {
        let err = LlmError::ApiError {
            status: 429,
            body: "rate limited".into(),
        };
        let s = err.to_string();
        assert!(s.contains("429"), "status missing from: {s}");
        assert!(s.contains("rate limited"), "body missing from: {s}");
    }

    /// `LlmError::MissingConfig` carries the field name in its display.
    ///
    /// Why: Operator experience — the error should tell them exactly which
    /// configuration field to set.
    /// What: Construct a `MissingConfig`, assert the key name appears.
    /// Test: this test.
    #[test]
    fn missing_config_display_includes_field_name() {
        let err = LlmError::MissingConfig("OPENROUTER_API_KEY".into());
        let s = err.to_string();
        assert!(s.contains("OPENROUTER_API_KEY"), "field name missing: {s}");
    }

    /// `LlmError::Deserialise` includes both the error and the body.
    ///
    /// Why: Debugging a deserialisation failure requires seeing the raw JSON
    /// body; confirm it appears in the display.
    /// What: Construct a deserialise error from a real `serde_json` error.
    /// Test: this test.
    #[test]
    fn deserialise_error_includes_body() {
        let raw = "{invalid json}";
        let serde_err = serde_json::from_str::<serde_json::Value>(raw).unwrap_err();
        let err = LlmError::Deserialise {
            source: serde_err,
            body: raw.into(),
        };
        let s = err.to_string();
        assert!(s.contains(raw), "raw body missing: {s}");
    }
}
