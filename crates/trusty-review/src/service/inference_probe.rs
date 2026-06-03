//! Inference-reachability probe for the `review_health` endpoint.
//!
//! Why: MPM and other orchestrators need to distinguish "service up AND
//! inference working" from "service up but creds expired / endpoint
//! unreachable" before paying the cost of a full `review_pr` call.
//! A cheap, cached liveness probe lets callers gate on a single JSON field
//! (`inference`) rather than attempting a real review and handling the failure.
//!
//! What: `InferenceProbe` wraps a short-TTL cache (default 10 s) around a
//! minimal real LLM call (max_tokens=1).  The cache means repeated `/health`
//! polls don't hammer the provider; a 3 s timeout means a hung endpoint can't
//! stall health checks.  `InferenceStatus` maps provider errors to the four
//! states: `ok`, `unreachable`, `auth_error`, `unknown`.
//!
//! Test: `probe_status_*` unit tests in this module inject stub providers and
//! verify each status transition. Live credential tests are separate (ignored).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::debug;

#[cfg(test)]
use crate::llm::LlmResponse;
use crate::llm::{ChatMessage, LlmError, LlmProvider, LlmRequest};

// ─── Status enum ─────────────────────────────────────────────────────────────

/// Inference-reachability status produced by the lightweight probe.
///
/// Why: callers need to distinguish four distinct states so they can decide
/// the appropriate remediation (retry vs. fix creds vs. check network).
/// What: serialises as lowercase string (`"ok"`, `"unreachable"`, etc.).
/// Test: `probe_status_serialises_lowercase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceStatus {
    /// Provider responded successfully to the probe request.
    Ok,
    /// Network or connectivity failure: DNS, TCP, TLS, timeout, or 5xx.
    Unreachable,
    /// Authentication or authorisation failure: 401, 403, or missing creds.
    AuthError,
    /// Probe could not be attempted (no provider configured, or build error).
    Unknown,
}

impl InferenceStatus {
    /// Returns `true` when the inference endpoint is confirmed healthy.
    ///
    /// Why: callers that only need a boolean gate (e.g. `status` → `"degraded"`)
    /// can call this without pattern-matching.
    /// What: `true` only for `Ok`; all other states are not ok.
    /// Test: `probe_status_is_ok`.
    pub fn is_ok(self) -> bool {
        self == InferenceStatus::Ok
    }
}

impl std::fmt::Display for InferenceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            InferenceStatus::Ok => "ok",
            InferenceStatus::Unreachable => "unreachable",
            InferenceStatus::AuthError => "auth_error",
            InferenceStatus::Unknown => "unknown",
        };
        write!(f, "{s}")
    }
}

// ─── Error-to-status mapping ──────────────────────────────────────────────────

/// Map an `LlmError` to the appropriate `InferenceStatus`.
///
/// Why: the four `InferenceStatus` variants each correspond to a subset of
/// `LlmError` variants; centralising the mapping keeps it consistent across
/// HTTP and MCP paths.
/// What: auth/validation errors → `AuthError`; transport/rate/5xx → `Unreachable`.
/// Test: `error_mapping_*` tests cover each `LlmError` variant.
pub fn map_llm_error(err: &LlmError) -> InferenceStatus {
    match err {
        // Access denied and model-not-found are both auth/config problems.
        LlmError::AccessDenied(_) | LlmError::ModelNotFound(_) | LlmError::ModelNotReady(_) => {
            InferenceStatus::AuthError
        }
        // Validation (e.g. bad model prefix) is a config problem, not connectivity.
        LlmError::Validation(_) => InferenceStatus::AuthError,
        // Network-level or 5xx: may be transient but connectivity is broken.
        LlmError::Transport(_) | LlmError::RateLimited | LlmError::Upstream { .. } => {
            InferenceStatus::Unreachable
        }
    }
}

// ─── Cached probe ─────────────────────────────────────────────────────────────

