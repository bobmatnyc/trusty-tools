//! HTTP client over trusty-search (`:7878`).
//!
//! Why: the review pipeline needs semantic / BM25 code search to retrieve
//! relevant context before generating a review.  trusty-search is the
//! REQUIRED dependency (spec REV-011, REV-431): if it is unreachable the
//! review must be skipped.  This module abstracts the transport behind a trait
//! so the pipeline is testable without a running daemon.
//! (spec REV-430, doc 01 REV-009)
//!
//! What: defines `SearchClient` trait (health check, list indexes, search)
//! and `HttpSearchClient`, an async HTTP implementation over
//! `TRUSTY_SEARCH_URL` (default `http://127.0.0.1:7878`).  All methods
//! return typed results; transport errors surface as `SearchClientError`
//! variants.
//!
//! `HealthResponse` and the tolerant `EmbedderState` deserialiser live in the
//! `health` submodule (see `health.rs`).
//!
//! Test: `search_client_base_url_construction` and
//! `http_search_client_url_is_configurable` verify URL building;
//! `search_result_deserialises` tests response parsing without a real daemon.

pub use super::health::{EmbedderState, HealthResponse};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ─── Error type ───────────────────────────────────────────────────────────────

/// Errors produced by `SearchClient` implementations.
///
/// Why: typed errors let the pipeline distinguish "service is down" (skip
/// review) from "bad request" (bug) from "empty result" (proceed with no
/// context).
/// What: `Transport` wraps reqwest failures; `Api` carries non-2xx responses;
/// `Parse` indicates unexpected JSON; `Unavailable` is the soft degradation
/// signal; `ClientInit` covers TLS-backend initialisation failures at
/// construction time so callers receive an `Err` instead of a panic.
/// Test: `search_error_display`.
#[derive(Debug, thiserror::Error)]
pub enum SearchClientError {
    /// HTTP transport failure (DNS, connect, TLS, timeout).
    #[error("trusty-search transport error: {0}")]
    Transport(String),

    /// trusty-search returned a non-2xx status.
    #[error("trusty-search API returned {status}: {body}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Response body text (may be truncated).
        body: String,
    },

    /// Could not parse the trusty-search response JSON.
    #[error("trusty-search response parse error: {0}")]
    Parse(String),

    /// trusty-search health check failed: service is unavailable.
    #[error("trusty-search is unavailable: {0}")]
    Unavailable(String),

    /// reqwest client construction failed (TLS backend unavailable).
    #[error("failed to build HTTP client: {0}")]
    ClientInit(String),
}

// ─── Response types ───────────────────────────────────────────────────────────

/// A single registered index from `GET /indexes?details=true`.
///
/// Why: the pipeline may need to verify the configured index exists before
/// issuing a search.
/// What: minimal shape — `id` and optional `root_path` are used by the
/// auto-derive resolver; other fields are ignored.
/// Test: `index_info_deserialises`, `list_indexes_parses_daemon_envelope`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IndexInfo {
    /// Unique index identifier.
    pub id: String,
    /// Optional human-readable name.
    #[serde(default)]
    pub name: Option<String>,
    /// Root path of the indexed directory (present only with `?details=true`).
    #[serde(default)]
    pub root_path: Option<String>,
}

/// Envelope wrapper for `GET /indexes?details=true`.
///
/// Why: the trusty-search daemon returns `{"indexes":[...]}`, not a bare array.
/// Deserialising directly as `Vec<IndexInfo>` fails with
/// `invalid type: map, expected a sequence`.  This wrapper absorbs the envelope
/// so callers receive a plain `Vec<IndexInfo>`.
/// What: single-field struct; `indexes` maps to the daemon's top-level key.
/// Test: `list_indexes_parses_daemon_envelope`.
#[derive(Debug, Deserialize)]
pub(crate) struct ListIndexesResponse {
    /// The list of registered indexes.
    pub(crate) indexes: Vec<IndexInfo>,
}

