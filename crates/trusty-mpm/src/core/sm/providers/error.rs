//! SM LLM provider error type (DOC-14 spec §5.3).
//!
//! Why: the SM must distinguish *config/lifecycle* errors (wrong model id,
//! missing credentials — deterministic, must NOT be retried, must surface
//! loudly) from *transient* errors (network blips, 429, 5xx — may be retried
//! with backoff and may trigger the optional provider fallback chain, §5.3).
//! A typed enum lets the resolver and the fallback loop apply the correct
//! policy without inspecting human-readable error strings.
//! What: [`SmLlmError`] mirrors trusty-review's `LlmError` classification
//! (this is the VENDORED copy per the SM-2 PM decision — see the module-level
//! TODO in `mod.rs`). `is_retryable` / `is_alarm` encode the policy so callers
//! never pattern-match on variants.
//! Test: `sm_llm_error_classification`, `sm_llm_error_messages` below.

use thiserror::Error;

/// Errors produced by [`super::LlmProvider::complete`] and by provider
/// construction / resolution.
///
/// Why: typed variants let the SM fallback loop (§5.3) retry only transient
/// failures and alarm on deterministic config errors, matching trusty-review's
/// retry/alarm policy (`llm/error.rs`).
/// What: config/lifecycle variants classify as `is_alarm = true`,
/// `is_retryable = false`; transient variants as `is_alarm = false`,
/// `is_retryable = true`. The extra [`SmLlmError::Degraded`] variant (no
/// provider credentials available, §5.3 / G6) is neither retryable nor an
/// alarm — it is a graceful, reportable "no inference configured" state.
/// Test: `sm_llm_error_classification`.
#[derive(Debug, Error)]
pub enum SmLlmError {
    // ── Config / lifecycle errors (ALARM, no retry) ───────────────────────
    /// The requested model id does not exist or is not available on the
    /// provider (Bedrock `ResourceNotFoundException` / OpenRouter 404 /
    /// Anthropic 404). Deterministic — retry will not help.
    #[error("model not found: {0}")]
    ModelNotFound(String),

    /// The model exists but is not in an ACTIVE lifecycle state (e.g. Bedrock
    /// provisioned throughput in CREATING/FAILED). Deterministic.
    #[error("model not ready: {0}")]
    ModelNotReady(String),

    /// Malformed request (missing field, bad JSON, Bedrock ValidationException
    /// on a missing `us.` prefix, or an unknown `provider` config string).
    /// Deterministic.
    #[error("validation error: {0}")]
    Validation(String),

    /// Authentication / authorisation failure (invalid API key, missing IAM
    /// permissions). Deterministic.
    #[error("access denied: {0}")]
    AccessDenied(String),

    // ── Transient errors (may retry with backoff / trigger fallback) ──────
    /// Network-level failure: DNS, TCP connect, TLS handshake, or read
    /// timeout. May resolve on retry.
    #[error("transport error: {0}")]
    Transport(String),

    /// Provider returned HTTP 429 (rate-limited / quota exceeded). Retry after
    /// back-off, or fall back to the next provider.
    #[error("rate limited")]
    RateLimited,

    /// Provider returned an HTTP 5xx or an unexpected non-success status.
    #[error("upstream error (HTTP {status}): {body}")]
    Upstream {
        /// HTTP status code.
        status: u16,
        /// Response body text.
        body: String,
    },

    // ── Degraded mode (no provider credentials — graceful, §5.3 / G6) ─────
    /// No provider has resolvable credentials, so free-text reasoning is
    /// unavailable. NOT an alarm and NOT retryable — the SM serves its
    /// deterministic surface (routed commands, session/goal management) and
    /// reports this as a graceful notice rather than a hard failure.
    #[error("degraded: no inference provider configured ({0})")]
    Degraded(String),
}