/// Cached inference-reachability probe.
///
/// Why: running a live LLM call on every `/health` hit is expensive and slow.
/// The cache amortises the probe cost across a configurable TTL window (default
/// 10 s) so repeated health polls don't hammer the provider.
/// What: holds a `Mutex`-guarded `Option<(InferenceStatus, Instant)>`.  `probe`
/// returns the cached value if it is younger than `ttl`; otherwise it runs a
/// fresh probe (with a short per-call timeout) and updates the cache.
/// Test: `probe_cache_ttl_*` tests use a mock provider to verify that the cache
/// is populated on the first call and reused until expiry.
#[derive(Clone)]
pub struct InferenceProbe {
    /// Cached result: `None` = never probed.
    cached: Arc<Mutex<Option<(InferenceStatus, Instant)>>>,
    /// Probe TTL.  Results younger than this are returned directly from cache.
    ttl: Duration,
    /// Per-probe hard timeout.  A probe that exceeds this → `Unreachable`.
    probe_timeout: Duration,
}

impl Default for InferenceProbe {
    /// Default TTL is 10 seconds; probe timeout is 3 seconds.
    ///
    /// Why: matches the design brief — 10 s prevents hammering providers on
    /// repeated health polls; 3 s ensures a hung endpoint doesn't stall /health.
    /// What: returns an `InferenceProbe` with `ttl=10s`, `probe_timeout=3s`.
    /// Test: `probe_default_starts_unknown`.
    fn default() -> Self {
        Self::new(Duration::from_secs(10), Duration::from_secs(3))
    }
}

impl InferenceProbe {
    /// Create a probe with the given TTL and per-call timeout.
    ///
    /// Why: allows tests to inject very short TTLs to exercise cache expiry
    /// without sleeping 10 s in CI.
    /// What: builds an empty cache and stores the two durations.
    /// Test: `probe_cache_ttl_zero_always_reprobes`.
    pub fn new(ttl: Duration, probe_timeout: Duration) -> Self {
        Self {
            cached: Arc::new(Mutex::new(None)),
            ttl,
            probe_timeout,
        }
    }

    /// Run the probe, returning the cached result if it is still fresh.
    ///
    /// Why: lets `/health` and `review_health` share the same cached probe
    /// without duplicating the caching logic.
    /// What: reads the cache under the mutex first.  If the result is still
    /// within TTL, returns it without any async work.  Otherwise releases the
    /// mutex, runs a fresh probe (with timeout), re-acquires the mutex, stores
    /// the new result, and returns it.
    /// Test: `probe_returns_ok_on_success`, `probe_returns_unreachable_on_transport`.
    pub async fn probe(&self, llm: &Arc<dyn LlmProvider>, model: &str) -> InferenceStatus {
        // ── Read cache ────────────────────────────────────────────────────────
        {
            let guard = self.cached.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((status, ts)) = *guard
                && ts.elapsed() < self.ttl
            {
                debug!(status = %status, "inference probe: cache hit");
                return status;
            }
        }

        // ── Run live probe ────────────────────────────────────────────────────
        let status = run_probe(llm, model, self.probe_timeout).await;
        debug!(status = %status, "inference probe: fresh result");

        // ── Update cache ──────────────────────────────────────────────────────
        {
            let mut guard = self.cached.lock().unwrap_or_else(|p| p.into_inner());
            *guard = Some((status, Instant::now()));
        }

        status
    }
}

// ─── Low-level probe ──────────────────────────────────────────────────────────

