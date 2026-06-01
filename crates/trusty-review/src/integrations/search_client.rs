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
//! Test: `search_client_base_url_construction` and
//! `http_search_client_url_is_configurable` verify URL building;
//! `search_result_deserialises` tests response parsing without a real daemon.

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
/// signal.
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
}

// ─── Response types ───────────────────────────────────────────────────────────

/// Response from `GET /health` on trusty-search.
///
/// Why: the pipeline checks health before issuing a search to give a clear
/// "service unavailable" error rather than a confusing transport failure.
/// What: `status` is `"ok"` when healthy; `embedder` is true when the
/// embedding model is loaded and ready.
/// Test: `health_response_deserialises`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HealthResponse {
    /// `"ok"` when healthy.
    pub status: String,
    /// True when the embedding model is loaded and ready.
    #[serde(default)]
    pub embedder: bool,
}

impl HealthResponse {
    /// Returns `true` when the daemon is healthy and the embedder is loaded.
    ///
    /// Why: the pipeline needs a single boolean to decide whether to proceed.
    /// What: checks `status == "ok"`.  The `embedder` flag is informational.
    /// Test: `health_response_is_healthy`.
    pub fn is_healthy(&self) -> bool {
        self.status == "ok"
    }
}

/// A single registered index from `GET /indexes`.
///
/// Why: the pipeline may need to verify the configured index exists before
/// issuing a search.
/// What: minimal shape — only `id` and optional `name` are needed for the
/// MVP; other fields are ignored.
/// Test: `index_info_deserialises`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IndexInfo {
    /// Unique index identifier.
    pub id: String,
    /// Optional human-readable name.
    #[serde(default)]
    pub name: Option<String>,
    /// Root path of the indexed directory.
    #[serde(default)]
    pub root_path: Option<String>,
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
/// Why: the trusty-search search endpoint accepts a JSON body with the query
/// and optional parameters.
/// What: minimal shape matching the trusty-search API (verified in
/// `crates/trusty-search/src/service/server.rs`).
/// Test: `search_request_serialises`.
#[derive(Debug, Clone, Serialize)]
pub struct SearchRequest {
    /// The search query string.
    pub query: String,
    /// Maximum number of results to return (default: 20).
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
    /// Why: the pipeline may need to verify the configured index exists.
    /// What: `GET /indexes` → `Vec<IndexInfo>`.  Gracefully degrades on
    /// transport error (returns `Err`; caller may treat as empty).
    /// Test: `list_indexes_deserialises`.
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
    /// path construction.
    /// Test: `http_search_client_url_is_configurable`.
    pub fn new(base_url: impl Into<String>) -> Self {
        let raw = base_url.into();
        let base_url = raw.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest::Client::build failed");
        Self { base_url, http }
    }

    /// Construct from a `ReviewConfig`, reading `search_url`.
    ///
    /// Why: the pipeline constructs the client from its injected config rather
    /// than reading env vars.
    /// What: calls `Self::new(config.search_url.clone())`.
    /// Test: `http_search_client_from_config`.
    pub fn from_config(config: &crate::config::ReviewConfig) -> Self {
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
        let url = format!("{}/indexes", self.base_url);
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

        serde_json::from_str(&body)
            .map_err(|e| SearchClientError::Parse(format!("list indexes response: {e}")))
    }