impl SmLlmError {
    /// Returns `true` if this error should trigger an operational alarm.
    ///
    /// Why: config/lifecycle errors indicate a broken deployment (wrong model
    /// id, missing credentials) and must be surfaced loudly, not swallowed.
    /// What: `ModelNotFound`, `ModelNotReady`, `Validation`, `AccessDenied`
    /// return `true`; transient errors and `Degraded` return `false`.
    /// Test: `sm_llm_error_classification`.
    pub fn is_alarm(&self) -> bool {
        matches!(
            self,
            SmLlmError::ModelNotFound(_)
                | SmLlmError::ModelNotReady(_)
                | SmLlmError::Validation(_)
                | SmLlmError::AccessDenied(_)
        )
    }

    /// Returns `true` if this error is safe to retry / fall back on.
    ///
    /// Why: retrying config/lifecycle errors wastes time and hides root
    /// causes; the SM fallback chain (§5.3) advances only on retryable errors.
    /// What: only `Transport`, `RateLimited`, and `Upstream` return `true`.
    /// Test: `sm_llm_error_classification`.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            SmLlmError::Transport(_) | SmLlmError::RateLimited | SmLlmError::Upstream { .. }
        )
    }

    /// Returns `true` when the SM is in degraded (no-provider) mode.
    ///
    /// Why: the SM-7 endpoint / SM-STDIO adapter renders a graceful notice for
    /// this case instead of an error, per §5.3.
    /// What: `true` only for the `Degraded` variant.
    /// Test: `sm_llm_error_classification`.
    pub fn is_degraded(&self) -> bool {
        matches!(self, SmLlmError::Degraded(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: pin the retry/alarm/degraded classification so a future variant
    /// addition can't silently mis-route the fallback loop.
    /// What: asserts each variant's `is_alarm`/`is_retryable`/`is_degraded`.
    /// Test: this is the test.
    #[test]
    fn sm_llm_error_classification() {
        let alarm: Vec<SmLlmError> = vec![
            SmLlmError::ModelNotFound("x".into()),
            SmLlmError::ModelNotReady("x".into()),
            SmLlmError::Validation("x".into()),
            SmLlmError::AccessDenied("x".into()),
        ];
        for e in &alarm {
            assert!(e.is_alarm(), "{e:?} should alarm");
            assert!(!e.is_retryable(), "{e:?} should not retry");
            assert!(!e.is_degraded(), "{e:?} is not degraded");
        }

        let transient: Vec<SmLlmError> = vec![
            SmLlmError::Transport("x".into()),
            SmLlmError::RateLimited,
            SmLlmError::Upstream {
                status: 503,
                body: "x".into(),
            },
        ];
        for e in &transient {
            assert!(!e.is_alarm(), "{e:?} should not alarm");
            assert!(e.is_retryable(), "{e:?} should retry");
            assert!(!e.is_degraded(), "{e:?} is not degraded");
        }

        let degraded = SmLlmError::Degraded("no keys".into());
        assert!(!degraded.is_alarm());
        assert!(!degraded.is_retryable());
        assert!(degraded.is_degraded());
    }

    /// Why: error messages feed operator-facing logs / notices; keep them
    /// informative and stable.
    /// What: asserts the `Display` output of representative variants.
    /// Test: this is the test.
    #[test]
    fn sm_llm_error_messages() {
        assert_eq!(
            SmLlmError::ModelNotFound("anthropic/claude-x".into()).to_string(),
            "model not found: anthropic/claude-x"
        );
        assert_eq!(SmLlmError::RateLimited.to_string(), "rate limited");
        assert_eq!(
            SmLlmError::Upstream {
                status: 503,
                body: "overloaded".into()
            }
            .to_string(),
            "upstream error (HTTP 503): overloaded"
        );
        assert_eq!(
            SmLlmError::Degraded("no ANTHROPIC_API_KEY / AWS / OPENROUTER_API_KEY".into())
                .to_string(),
            "degraded: no inference provider configured (no ANTHROPIC_API_KEY / AWS / OPENROUTER_API_KEY)"
        );
    }
}
