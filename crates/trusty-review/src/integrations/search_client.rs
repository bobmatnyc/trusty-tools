//! Client over trusty-search — its Unix socket or, until #9214 phase C, HTTP.
//!
//! Why: the review pipeline needs semantic / BM25 code search to retrieve
//! relevant context before generating a review.  trusty-search is the
//! REQUIRED dependency (spec REV-011, REV-431): if it is unreachable the
//! review must be skipped.  This module abstracts the transport behind a trait
//! so the pipeline is testable without a running daemon.
//! (spec REV-430, doc 01 REV-009)
//!
//! What: defines `SearchClient` trait (health check, list indexes, search)
//! and `HttpSearchClient`, which reaches the daemon over the transport
//! [`super::search_transport::SearchTransport`] resolves (#9214): the socket
//! when one is present, HTTP otherwise.  All methods return typed results;
//! transport errors surface as `SearchClientError` variants on either leg.
//!
//! `HealthResponse` and the tolerant `EmbedderState` deserialiser live in the
//! `health` submodule (see `health.rs`).
//!
//! Test: `search_client_base_url_construction` and
//! `http_search_client_url_is_configurable` verify URL building;
//! `search_result_deserialises` tests response parsing without a real daemon.

pub use super::health::{EmbedderState, HealthResponse};
pub use super::index_status::{
    CorpusOpenFailure, IndexStageReport, IndexStagesReport, IndexStatusResponse,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use trusty_common::search_rpc::{METHOD_HEALTH, METHOD_INDEX_STATUS, METHOD_INDEXES_LIST};

use super::search_transport::{
    METHOD_CALL_CHAIN, METHOD_QUERY, SearchTransport, call_socket, decode,
};
use crate::pipeline::optional_context::probes::redact_credentials; // #9431

/// Whole-request bound for one call, on either leg.
const SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
/// #9431: `Transport` and `Unavailable` name the request URL (and reqwest's
/// error names it again), so their Display masks its credentials; every
/// `to_string`, log line and review field built from them inherits that.
/// Test: `search_error_display`, `credential_url_never_reaches_result_error`.
#[derive(Debug, thiserror::Error)]
pub enum SearchClientError {
    /// HTTP transport failure (DNS, connect, TLS, timeout).
    #[error("trusty-search transport error: {}", redact_credentials(.0))]
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
    #[error("trusty-search is unavailable: {}", redact_credentials(.0))]
    Unavailable(String),

    /// reqwest client construction failed (TLS backend unavailable).
    #[error("failed to build HTTP client: {0}")]
    ClientInit(String),
}

impl SearchClientError {
    /// Whether this error says the index does not exist on the daemon (#6687).
    ///
    /// Why: an unknown index and a failed query are not the same outcome, and
    /// collapsing both into an empty result set is what let a review run with no
    /// code context and still publish a verdict. trusty-search answers a query
    /// naming an index it has never heard of with `404 {"error":"unknown index:
    /// …"}`; a registered-but-not-resident index answers `503` and a genuine
    /// query fault answers `5xx`, so the status code is the discriminator.
    /// What: `true` only for `Api { status: 404, .. }`.
    /// Test: `unknown_index_error_is_only_a_404`.
    pub fn is_unknown_index(&self) -> bool {
        matches!(self, SearchClientError::Api { status: 404, .. })
    }
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

/// A registered index plus the repository trusty-search recorded for it (#8649).
///
/// Why: kept apart from [`IndexInfo`] so adding the field breaks no struct
/// literal in a downstream crate.
/// What: `repo_identity` is the daemon's canonical `owner/repo` (or
/// `content:<sha>`); `None` when the daemon did not report one.
/// `last_used_unix` is the later of the index's last query and last index,
/// the tiebreak among several indexes of one repo.
/// Test: `list_index_identities_parses_repo_identity`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IndexIdentity {
    /// Unique index identifier.
    pub id: String,
    /// Root path of the indexed directory.
    #[serde(default)]
    pub root_path: Option<String>,
    /// Canonical repository identity, when the daemon recorded one.
    #[serde(default)]
    pub repo_identity: Option<String>,
    /// Unix seconds of the index's last use; `None` when never used.
    #[serde(default)]
    pub last_used_unix: Option<u64>,
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
pub(crate) struct ListIndexesResponse<T = IndexInfo> {
    /// The list of registered indexes.
    pub(crate) indexes: Vec<T>,
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

    /// List registered indexes with the repository each one belongs to (#8649).
    ///
    /// Why: `review_pr` names a repo, not a directory; trusty-search records
    /// each index's canonical `owner/repo` (`repo_identity`, DOC-37), which is
    /// the only registry-side mapping from a repo to its index.
    /// What: `repo_identity = Some(r)` asks the daemon for only the indexes
    /// recorded for `r`; `None` lists every index. The default body fails with
    /// [`SearchClientError::Unavailable`]: a client that cannot report
    /// identities must not look like one whose indexes have none, or a
    /// same-named index of another repo could be picked by name.
    /// `HttpSearchClient` overrides it.
    /// Test: `list_index_identities_parses_repo_identity`,
    /// `default_list_index_identities_fails_closed`.
    async fn list_index_identities(
        &self,
        repo_identity: Option<&str>,
    ) -> Result<Vec<IndexIdentity>, SearchClientError> {
        let _ = repo_identity;
        Err(SearchClientError::Unavailable(
            "repo identities not supported by this search client".to_string(),
        ))
    }

    /// Read the status of ONE index (#6686).
    ///
    /// Why: `/health` counts registry handles and discards index ids, so the
    /// gate that branched on it degraded every review on the host whenever any
    /// index anywhere had failed. The question a review needs answered is
    /// whether the index IT queries can return results, and this is the only
    /// endpoint that answers it. `health` keeps exactly one job: is the daemon
    /// reachable and serving.
    /// What: `GET /indexes/{index_id}/status` → [`IndexStatusResponse`]. An
    /// index the daemon has never heard of answers `404`, which surfaces as
    /// `SearchClientError::Api { status: 404, .. }` — see
    /// [`SearchClientError::is_unknown_index`], the discriminator the gate skips
    /// on (#6687). A registered-but-not-resident index answers `503` and is a
    /// degradation, not a skip.
    /// Test: `index_status_parses_a_live_payload`,
    /// `index_status_unknown_index_is_a_404_api_error`; the gate-level
    /// behaviour is covered by `context_gate_tests`.
    async fn index_status(&self, index_id: &str) -> Result<IndexStatusResponse, SearchClientError>;

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

    /// Read the call-graph report for one entry point (#9196).
    ///
    /// Why: B5 shows each changed symbol's callers, callees and tests from the
    /// trusty-search call graph. Architect ruling Q6: a default-bodied method,
    /// so every existing implementor keeps compiling.
    /// What: the daemon's plain-text report for `entry_point`, walked in
    /// `direction` (`both`, `outgoing` or `callers`) to `max_depth`, with
    /// function bodies only when `include_source`. The default body fails
    /// with [`SearchClientError::Unavailable`]: a client that cannot read the
    /// graph must not look like one whose symbols have no edges (precedent
    /// `list_index_identities`, #8649). `HttpSearchClient` overrides it.
    /// Test: `call_chain_goes_over_the_socket_with_its_params`,
    /// `call_chain_over_http_returns_the_text_body`,
    /// `default_call_chain_is_unavailable`.
    async fn call_chain(
        &self,
        index_id: &str,
        entry_point: &str,
        direction: &str,
        max_depth: u32,
        include_source: bool,
    ) -> Result<String, SearchClientError> {
        let _ = (index_id, entry_point, direction, max_depth, include_source);
        Err(SearchClientError::Unavailable(
            "call chains not supported by this search client".to_string(),
        ))
    }
}

// ─── HTTP implementation ──────────────────────────────────────────────────────

/// `SearchClient` over a running trusty-search daemon.
///
/// Why: the default client for all production and staging deployments. The
/// name predates #9214; it now speaks the socket too, and keeps the name until
/// phase C so no caller breaks.
/// What: each method calls the socket method when `transport` is the socket
/// leg and the REST route when it is HTTP. Both legs map failures onto the
/// same `SearchClientError` values so the pipeline degrades identically.
/// Test: `http_search_client_url_is_configurable`, `socket_is_used_when_present`.
pub struct HttpSearchClient {
    /// HTTP base URL (no trailing slash); empty on the socket leg.
    base_url: String, // #9214 phase C: delete
    /// Underlying reqwest client.
    http: reqwest::Client, // #9214 phase C: delete
    /// #9214: which leg every call takes.
    transport: SearchTransport,
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
        // #9214: an explicit URL is the HTTP leg, exactly as before.
        Self::with_transport(SearchTransport::Http(raw.trim_end_matches('/').to_string()))
    }

    /// Construct over an already-resolved transport (#9214).
    ///
    /// Why: the report pass and the tests pick the leg themselves; this is the
    /// additive, transport-aware twin of [`Self::new`].
    /// What: stores `transport`; the HTTP client is built either way so the
    /// HTTP leg stays usable until phase C. `Err(ClientInit)` on a TLS failure.
    /// Test: `rpc_32004_maps_to_the_http_404_error`.
    pub fn with_transport(transport: SearchTransport) -> Result<Self, SearchClientError> {
        let base_url = match &transport {
            SearchTransport::Http(url) => url.clone(), // #9214 phase C: delete
            SearchTransport::Socket(_) => String::new(),
        };
        let http = reqwest::Client::builder() // #9214 phase C: delete
            .timeout(SEARCH_TIMEOUT)
            .build()
            .map_err(|e| SearchClientError::ClientInit(e.to_string()))?;
        Ok(Self {
            base_url,
            http,
            transport,
        })
    }

    /// Construct from a `ReviewConfig`, reading `search_url`.
    ///
    /// Why: the pipeline constructs the client from its injected config rather
    /// than reading env vars.
    /// What: #9214 — resolves the transport from `config` (socket first, see
    /// [`SearchTransport::resolve`]) and propagates any TLS-backend init
    /// failure as `Err`.
    /// Test: `http_search_client_from_config`, `socket_is_used_when_present`.
    pub fn from_config(config: &crate::config::ReviewConfig) -> Result<Self, SearchClientError> {
        Self::with_transport(SearchTransport::resolve(config))
    }

    /// The leg this client calls (#9214).
    pub fn transport(&self) -> &SearchTransport {
        &self.transport
    }

    /// Return the HTTP base URL this client targets.
    ///
    /// Why: tests need to assert the URL is constructed correctly.
    /// What: returns the stored base URL; empty on the socket leg (#9214).
    /// Test: `http_search_client_url_is_configurable`.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `GET /indexes?details=true`, unwrapped from its `{"indexes":[...]}`
    /// envelope into whichever entry shape the caller reads.
    ///
    /// `?details=true` is REQUIRED: without it the daemon omits `root_path` and
    /// `repo_identity` from each entry. #8649 made this generic so
    /// `list_indexes` and `list_index_identities` share one request; an
    /// optional `?repo_identity=` filter is applied by the daemon before its
    /// per-index disk walk.
    async fn get_index_list<T: serde::de::DeserializeOwned>(
        &self,
        repo_identity: Option<&str>,
    ) -> Result<Vec<T>, SearchClientError> {
        // #9214: `search.indexes.list` takes the same fields as the query string.
        if let Some(socket) = self.transport.socket_path() {
            let mut params = serde_json::json!({ "details": true });
            if let Some(identity) = repo_identity {
                params["repo_identity"] = identity.into();
            }
            let value = call_socket(socket, METHOD_INDEXES_LIST, params, SEARCH_TIMEOUT).await?;
            let envelope: ListIndexesResponse<T> = decode(value, "list indexes response")?;
            return Ok(envelope.indexes);
        }
        // #9214 phase C: delete — the HTTP leg, from here to the end of the fn.
        let url = format!("{}/indexes", self.base_url);
        let mut query = vec![("details", "true")];
        if let Some(identity) = repo_identity {
            query.push(("repo_identity", identity));
        }
        let resp = self
            .http
            .get(&url)
            .query(&query)
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

        let envelope: ListIndexesResponse<T> = serde_json::from_str(&body)
            .map_err(|e| SearchClientError::Parse(format!("list indexes response: {e}")))?;
        Ok(envelope.indexes)
    }
}

#[async_trait]
impl SearchClient for HttpSearchClient {
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        // #9214: any socket failure is "unavailable", as any HTTP failure is.
        if let Some(socket) = self.transport.socket_path() {
            let value = call_socket(socket, METHOD_HEALTH, serde_json::json!({}), SEARCH_TIMEOUT)
                .await
                .map_err(|e| {
                    SearchClientError::Unavailable(format!("{}: {e}", self.transport.describe()))
                })?;
            return decode(value, "health response");
        }
        // #9214 phase C: delete — the HTTP leg, from here to the end of the fn.
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
        self.get_index_list(None).await
    }

    // #8649: same request, read with each entry's `repo_identity` kept.
    async fn list_index_identities(
        &self,
        repo_identity: Option<&str>,
    ) -> Result<Vec<IndexIdentity>, SearchClientError> {
        self.get_index_list(repo_identity).await
    }

    // #6686: the per-index probe the required-context gate decides on.
    async fn index_status(&self, index_id: &str) -> Result<IndexStatusResponse, SearchClientError> {
        // #9214: -32004 arrives as `Api{404}`, so `is_unknown_index` holds.
        if let Some(socket) = self.transport.socket_path() {
            let params = serde_json::json!({ "index_id": index_id });
            let value = call_socket(socket, METHOD_INDEX_STATUS, params, SEARCH_TIMEOUT).await?;
            let parsed: IndexStatusResponse = decode(value, "index status response")?;
            return Ok(with_index_id(parsed, index_id));
        }
        // #9214 phase C: delete — the HTTP leg, from here to the end of the fn.
        let url = format!("{}/indexes/{index_id}/status", self.base_url);
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
            // The 404 here is load-bearing for #6687: it is what tells the gate
            // the configured index does not exist, rather than that the query
            // found nothing.
            return Err(SearchClientError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: IndexStatusResponse = serde_json::from_str(&body)
            .map_err(|e| SearchClientError::Parse(format!("index status response: {e}")))?;
        Ok(with_index_id(parsed, index_id))
    }

    async fn search(
        &self,
        index_id: &str,
        query: &str,
        top_k: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        let request_body = SearchRequest {
            text: query.to_string(),
            top_k,
        };
        // #9214: `search.query` nests the HTTP body under `body`.
        if let Some(socket) = self.transport.socket_path() {
            let params = serde_json::json!({ "index_id": index_id, "body": request_body });
            let value = call_socket(socket, METHOD_QUERY, params, SEARCH_TIMEOUT).await?;
            let search_resp: SearchResponse = decode(value, "search response")?;
            return Ok(search_resp.results);
        }
        // #9214 phase C: delete — the HTTP leg, from here to the end of the fn.
        let url = format!("{}/indexes/{index_id}/search", self.base_url);

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

    // #9196: the call-graph report, plain text on both legs.
    async fn call_chain(
        &self,
        index_id: &str,
        entry_point: &str,
        direction: &str,
        max_depth: u32,
        include_source: bool,
    ) -> Result<String, SearchClientError> {
        // #9214: `search.call_chain` answers the report as a bare string.
        if let Some(socket) = self.transport.socket_path() {
            let params = serde_json::json!({
                "index_id": index_id,
                "entry_point": entry_point,
                "direction": direction,
                "max_depth": max_depth,
                "include_source": include_source,
            });
            let value = call_socket(socket, METHOD_CALL_CHAIN, params, SEARCH_TIMEOUT).await?;
            return match value {
                serde_json::Value::String(text) => Ok(text),
                _ => Err(SearchClientError::Parse(
                    "non-string call_chain result".to_string(),
                )),
            };
        }
        // #9214 phase C: delete — the HTTP leg, from here to the end of the fn.
        let url = format!("{}/indexes/{index_id}/call_chain", self.base_url);
        let depth = max_depth.to_string();
        let source = include_source.to_string();
        let resp = self
            .http
            .get(&url)
            .query(&[
                ("entry_point", entry_point),
                ("direction", direction),
                ("max_depth", depth.as_str()),
                ("include_source", source.as_str()),
            ])
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
        Ok(body)
    }
}

/// Fill an index status's `index_id` when the daemon omitted it.
///
/// Older daemons may omit `index_id`; the reason string must still name the
/// index the review asked about.
fn with_index_id(mut parsed: IndexStatusResponse, index_id: &str) -> IndexStatusResponse {
    if parsed.index_id.is_empty() {
        parsed.index_id = index_id.to_string();
    }
    parsed
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Split into a sibling file to keep this file under the 500-line cap (#610).

#[cfg(test)]
#[path = "search_client_tests.rs"]
mod tests;