/// A single search result item returned by `POST /indexes/{id}/search`.
///
/// Why: the review pipeline uses the file path and snippet to build the
/// LLM context block.
/// What: `file` is the repo-relative path; `snippet` is a short code excerpt;
/// `score` is the combined BM25+vector relevance score.
/// Test: `search_result_deserialises`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SearchResult {
    /// Repository-relative file path.
    pub file: String,
    /// Short code snippet from the matching chunk.
    #[serde(default)]
    pub snippet: Option<String>,
    /// Combined relevance score.
    #[serde(default)]
    pub score: f32,
    /// Starting line number in the file (1-based).
    #[serde(default)]
    pub start_line: Option<u32>,
    /// Ending line number in the file (1-based).
    #[serde(default)]
    pub end_line: Option<u32>,
}

/// Request body for `POST /indexes/{id}/search`.
///
/// Why: the trusty-search search endpoint uses `SearchQuery` (defined in
/// `crates/trusty-search/src/core/indexer/mod.rs`), whose required field is
/// named `text` — not `query`.  Sending `query` causes a 422 "missing field
/// `text`" response, disabling context retrieval for every review.
/// What: minimal shape matching the trusty-search `SearchQuery` wire type.
/// The `text` field is required; `top_k` is optional (server default: 10).
/// Test: `search_request_body_uses_text_field`, `search_request_omits_none_top_k`.
#[derive(Debug, Clone, Serialize)]
pub struct SearchRequest {
    /// The search query string — MUST be named `text` to match trusty-search's
    /// `SearchQuery` struct (field `pub text: String`).  A `query` field is
    /// silently ignored and the server returns 422 for the missing `text`.
    pub text: String,
    /// Maximum number of results to return (server default: 10).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
}

/// Response from `POST /indexes/{id}/search`.
///
/// Why: trusty-search wraps results in a JSON envelope; we deserialise the
/// relevant fields and discard the rest.
/// What: `results` is the list of matched chunks; other fields are discarded.
/// Test: `search_response_deserialises`.
#[derive(Debug, Clone, Deserialize)]
pub struct SearchResponse {
    /// Matched results, ordered by descending relevance score.
    #[serde(default)]
    pub results: Vec<SearchResult>,
}

// ─── Trait definition ─────────────────────────────────────────────────────────

/// Client interface for the trusty-search HTTP daemon.
///
/// Why: the pipeline depends on this trait rather than `HttpSearchClient`
/// directly so the transport can be swapped (HTTP → in-process, or mock) without
/// touching pipeline code.  (spec REV-009)
/// What: exposes `health`, `list_indexes`, and `search` methods.  All methods
/// take `&self` and are `async`.
/// Test: `search_client_trait_object_compiles` verifies object safety.
#[async_trait]
pub trait SearchClient: Send + Sync {
    /// Check liveness of the trusty-search daemon.
    ///
    /// Why: the pipeline calls this before issuing a search to surface
    /// "service unavailable" rather than a cryptic connection error.
    /// What: `GET /health` → `HealthResponse`.  Returns
    /// `Err(SearchClientError::Unavailable)` on transport failure or non-2xx.
    /// Test: covered by integration tests; unit tests mock this method.
    async fn health(&self) -> Result<HealthResponse, SearchClientError>;

    /// List registered indexes.
    ///
    /// Why: the pipeline may need to verify the configured index exists and
    /// the auto-derive resolver needs `root_path` to match the current repo.
    /// What: `GET /indexes?details=true` → `Vec<IndexInfo>`.  The `?details=true`
    /// query is required so the daemon includes `root_path` in each entry.
    /// Gracefully degrades on transport error (returns `Err`; caller treats as
    /// daemon unreachable and falls back to `"main"`).
    /// Test: `list_indexes_parses_daemon_envelope`.
    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError>;

