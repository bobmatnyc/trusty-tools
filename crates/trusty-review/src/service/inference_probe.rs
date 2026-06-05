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
//! polls don't hammer the provider.  The per-probe timeout is configurable via
//! `TRUSTY_REVIEW_HEALTH_TIMEOUT_SECS` (default 10 s, see #739); a timed-out
//! probe returns `Unknown` rather than `Unreachable` so a slow Bedrock cold-start
//! does not falsely degrade health — real review calls have a ~300 s budget, so
//! a probe timeout should not be treated as a hard unreachability signal.
//! `InferenceStatus` maps provider errors to the four states: `ok`,
//! `unreachable`, `auth_error`, `unknown`.
//!
//! Test: `probe_status_*` unit tests in this module inject stub providers and
//! verify each status transition. Live credential tests are separate (ignored).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::debug;

// ─── Configurable timeout helper ─────────────────────────────────────────────

/// Return the per-probe hard timeout, consulting `TRUSTY_REVIEW_HEALTH_TIMEOUT_SECS`.
///
/// Why: AWS Bedrock cold-starts were measured at ~7.4 s (#739), which exceeded
/// the previous hard-coded 3 s timeout and caused spurious `unreachable` /
/// `degraded` health results.  Making the timeout configurable lets operators
/// tune it without recompiling; 10 s comfortably covers observed cold-start
/// latency while still bounding health-check latency.
/// What: reads `TRUSTY_REVIEW_HEALTH_TIMEOUT_SECS` from the environment; parses
/// it as a `u64`; falls back to `DEFAULT_HEALTH_TIMEOUT_SECS` (10) on any
/// parse failure or if the variable is unset.  A value of 0 is treated as
/// "use default" to prevent an accidentally zero timeout from hanging forever.
/// Test: `health_probe_timeout_default`, `health_probe_timeout_env_override`,
/// `health_probe_timeout_env_invalid_falls_back`,
/// `health_probe_timeout_env_zero_falls_back` in the `tests` module.
pub fn health_probe_timeout() -> Duration {
    const DEFAULT_HEALTH_TIMEOUT_SECS: u64 = 10;
    const ENV_VAR: &str = "TRUSTY_REVIEW_HEALTH_TIMEOUT_SECS";

    let secs = std::env::var(ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(DEFAULT_HEALTH_TIMEOUT_SECS);

    Duration::from_secs(secs)
}

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
    /// Default TTL is 10 seconds; probe timeout reads `TRUSTY_REVIEW_HEALTH_TIMEOUT_SECS` (default 10 s).
    ///
    /// Why: the previous hard-coded 3 s timeout was shorter than observed AWS
    /// Bedrock cold-start latency (~7.4 s), causing spurious `unreachable` /
    /// `degraded` health results (#739).  The timeout is now 10 s by default and
    /// configurable via `TRUSTY_REVIEW_HEALTH_TIMEOUT_SECS` so operators can tune
    /// it without recompiling.
    /// What: returns an `InferenceProbe` with `ttl=10s` and `probe_timeout` from
    /// `health_probe_timeout()`.
    /// Test: `probe_default_starts_unknown`.
    fn default() -> Self {
        Self::new(Duration::from_secs(10), health_probe_timeout())
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
/// A timeout returns `Unknown` rather than `Unreachable` (#739): the probe
/// budget is much shorter than the real review budget (~300 s), so a slow
/// cold-start should not be reported as "endpoint unreachable" — it is simply
/// "could not confirm reachability within the probe window".
/// Test: `probe_returns_ok_on_success`, `probe_returns_auth_error_on_access_denied`,
/// `probe_returns_unreachable_on_transport`, `probe_timeout_returns_unknown`.
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
        // Timed out → could not confirm reachability within the probe window.
        // Use `Unknown` (not `Unreachable`) so a slow Bedrock cold-start does not
        // falsely degrade health (#739).  Real review calls have a ~300 s budget.
        Err(_elapsed) => {
            debug!("inference probe: timed out — returning Unknown (not Unreachable)");
            InferenceStatus::Unknown
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
// Split into a sibling file to keep this file under the 500-line cap (#610).
// The sibling file is included as the module body via `#[path = ...]` so it
// has full access to private items (`run_probe`, etc.) just as inline tests would.

#[cfg(test)]
#[path = "inference_probe_tests.rs"]
mod tests;
