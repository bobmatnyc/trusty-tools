//! The `AnalyzeClient` seam over trusty-analyze — OPTIONAL dependency.
//!
//! Why: trusty-analyze provides static analysis context (complexity hotspots,
//! code smells) that enriches the review.  It is OPTIONAL: if unavailable the
//! pipeline proceeds with empty static-analysis context and the service-
//! unavailable Slack notice is NOT raised.  (spec REV-012, REV-440, REV-442)
//!
//! What: defines the `AnalyzeClient` trait and its response types.
//!
//! #6287 (ADR-0032) removed `HttpAnalyzeClient`, the daemon-dialling
//! implementation this module was named for — see the note where it stood. The
//! two implementations left are `SubprocessAnalyzeClient` (spawns
//! `trusty-analyze review` per call, no daemon) and `NullAnalyzeClient`.
//!
//! Test: `analyze_error_display` and the response-type tests below; the
//! graceful-degradation contract is exercised through each implementation.

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

// ─── HTTP implementation: REMOVED (#6287) ────────────────────────────────────
//
// `HttpAnalyzeClient` lived here and dialled the trusty-analyze daemon over
// TCP loopback HTTP — `/health`, `/indexes`, `/indexes/{id}/complexity_hotspots`
// and `/indexes/{id}/smells`. ADR-0032 moved that daemon onto a Unix socket
// serving JSON-RPC, so none of those paths exist any more.
//
// It is DELETED rather than migrated because it had one caller left —
// `mcp::build_review_state` — and that caller now builds a
// `SubprocessAnalyzeClient`, which needs no daemon at all: it spawns
// `trusty-analyze review` per call. `webhook_drain` had already made the same
// swap. Porting a client nothing would construct would have been carrying a
// second transport for no consumer.
//
// The trait above and its response types stay: they are the seam
// `SubprocessAnalyzeClient`, `NullAnalyzeClient` and every pipeline test
// implement.

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "analyze_client_tests.rs"]
mod tests;