    /// Search within an index.
    ///
    /// Why: context retrieval is the core SearchClient use-case in the pipeline.
    /// What: `POST /indexes/{index_id}/search` with a `SearchRequest` body.
    /// Returns typed results; gracefully degrades on transport or empty-result.
    /// Test: `search_returns_empty_on_no_match`.
    async fn search(
        &self,
        index_id: &str,
        query: &str,
        top_k: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError>;
}

// ─── HTTP implementation ──────────────────────────────────────────────────────

/// HTTP implementation of `SearchClient` over a running trusty-search daemon.
///
/// Why: the default transport for all production and staging deployments.
/// What: targets `TRUSTY_SEARCH_URL` (default `http://127.0.0.1:7878`) and
/// calls the live trusty-search REST API.  Transport errors are mapped to
/// `SearchClientError` variants so the pipeline can degrade gracefully.
/// Test: `http_search_client_url_is_configurable`.
pub struct HttpSearchClient {
    /// Base URL of the trusty-search daemon (no trailing slash).
    base_url: String,
    /// Underlying reqwest client.
    http: reqwest::Client,
}

impl HttpSearchClient {
    /// Construct from an explicit base URL.
    ///
    /// Why: allows tests and library consumers to point the client at any URL
    /// without going through the config system.
    /// What: strips any trailing slash from `base_url` to avoid double-slash
    /// path construction.  Returns `Err(ClientInit)` if the TLS backend cannot
    /// be initialised — surfaces the failure to the caller rather than panicking
    /// at daemon startup (closes #953).
    /// Test: `http_search_client_url_is_configurable`.
    pub fn new(base_url: impl Into<String>) -> Result<Self, SearchClientError> {
        let raw = base_url.into();
        let base_url = raw.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| SearchClientError::ClientInit(e.to_string()))?;
        Ok(Self { base_url, http })
    }

    /// Construct from a `ReviewConfig`, reading `search_url`.
    ///
    /// Why: the pipeline constructs the client from its injected config rather
    /// than reading env vars.
    /// What: calls `Self::new(config.search_url.clone())` and propagates any
    /// TLS-backend init failure as `Err`.
    /// Test: `http_search_client_from_config`.
    pub fn from_config(config: &crate::config::ReviewConfig) -> Result<Self, SearchClientError> {
        Self::new(config.search_url.clone())
    }

    /// Return the base URL this client targets.
    ///
    /// Why: tests need to assert the URL is constructed correctly.
    /// What: returns a reference to the stored base URL string.
    /// Test: `http_search_client_url_is_configurable`.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[async_trait]
impl SearchClient for HttpSearchClient {
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        let url = format!("{}/health", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| SearchClientError::Unavailable(format!("GET {url}: {e}")))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| SearchClientError::Transport(format!("read body of {url}: {e}")))?;

        if !status.is_success() {
            return Err(SearchClientError::Unavailable(format!(
                "GET {url} returned {status}: {body}"
            )));
        }

        serde_json::from_str(&body)
            .map_err(|e| SearchClientError::Parse(format!("health response: {e}")))
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        // `?details=true` is REQUIRED: without it the daemon omits `root_path`
        // from each index entry, making auto-derive unable to match any index.
        let url = format!("{}/indexes?details=true", self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| SearchClientError::Transport(format!("GET {url}: {e}")))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| SearchClientError::Transport(format!("read body of {url}: {e}")))?;

        if !status.is_success() {
            return Err(SearchClientError::Api {
                status: status.as_u16(),
                body,
            });
        }

        // The daemon returns `{"indexes":[...]}`, not a bare array.
        // Unwrap the envelope and return the inner Vec.
        let envelope: ListIndexesResponse = serde_json::from_str(&body)
            .map_err(|e| SearchClientError::Parse(format!("list indexes response: {e}")))?;
        Ok(envelope.indexes)
    }

    async fn search(
        &self,
        index_id: &str,
        query: &str,
        top_k: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        let url = format!("{}/indexes/{index_id}/search", self.base_url);
        let request_body = SearchRequest {
            text: query.to_string(),
            top_k,
        };

        let resp = self
            .http
            .post(&url)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| SearchClientError::Transport(format!("POST {url}: {e}")))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| SearchClientError::Transport(format!("read body of {url}: {e}")))?;

        if !status.is_success() {
            return Err(SearchClientError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let search_resp: SearchResponse = serde_json::from_str(&body)
            .map_err(|e| SearchClientError::Parse(format!("search response: {e}")))?;

        Ok(search_resp.results)
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Split into a sibling file to keep this file under the 500-line cap (#610).

#[cfg(test)]
#[path = "search_client_tests.rs"]
mod tests;