/// Issue the smallest possible real request to the LLM provider.
///
/// Why: a real (not mocked) request exercises both connectivity AND auth,
/// so we can distinguish `unreachable` from `auth_error` — a purely
/// credential-check API (if one existed) would not verify connectivity.
/// What: sends a 1-token completion with a trivial prompt through the provider;
/// maps any error to `InferenceStatus` via `map_llm_error`.  The call is
/// wrapped in `tokio::time::timeout` so a hung endpoint never stalls /health.
/// Test: `probe_returns_ok_on_success`, `probe_returns_auth_error_on_access_denied`,
/// `probe_returns_unreachable_on_transport`, `probe_respects_timeout`.
async fn run_probe(llm: &Arc<dyn LlmProvider>, model: &str, timeout: Duration) -> InferenceStatus {
    let req = LlmRequest {
        model: model.to_string(),
        system: String::new(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: "hi".to_string(),
        }],
        temperature: 0.0,
        max_tokens: 1,
        response_schema: None,
    };

    let result = tokio::time::timeout(timeout, llm.complete(req)).await;

    match result {
        // Timed out → endpoint unreachable / hung.
        Err(_elapsed) => {
            debug!("inference probe: timed out");
            InferenceStatus::Unreachable
        }
        // Call completed — check the inner result.
        Ok(Ok(_)) => InferenceStatus::Ok,
        Ok(Err(e)) => {
            debug!(error = %e, "inference probe: provider error");
            map_llm_error(&e)
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU32, Ordering};

    // ── Stub providers ────────────────────────────────────────────────────────

    struct OkLlm;

    #[async_trait]
    impl LlmProvider for OkLlm {
        fn name(&self) -> &str {
            "ok-stub"
        }

        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                text: "hi".to_string(),
                model: req.model.clone(),
                input_tokens: 1,
                output_tokens: 1,
                latency_ms: 0,
                cost_usd: 0.0,
            })
        }
    }

    struct AuthErrorLlm;

    #[async_trait]
    impl LlmProvider for AuthErrorLlm {
        fn name(&self) -> &str {
            "auth-error-stub"
        }

        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Err(LlmError::AccessDenied("invalid api key".into()))
        }
    }

    struct TransportErrorLlm;

    #[async_trait]
    impl LlmProvider for TransportErrorLlm {
        fn name(&self) -> &str {
            "transport-stub"
        }

        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Transport("connection refused".into()))
        }
    }

    struct HungLlm;

    #[async_trait]
    impl LlmProvider for HungLlm {
        fn name(&self) -> &str {
            "hung-stub"
        }

        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            // Simulate an endpoint that never responds within the probe timeout.
            tokio::time::sleep(Duration::from_secs(60)).await;
            Err(LlmError::Transport("hung".into()))
        }
    }

    /// A counting stub to verify the cache prevents redundant probes.
    struct CountingLlm {
        calls: Arc<AtomicU32>,
    }

    #[async_trait]
    impl LlmProvider for CountingLlm {
        fn name(&self) -> &str {
            "counting-stub"
        }

        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(LlmResponse {
                text: "x".into(),
                model: req.model.clone(),
                input_tokens: 1,
                output_tokens: 1,
                latency_ms: 0,
                cost_usd: 0.0,
            })
        }
    }

    // ── InferenceStatus tests ─────────────────────────────────────────────────

    #[test]
    fn probe_status_serialises_lowercase() {
        assert_eq!(
            serde_json::to_string(&InferenceStatus::Ok).unwrap(),
            "\"ok\""
        );
        assert_eq!(
            serde_json::to_string(&InferenceStatus::Unreachable).unwrap(),
            "\"unreachable\""
        );
        assert_eq!(
            serde_json::to_string(&InferenceStatus::AuthError).unwrap(),
            "\"auth_error\""
        );
        assert_eq!(
            serde_json::to_string(&InferenceStatus::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    #[test]
    fn probe_status_is_ok() {
        assert!(InferenceStatus::Ok.is_ok());
        assert!(!InferenceStatus::Unreachable.is_ok());
        assert!(!InferenceStatus::AuthError.is_ok());
        assert!(!InferenceStatus::Unknown.is_ok());
    }

    // ── Error mapping tests ───────────────────────────────────────────────────

    #[test]
    fn error_mapping_access_denied_is_auth_error() {
        let status = map_llm_error(&LlmError::AccessDenied("denied".into()));
        assert_eq!(status, InferenceStatus::AuthError);
    }

    #[test]
    fn error_mapping_model_not_found_is_auth_error() {
        let status = map_llm_error(&LlmError::ModelNotFound("no-model".into()));
        assert_eq!(status, InferenceStatus::AuthError);
    }

    #[test]
    fn error_mapping_model_not_ready_is_auth_error() {
        let status = map_llm_error(&LlmError::ModelNotReady("creating".into()));
        assert_eq!(status, InferenceStatus::AuthError);
    }

    #[test]
    fn error_mapping_validation_is_auth_error() {
        let status = map_llm_error(&LlmError::Validation("bad prefix".into()));
        assert_eq!(status, InferenceStatus::AuthError);
    }

    #[test]
    fn error_mapping_transport_is_unreachable() {
        let status = map_llm_error(&LlmError::Transport("connection refused".into()));
        assert_eq!(status, InferenceStatus::Unreachable);
    }

    #[test]
    fn error_mapping_rate_limited_is_unreachable() {
        let status = map_llm_error(&LlmError::RateLimited);
        assert_eq!(status, InferenceStatus::Unreachable);
    }

    #[test]
    fn error_mapping_upstream_5xx_is_unreachable() {
        let status = map_llm_error(&LlmError::Upstream {
            status: 503,
            body: "overloaded".into(),
        });
        assert_eq!(status, InferenceStatus::Unreachable);
    }

    // ── Live probe tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn probe_returns_ok_on_success() {
        let llm: Arc<dyn LlmProvider> = Arc::new(OkLlm);
        let status = run_probe(&llm, "test-model", Duration::from_secs(5)).await;
        assert_eq!(status, InferenceStatus::Ok);
    }

    #[tokio::test]
    async fn probe_returns_auth_error_on_access_denied() {
        let llm: Arc<dyn LlmProvider> = Arc::new(AuthErrorLlm);
        let status = run_probe(&llm, "test-model", Duration::from_secs(5)).await;
        assert_eq!(status, InferenceStatus::AuthError);
    }

    #[tokio::test]
    async fn probe_returns_unreachable_on_transport() {
        let llm: Arc<dyn LlmProvider> = Arc::new(TransportErrorLlm);
        let status = run_probe(&llm, "test-model", Duration::from_secs(5)).await;
        assert_eq!(status, InferenceStatus::Unreachable);
    }

    #[tokio::test(start_paused = true)]
    async fn probe_respects_timeout() {
        // The HungLlm sleeps 60 s; the probe timeout is 10 ms.
        // With paused clock, `tokio::time::sleep` returns instantly when
        // the runtime advances time; `timeout` fires before HungLlm wakes.
        let llm: Arc<dyn LlmProvider> = Arc::new(HungLlm);
        let status = run_probe(&llm, "test-model", Duration::from_millis(10)).await;
        assert_eq!(
            status,
            InferenceStatus::Unreachable,
            "hung endpoint must produce Unreachable"
        );
    }

    // ── Cache TTL tests ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn probe_cache_prevents_redundant_calls() {
        let calls = Arc::new(AtomicU32::new(0));
        let llm: Arc<dyn LlmProvider> = Arc::new(CountingLlm {
            calls: Arc::clone(&calls),
        });

        // Long TTL: 60 s — the cache should remain warm across both calls.
        let probe = InferenceProbe::new(Duration::from_secs(60), Duration::from_secs(5));

        let s1 = probe.probe(&llm, "m").await;
        let s2 = probe.probe(&llm, "m").await;

        assert_eq!(s1, InferenceStatus::Ok);
        assert_eq!(s2, InferenceStatus::Ok);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "provider must be called exactly once when cache is warm"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn probe_cache_ttl_zero_always_reprobes() {
        let calls = Arc::new(AtomicU32::new(0));
        let llm: Arc<dyn LlmProvider> = Arc::new(CountingLlm {
            calls: Arc::clone(&calls),
        });

        // TTL of 0 means the cache always looks stale.
        let probe = InferenceProbe::new(Duration::ZERO, Duration::from_secs(5));

        probe.probe(&llm, "m").await;
        probe.probe(&llm, "m").await;

        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "zero TTL must reprobe on every call"
        );
    }
}