    async fn search(
        &self,
        index_id: &str,
        query: &str,
        top_k: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        let url = format!("{}/indexes/{index_id}/search", self.base_url);
        let request_body = SearchRequest {
            query: query.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_client_trait_object_compiles() {
        // This test just needs to compile; the coercion proves SearchClient is
        // object-safe.
        fn _accepts_dyn(_c: &dyn SearchClient) {}
    }

    #[test]
    fn http_search_client_url_is_configurable() {
        let client = HttpSearchClient::new("http://127.0.0.1:7878");
        assert_eq!(client.base_url(), "http://127.0.0.1:7878");
    }

    #[test]
    fn http_search_client_strips_trailing_slash() {
        let client = HttpSearchClient::new("http://127.0.0.1:7878/");
        // Trailing slash must be removed to prevent double-slash paths.
        assert_eq!(client.base_url(), "http://127.0.0.1:7878");
    }

    #[test]
    fn http_search_client_from_config() {
        let mut config = crate::config::ReviewConfig::load(None);
        config.search_url = "http://localhost:9999".to_string();
        let client = HttpSearchClient::from_config(&config);
        assert_eq!(client.base_url(), "http://localhost:9999");
    }

    #[test]
    fn health_response_is_healthy() {
        let resp = HealthResponse {
            status: "ok".to_string(),
            embedder: true,
        };
        assert!(resp.is_healthy());
    }

    #[test]
    fn health_response_not_ok_is_unhealthy() {
        let resp = HealthResponse {
            status: "starting".to_string(),
            embedder: false,
        };
        assert!(!resp.is_healthy());
    }

    #[test]
    fn health_response_deserialises() {
        let json = r#"{"status":"ok","embedder":true}"#;
        let resp: HealthResponse = serde_json::from_str(json).unwrap();
        assert!(resp.is_healthy());
        assert!(resp.embedder);
    }

    #[test]
    fn index_info_deserialises() {
        let json = r#"{"id":"main","name":"trusty-tools","root_path":"/home/user/trusty-tools"}"#;
        let info: IndexInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.id, "main");
        assert_eq!(info.name.as_deref(), Some("trusty-tools"));
    }

    #[test]
    fn search_result_deserialises() {
        let json = r#"{
            "file": "src/lib.rs",
            "snippet": "pub fn authenticate() {",
            "score": 0.92,
            "start_line": 42,
            "end_line": 58
        }"#;
        let result: SearchResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.file, "src/lib.rs");
        assert_eq!(result.snippet.as_deref(), Some("pub fn authenticate() {"));
        assert!((result.score - 0.92_f32).abs() < 1e-5);
        assert_eq!(result.start_line, Some(42));
        assert_eq!(result.end_line, Some(58));
    }

    #[test]
    fn search_result_missing_optional_fields() {
        let json = r#"{"file":"src/main.rs"}"#;
        let result: SearchResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.file, "src/main.rs");
        assert!(result.snippet.is_none());
        assert!((result.score - 0.0_f32).abs() < 1e-10);
    }

    #[test]
    fn search_request_serialises() {
        let req = SearchRequest {
            query: "fn authenticate".to_string(),
            top_k: Some(10),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("fn authenticate"));
        assert!(json.contains("10"));
    }

    #[test]
    fn search_request_omits_none_top_k() {
        let req = SearchRequest {
            query: "async fn".to_string(),
            top_k: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("top_k"));
    }

    #[test]
    fn search_response_deserialises() {
        let json = r#"{"results":[{"file":"a.rs","score":0.5},{"file":"b.rs","score":0.3}]}"#;
        let resp: SearchResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.results.len(), 2);
        assert_eq!(resp.results[0].file, "a.rs");
    }

    #[test]
    fn search_error_display() {
        let err = SearchClientError::Transport("connection refused".to_string());
        assert!(err.to_string().contains("connection refused"));

        let err = SearchClientError::Api {
            status: 503,
            body: "overloaded".to_string(),
        };
        let s = err.to_string();
        assert!(s.contains("503"));
        assert!(s.contains("overloaded"));
    }

    #[tokio::test]
    async fn health_check_transport_error_on_unreachable() {
        // Port 1 is always refused; this verifies graceful transport error handling.
        let client = HttpSearchClient::new("http://127.0.0.1:1");
        let result = client.health().await;
        assert!(
            result.is_err(),
            "unreachable host must return an error, not panic"
        );
        match result.unwrap_err() {
            SearchClientError::Unavailable(_) => {}
            SearchClientError::Transport(_) => {}
            other => panic!("expected Unavailable or Transport, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn search_transport_error_on_unreachable() {
        let client = HttpSearchClient::new("http://127.0.0.1:1");
        let result = client.search("main", "fn auth", Some(5)).await;
        assert!(
            result.is_err(),
            "unreachable host must return an error, not panic"
        );
    }
}
