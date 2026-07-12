//! [`InferenceError`] — the structured error surface for inference calls.
//!
//! Why: a library must expose structured errors (not `anyhow`) so callers can
//! programmatically decide "retry this" vs. "this needs an operator". The two
//! classifications every consumer needs — is this transient/retryable, and is
//! this an alarm (config/auth/capability problem a human must fix) — are folded
//! into methods on one enum so the policy lives in one place, not re-derived at
//! each call site. It unions the failure modes tcode's `LlmError` and
//! trusty-review's client already distinguish so #2406 maps onto it directly.
//! What: [`InferenceError`] with a variant per failure class, plus
//! [`InferenceError::is_retryable`] and [`InferenceError::is_alarm`] classifiers.
//! Concrete HTTP adapters (#2403) map `reqwest`/SDK errors into these variants.
//! Test: inline `tests` — `retryable_classification`, `alarm_classification`,
//! `display_includes_context`.

use crate::inference::registry::ProviderId;

/// Errors raised by an [`super::adapter::InferenceAdapter`] or the configurator.
///
/// Why: structured variants let the agent loop pattern-match on failure mode —
/// a 429 or a transport blip is retryable, a 401 or a missing credential is an
/// alarm — without parsing display strings.
/// What: transport, API (non-2xx), deserialisation, credential-resolution,
/// adapter-registration, provider-specific, and unsupported-capability variants.
/// Kept `String`-carrying (rather than `#[from] reqwest::Error`) so this
/// foundation crate stays free of a concrete HTTP dependency; #2403's adapters
/// stringify their transport errors into [`Self::Transport`].
/// Test: `retryable_classification`, `alarm_classification`.
#[derive(Debug, thiserror::Error)]
pub enum InferenceError {
    /// The HTTP/transport layer failed (connection refused, TLS, timeout).
    #[error("inference transport error: {0}")]
    Transport(String),

    /// The provider returned a non-2xx status.
    #[error("inference API error {status}: {body}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Raw response body (may be JSON or plain text).
        body: String,
    },

    /// The response body could not be deserialised into a [`super::ChatResponse`].
    #[error("inference response deserialisation failed: {message}")]
    Deserialise {
        /// Human-readable parse error (never contains the API key).
        message: String,
        /// The raw body that failed to parse (aids debugging).
        body: String,
    },

    /// No credential could be resolved for the target provider.
    ///
    /// Why: distinct from an API 401 — this fires before any network call, when
    /// the configurator's env/`.env.local`/store chain yields nothing.
    #[error("no credential resolved for provider {provider}")]
    MissingCredential {
        /// The provider whose key could not be found.
        provider: ProviderId,
    },

    /// The configurator has no adapter factory registered for the resolved
    /// provider.
    ///
    /// Why: the two-stage resolver picked a provider, but no adapter was wired
    /// in for it (in this foundation ticket only the test-support factory exists;
    /// real factories register in #2403).
    #[error("no adapter registered for provider {provider}")]
    NoAdapterRegistered {
        /// The provider that resolved but has no factory.
        provider: ProviderId,
    },

    /// A provider-specific failure that does not fit the HTTP shapes (e.g. an
    /// AWS Bedrock SDK error).
    #[error("inference provider error: {0}")]
    Provider(String),

    /// A requested capability is not supported by the target provider/model.
    #[error("unsupported inference capability: {0}")]
    Unsupported(String),
}

impl InferenceError {
    /// Whether retrying the same call may succeed.
    ///
    /// Why: the agent loop applies backoff+retry to transient failures only —
    /// retrying a 401 or a schema mismatch just wastes budget.
    /// What: `true` for [`Self::Transport`] and for [`Self::Api`] with a 429 or
    /// any 5xx status; `false` otherwise.
    /// Test: `retryable_classification`.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(_) => true,
            Self::Api { status, .. } => *status == 429 || (500..=599).contains(status),
            _ => false,
        }
    }

    /// Whether this failure needs operator attention rather than a retry.
    ///
    /// Why: config/auth/capability problems are dead-ends for automated retry —
    /// surfacing them as alarms lets the caller stop and report instead of
    /// spinning.
    /// What: `true` for missing-credential, no-adapter-registered, unsupported,
    /// and authentication/authorisation/not-found API statuses (401/403/404);
    /// `false` otherwise.
    /// Test: `alarm_classification`.
    pub fn is_alarm(&self) -> bool {
        match self {
            Self::MissingCredential { .. }
            | Self::NoAdapterRegistered { .. }
            | Self::Unsupported(_) => true,
            Self::Api { status, .. } => matches!(status, 401 | 403 | 404),
            _ => false,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: only transient failures may be retried.
    /// Test: itself.
    #[test]
    fn retryable_classification() {
        assert!(InferenceError::Transport("timeout".into()).is_retryable());
        assert!(
            InferenceError::Api {
                status: 429,
                body: "slow down".into()
            }
            .is_retryable()
        );
        assert!(
            InferenceError::Api {
                status: 503,
                body: "unavailable".into()
            }
            .is_retryable()
        );
        assert!(
            !InferenceError::Api {
                status: 401,
                body: "bad key".into()
            }
            .is_retryable()
        );
        assert!(
            !InferenceError::MissingCredential {
                provider: ProviderId::OpenRouter
            }
            .is_retryable()
        );
    }

    /// Why: config/auth/capability failures must alarm, not retry.
    /// Test: itself.
    #[test]
    fn alarm_classification() {
        assert!(
            InferenceError::MissingCredential {
                provider: ProviderId::Anthropic
            }
            .is_alarm()
        );
        assert!(
            InferenceError::NoAdapterRegistered {
                provider: ProviderId::Fireworks
            }
            .is_alarm()
        );
        assert!(InferenceError::Unsupported("vision".into()).is_alarm());
        assert!(
            InferenceError::Api {
                status: 403,
                body: "denied".into()
            }
            .is_alarm()
        );
        assert!(!InferenceError::Transport("blip".into()).is_alarm());
    }

    /// Why: display strings feed logs and must carry the actionable context.
    /// Test: itself.
    #[test]
    fn display_includes_context() {
        let e = InferenceError::Api {
            status: 429,
            body: "rate limited".into(),
        };
        let s = e.to_string();
        assert!(s.contains("429"), "{s}");
        assert!(s.contains("rate limited"), "{s}");
    }
}
