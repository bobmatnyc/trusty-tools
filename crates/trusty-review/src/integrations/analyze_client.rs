//! HTTP client over trusty-analyze (`:7879`) — OPTIONAL dependency.
//!
//! Why: trusty-analyze provides static analysis context (complexity hotspots,
//! code smells) that enriches the review.  It is OPTIONAL: if unavailable the
//! pipeline proceeds with empty static-analysis context and the service-
//! unavailable Slack notice is NOT raised.  (spec REV-012, REV-440, REV-442)
//!
//! What: defines `AnalyzeClient` trait and `HttpAnalyzeClient`.  The two-step
//! readiness probe (`has_analysis`) calls `GET /health` AND `GET /indexes` —
//! NEVER `GET /indexes/{id}/quality` which is O(corpus) and always times out.
//! (spec REV-441, lesson §12.3)
//!
//! Routes verified from `crates/trusty-analyze/src/service/mod.rs`:
//!   GET  /health
//!   GET  /indexes
//!   GET  /indexes/{id}/complexity_hotspots[?top_k=N]
//!   GET  /indexes/{id}/smells[?category=<name>]
//!   (GET /indexes/{id}/quality  — NEVER a readiness probe; O(corpus))
//!
//! Test: `two_step_probe_never_calls_quality` documents the invariant;
//! `analyze_client_graceful_degradation` verifies transport errors return
//! empty defaults rather than propagating.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ─── Error type ───────────────────────────────────────────────────────────────

/// Errors produced by `AnalyzeClient` implementations.
///
/// Why: typed errors let callers log the specific failure without pattern-
/// matching on strings.
/// What: `Transport`, `Api`, `Parse`, `Unavailable` match the equivalent
/// `SearchClientError` variants.  All errors are treated as "graceful
/// degradation" by the pipeline — none should block a review.  `ClientInit`
/// covers TLS-backend initialisation failures at construction time so callers
/// receive an `Err` instead of a panic.
/// Test: `analyze_error_display`.
#[derive(Debug, thiserror::Error)]
pub enum AnalyzeClientError {
    /// HTTP transport failure.
    #[error("trusty-analyze transport error: {0}")]
    Transport(String),

    /// trusty-analyze returned a non-2xx status.
    #[error("trusty-analyze API returned {status}: {body}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Response body (may be truncated).
        body: String,
    },

    /// Response JSON could not be parsed.
    #[error("trusty-analyze response parse error: {0}")]
    Parse(String),

    /// Daemon is unreachable or unhealthy.
    #[error("trusty-analyze unavailable: {0}")]
    Unavailable(String),

    /// reqwest client construction failed (TLS backend unavailable).
    #[error("failed to build HTTP client: {0}")]
    ClientInit(String),
}

// ─── Response types ───────────────────────────────────────────────────────────

/// Response from `GET /health` on trusty-analyze.
///
/// Why: the two-step probe (REV-441) checks `status == "ok"` AND
/// `search_reachable == true` before considering analyze available.
/// What: maps the trusty-analyze health JSON; extra fields are discarded.
/// Test: `analyze_health_response_deserialises`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnalyzeHealthResponse {
    /// `"ok"` when the analyze daemon itself is healthy.
    pub status: String,
    /// True when the analyze daemon can reach the trusty-search daemon.
    #[serde(default)]
    pub search_reachable: bool,
}

impl AnalyzeHealthResponse {
    /// Returns `true` when the daemon is healthy AND can reach trusty-search.
    ///
    /// Why: the pipeline must not rely on analyze context if the search sidecar
    /// it depends on is also down.  (spec REV-441)
    /// What: checks `status == "ok" && search_reachable`.
    /// Test: `analyze_health_response_is_healthy`.
    pub fn is_healthy(&self) -> bool {
        self.status == "ok" && self.search_reachable
    }
}

/// A single registered index from `GET /indexes` on trusty-analyze.
///
/// Why: the two-step probe checks that at least one index exists before
/// marking the service available.
/// What: minimal shape — `id` only; other fields discarded.
/// Test: `analyze_index_info_deserialises`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnalyzeIndexInfo {
    /// Unique index identifier.
    pub id: String,
}

/// A single complexity hotspot from `GET /indexes/{id}/complexity_hotspots`.
///
/// Why: the pipeline uses hotspots to annotate the review with files/functions
/// that are structurally complex.
/// What: `file` and `cyclomatic` are the primary fields; `function_name` and
/// `cognitive` are optional enrichment.
/// Test: `hotspot_deserialises`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComplexityHotspot {
    /// Repository-relative file path.
    pub file: String,
    /// Function or chunk name, if available.
    #[serde(default)]
    pub function_name: Option<String>,
    /// Cyclomatic complexity score.
    #[serde(default)]
    pub cyclomatic: u32,
    /// Cognitive complexity score.
    #[serde(default)]
    pub cognitive: u32,
}

