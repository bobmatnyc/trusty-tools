//! LLM provider error types.
//!
//! Why: the spec (REV-302 / REV-334) requires a clear distinction between
//! config/lifecycle errors (which MUST NOT be retried and MUST alarm) and
//! transient errors (which MAY be retried with backoff).  Having a typed enum
//! lets the pipeline implement that logic without inspecting error messages.
//!
//! What: `LlmError` variants map to the two classes.  `is_retryable` and
//! `is_alarm` helpers encode the retry / alarm policy so pipeline code never
//! needs to pattern-match on variants.
//!
//! Test: `lm_error_classification` verifies that every variant is classified
//! consistently with the spec.

use thiserror::Error;

/// Errors produced by `LlmProvider::complete`.
///
/// Why: typed variants allow the pipeline to apply the correct retry / alarm
/// policy per error class (spec REV-334, source-analysis §12.1).
/// What: config/lifecycle errors map to `is_alarm = true, is_retryable = false`;
/// transient errors map to `is_alarm = false, is_retryable = true`.
/// Test: `lm_error_classification` verifies all variants.
#[derive(Debug, Error)]
pub enum LlmError {
    // ── Config / lifecycle errors (ALARM, no retry) ───────────────────────
    /// The requested model id does not exist or is not available on the
    /// provider.  Maps to Bedrock `ResourceNotFoundException` or OpenRouter
    /// 404.  Deterministic — retry will not help.
    #[error("model not found: {0}")]
    ModelNotFound(String),

    /// The model exists but is not in the ACTIVE lifecycle state (e.g. Bedrock
    /// provisioned throughput is in CREATING or FAILED state).  Deterministic.
    #[error("model not ready: {0}")]
    ModelNotReady(String),

    /// Malformed request (e.g. missing required field, bad JSON, Bedrock
    /// ValidationException on missing `us.` prefix).  Deterministic.
    #[error("validation error: {0}")]
    Validation(String),

    /// Authentication or authorisation failure (invalid API key, missing IAM
    /// permissions).  Deterministic.
    #[error("access denied: {0}")]
    AccessDenied(String),

    // ── Transient errors (may retry with backoff) ─────────────────────────
    /// Network-level failure: DNS resolution, TCP connect, TLS handshake, or
    /// read timeout.  May resolve on retry.
    #[error("transport error: {0}")]
    Transport(String),

    /// Provider returned HTTP 429 (rate-limited / quota exceeded).  Retry
    /// after back-off.
    #[error("rate limited")]
    RateLimited,

    /// Provider returned an HTTP 5xx or an unexpected non-success status.
    ///
    /// `status` is the numeric HTTP status; `body` is the response body.
    #[error("upstream error (HTTP {status}): {body}")]
    Upstream {
        /// HTTP status code.
        status: u16,
        /// Response body text.
        body: String,
    },
}

impl LlmError {
    /// Returns `true` if this error should trigger an operational alarm.
    ///
    /// Why: config/lifecycle errors indicate a broken deployment (wrong model
    /// id, missing credentials) and must be surfaced loudly, not swallowed.
    /// What: `ModelNotFound`, `ModelNotReady`, `Validation`, `AccessDenied`
    /// return `true`; transient errors return `false`.
    /// Test: `lm_error_classification`.
    pub fn is_alarm(&self) -> bool {
        matches!(
            self,
            LlmError::ModelNotFound(_)
                | LlmError::ModelNotReady(_)
                | LlmError::Validation(_)
                | LlmError::AccessDenied(_)
        )
    }

    /// Returns `true` if this error is safe to retry with exponential backoff.
    ///
    /// Why: retrying config/lifecycle errors wastes time and hides root causes.
    /// What: only `Transport`, `RateLimited`, and `Upstream` return `true`.
    /// Test: `lm_error_classification`.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            LlmError::Transport(_) | LlmError::RateLimited | LlmError::Upstream { .. }
        )
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lm_error_classification() {
        let alarm_variants: Vec<LlmError> = vec![
            LlmError::ModelNotFound("no-such-model".into()),
            LlmError::ModelNotReady("in-creating-state".into()),
            LlmError::Validation("missing us. prefix".into()),
            LlmError::AccessDenied("invalid api key".into()),
        ];
        for err in &alarm_variants {
            assert!(err.is_alarm(), "{err:?} should be alarm");
            assert!(!err.is_retryable(), "{err:?} should not be retryable");
        }

        let retryable_variants: Vec<LlmError> = vec![
            LlmError::Transport("connection refused".into()),
            LlmError::RateLimited,
            LlmError::Upstream {
                status: 503,
                body: "service unavailable".into(),
            },
        ];
        for err in &retryable_variants {
            assert!(!err.is_alarm(), "{err:?} should not be alarm");
            assert!(err.is_retryable(), "{err:?} should be retryable");
        }
    }

    #[test]
    fn error_messages_are_informative() {
        assert_eq!(
            LlmError::ModelNotFound("openai/gpt-99".into()).to_string(),
            "model not found: openai/gpt-99"
        );
        assert_eq!(LlmError::RateLimited.to_string(), "rate limited");
        assert_eq!(
            LlmError::Upstream {
                status: 503,
                body: "overloaded".into()
            }
            .to_string(),
            "upstream error (HTTP 503): overloaded"
        );
    }
}