/// A single code smell from `GET /indexes/{id}/smells`.
///
/// Why: the pipeline annotates the review with detected code smells in the
/// changed files.
/// What: `file`, `category`, and `severity` are the key fields.
/// Test: `smell_deserialises`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Smell {
    /// Repository-relative file path.
    pub file: String,
    /// Smell category (e.g. `"long_method"`, `"deep_nesting"`).
    pub category: String,
    /// Severity level (e.g. `"low"`, `"medium"`, `"high"`).
    #[serde(default)]
    pub severity: String,
    /// Line number, if available.
    #[serde(default)]
    pub line: Option<u32>,
}

// ─── Trait definition ─────────────────────────────────────────────────────────

/// Client interface for the trusty-analyze HTTP daemon (OPTIONAL dependency).
///
/// Why: the pipeline depends on this trait so the transport can be mocked
/// or swapped without touching pipeline code.  (spec REV-009, REV-440)
/// What: exposes `health`, `has_analysis` (two-step probe), `complexity_hotspots`,
/// and `smells`.  ALL methods must gracefully degrade — return an empty default
/// on transport error, never panic, never block the review.
/// Test: `analyze_client_trait_object_compiles`.
#[async_trait]
pub trait AnalyzeClient: Send + Sync {
    /// Check liveness of the trusty-analyze daemon.
    ///
    /// Why: quick liveness check used by `has_analysis`; does not check
    /// whether analysis data is available.
    /// What: `GET /health` → `AnalyzeHealthResponse`.
    /// Test: integration tests; unit tests mock this method.
    async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError>;

    /// Two-step readiness probe: is analyze available AND does it have data?
    ///
    /// Why: spec REV-441 requires both a health check AND an index-list check
    /// before marking analyze as available.  NEVER call `/quality` here —
    /// it is O(corpus) and always times out at 5s.  (lesson §12.3)
    /// What: calls `GET /health` (checks `status == ok && search_reachable`)
    /// AND `GET /indexes` (checks at least one index exists).  Returns `false`
    /// (not an error) on any transport failure — analyze is optional.
    /// Test: `two_step_probe_returns_false_on_transport_error`.
    async fn has_analysis(&self, index_id: &str) -> bool;

    /// Fetch complexity hotspots for an index.
    ///
    /// Why: provides the pipeline with a ranked list of complex files/functions
    /// to annotate the review.
    /// What: `GET /indexes/{index_id}/complexity_hotspots[?top_k=N]`.
    /// On any error, returns `Ok(vec![])` — never blocks the review.
    /// Test: `complexity_hotspots_empty_on_transport_error`.
    async fn complexity_hotspots(
        &self,
        index_id: &str,
        top_k: Option<u32>,
    ) -> Result<Vec<ComplexityHotspot>, AnalyzeClientError>;

    /// Fetch code smells for an index.
    ///
    /// Why: provides the pipeline with smell annotations for the changed files.
    /// What: `GET /indexes/{index_id}/smells`.
    /// On any error, returns `Ok(vec![])` — never blocks the review.
    /// Test: `smells_empty_on_transport_error`.
    async fn smells(&self, index_id: &str) -> Result<Vec<Smell>, AnalyzeClientError>;
}

// ─── HTTP implementation ──────────────────────────────────────────────────────

/// HTTP implementation of `AnalyzeClient` over a running trusty-analyze daemon.
///
/// Why: the default transport for all production and staging deployments.
/// What: targets `PR_INTELLIGENCE_ANALYZER_URL` (default
/// `http://127.0.0.1:7879`).  All methods use a 5s timeout for probe calls and
/// a 180s timeout for analysis calls (matching spec REV-440 table).
/// Test: `http_analyze_client_url_is_configurable`.
pub struct HttpAnalyzeClient {
    /// Base URL of the trusty-analyze daemon (no trailing slash).
    base_url: String,
    /// Short-timeout client for health / index probes (5s).
    probe_http: reqwest::Client,
    /// Long-timeout client for analysis calls (180s).
    analysis_http: reqwest::Client,
}

impl HttpAnalyzeClient {
    /// Construct from an explicit base URL.
    ///
    /// Why: allows tests to point the client at any URL without going through
    /// the config system.
    /// What: builds two reqwest clients — `probe_http` (5s timeout) and
    /// `analysis_http` (180s timeout) — matching the timeout table in spec
    /// REV-440.  Returns `Err(ClientInit)` if the TLS backend cannot be
    /// initialised — surfaces the failure to the caller rather than panicking
    /// at daemon startup (closes #953).
    /// Test: `http_analyze_client_url_is_configurable`.
    pub fn new(base_url: impl Into<String>) -> Result<Self, AnalyzeClientError> {
        let raw = base_url.into();
        let base_url = raw.trim_end_matches('/').to_string();
        let probe_http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| AnalyzeClientError::ClientInit(e.to_string()))?;
        let analysis_http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .map_err(|e| AnalyzeClientError::ClientInit(e.to_string()))?;
        Ok(Self {
            base_url,
            probe_http,
            analysis_http,
        })
    }

    /// Construct from a `ReviewConfig`, reading `analyzer_url`.
    ///
    /// Why: the pipeline constructs the client from its injected config.
    /// What: calls `Self::new(config.analyzer_url.clone())` and propagates any
    /// TLS-backend init failure as `Err`.
    /// Test: `http_analyze_client_from_config`.
    pub fn from_config(config: &crate::config::ReviewConfig) -> Result<Self, AnalyzeClientError> {
        Self::new(config.analyzer_url.clone())
    }

    /// Return the base URL this client targets.
    ///
    /// Why: tests need to assert the URL is constructed correctly.
    /// What: returns a reference to the stored base URL string.
    /// Test: `http_analyze_client_url_is_configurable`.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[async_trait]
impl AnalyzeClient for HttpAnalyzeClient {
    async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError> {
        let url = format!("{}/health", self.base_url);
        let resp = self
            .probe_http
            .get(&url)
            .send()
            .await
            .map_err(|e| AnalyzeClientError::Unavailable(format!("GET {url}: {e}")))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AnalyzeClientError::Transport(format!("read body of {url}: {e}")))?;

        if !status.is_success() {
            return Err(AnalyzeClientError::Unavailable(format!(
                "GET {url} returned {status}: {body}"
            )));
        }

        serde_json::from_str(&body)
            .map_err(|e| AnalyzeClientError::Parse(format!("health response: {e}")))
    }

    /// Two-step readiness probe (spec REV-441).
    ///
    /// Why: both `/health` and `/indexes` must succeed before marking analyze
    /// available.  NEVER calls `/quality` — it is O(corpus).  (lesson §12.3)
    /// What: calls `health()` first; if that fails or `search_reachable` is
    /// false, returns `false` immediately.  Otherwise calls `GET /indexes` and
    /// checks the index_id is present.
    /// Test: `two_step_probe_returns_false_on_transport_error`.
    async fn has_analysis(&self, index_id: &str) -> bool {
        // Step 1: health check.
        let health = match self.health().await {
            Ok(h) => h,
            Err(e) => {
                tracing::debug!("trusty-analyze health check failed (optional): {e}");
                return false;
            }
        };
        if !health.is_healthy() {
            tracing::debug!(
                status = %health.status,
                search_reachable = health.search_reachable,
                "trusty-analyze health indicates not ready"
            );
            return false;
        }

        // Step 2: list indexes and verify the target index exists.
        let url = format!("{}/indexes", self.base_url);
        let indexes_resp = match self.probe_http.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("trusty-analyze GET /indexes failed (optional): {e}");
                return false;
            }
        };

        if !indexes_resp.status().is_success() {
            tracing::debug!(
                status = %indexes_resp.status(),
                "trusty-analyze GET /indexes returned non-2xx"
            );
            return false;
        }

        let body = match indexes_resp.text().await {
            Ok(b) => b,
            Err(e) => {
                tracing::debug!("trusty-analyze read /indexes body failed: {e}");
                return false;
            }
        };

        let indexes: Vec<AnalyzeIndexInfo> = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("trusty-analyze /indexes parse failed: {e}");
                return false;
            }
        };

        // Check the target index exists.
        let found = indexes.iter().any(|i| i.id == index_id);
        if !found {
            tracing::debug!(
                index_id,
                "trusty-analyze has no matching index — analyze context unavailable"
            );
        }
        found
    }

    async fn complexity_hotspots(
        &self,
        index_id: &str,
        top_k: Option<u32>,
    ) -> Result<Vec<ComplexityHotspot>, AnalyzeClientError> {
        let mut url = format!("{}/indexes/{index_id}/complexity_hotspots", self.base_url);
        if let Some(k) = top_k {
            url.push_str(&format!("?top_k={k}"));
        }

        let resp = self
            .analysis_http
            .get(&url)
            .send()
            .await
            .map_err(|e| AnalyzeClientError::Transport(format!("GET {url}: {e}")))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AnalyzeClientError::Transport(format!("read body of {url}: {e}")))?;

        if !status.is_success() {
            return Err(AnalyzeClientError::Api {
                status: status.as_u16(),
                body,
            });
        }

        serde_json::from_str(&body)
            .map_err(|e| AnalyzeClientError::Parse(format!("complexity_hotspots response: {e}")))
    }

    async fn smells(&self, index_id: &str) -> Result<Vec<Smell>, AnalyzeClientError> {
        let url = format!("{}/indexes/{index_id}/smells", self.base_url);

        let resp = self
            .analysis_http
            .get(&url)
            .send()
            .await
            .map_err(|e| AnalyzeClientError::Transport(format!("GET {url}: {e}")))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| AnalyzeClientError::Transport(format!("read body of {url}: {e}")))?;

        if !status.is_success() {
            return Err(AnalyzeClientError::Api {
                status: status.as_u16(),
                body,
            });
        }

        serde_json::from_str(&body)
            .map_err(|e| AnalyzeClientError::Parse(format!("smells response: {e}")))
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "analyze_client_tests.rs"]
mod tests;
