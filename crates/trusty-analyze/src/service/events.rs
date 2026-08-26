//! Shared daemon state and the uniform handler error type.
//!
//! Why: Extracted from `service/mod.rs` to keep each module focused. This
//! module owns what every handler receives — the shared stores and clients —
//! separated from the handler logic that acts on it.
//!
//! What: defines [`AnalyzerAppState`] (cloneable shared state threaded through
//! every RPC handler), [`ApiError`] (the uniform handler error), and
//! [`fetch_chunks`] (the one way a handler reaches the search corpus).
//!
//! #6287 removed `AnalyzerEvent`, the `events` broadcast channel and
//! `DEFAULT_PORT` along with the axum surface that carried them. `/sse` was the
//! only subscriber the enum ever had, and ADR-0032 leaves this daemon with no
//! HTTP surface to stream from; a push channel with no transport is state
//! nothing can observe. The dashboard mounts on `trusty-console` instead.
//!
//! Test: `service::rpc_tests` drives every handler over a real socket.

use std::sync::Arc;

use crate::core::{AnalyzerRegistry, FactStore, ScipOverlayStore, TrustySearchClient};
use crate::embedder::{BowEmbedder, Embedder};
use crate::types::SmellThresholds;
use trusty_common::uds::server::{RpcError, CODE_INTERNAL_ERROR, CODE_INVALID_PARAMS};

/// Shared state for every handler. Cheap to clone (everything is `Arc`-ish).
#[derive(Clone)]
pub struct AnalyzerAppState {
    pub search: TrustySearchClient,
    pub facts: FactStore,
    pub registry: Arc<AnalyzerRegistry>,
    /// Embedder used by `/indexes/{id}/clusters`.
    ///
    /// Why (#5067): this used to be swapped out at boot by `run_serve` for a
    /// fastembed model, and that swap is what stalled startup. It is now always
    /// the `BowEmbedder` that `new` installs — construction is pure, so no
    /// caller can make this field cost a network round trip.
    /// Test: `analyze_declares_no_in_process_model_deps`.
    pub embedder: Arc<dyn Embedder>,
    /// Per-index SCIP-derived knowledge graph overlay, populated by
    /// `POST /indexes/{id}/scip`. Merged into the response of
    /// `GET /indexes/{id}/graph` so consumers see the union of tree-sitter
    /// extraction and any precise SCIP indexes the user has uploaded.
    ///
    /// Why: this was an `Arc<RwLock<HashMap<String, KgGraph>>>` until #5049 —
    /// pure process memory, so a restart discarded an ingest that had already
    /// returned HTTP 200 and `/graph` then served a tree-sitter-only graph
    /// with no way to tell it apart. A SCIP index is uploaded by the operator
    /// and is not re-derivable from the corpus, so it must be on disk.
    /// What: redb-backed store keyed by index id. `get` returns `Option`,
    /// which is how `GET /indexes/{id}/scip` distinguishes "nobody ingested"
    /// (404) from "ingested, zero symbols" (200 with `nodes: 0`).
    /// Test: `scip_overlay_survives_state_rebuild`,
    /// `scip_overlay_status_404_when_never_ingested`.
    pub scip_overlays: ScipOverlayStore,
    /// Runtime-configurable thresholds for code-smell detection.
    ///
    /// Why: satisfies the README's "configurable thresholds" promise — operators
    /// can override the defaults at daemon startup (e.g. via CLI flags or a
    /// config file) without recompilation. All handlers that run smell detection
    /// read thresholds from here so a single override site controls all paths.
    /// What: `SmellThresholds` with defaults matching the former compile-time
    /// constants (50 lines / depth 4 / 5 params). Set via `with_smell_thresholds`.
    /// Test: `custom_threshold_lowers_long_function_trigger` in
    /// `core::complexity` proves that a non-default threshold fires on a chunk
    /// the defaults would ignore.
    pub smell_thresholds: SmellThresholds,
    /// OpenRouter API key used by the `POST /analyze/deep` endpoint.
    ///
    /// Why: the deep-analysis endpoint needs an LLM provider to generate the
    /// narrative; threading the key through state lets the binary read it
    /// once at startup and keeps tests hermetic (no live env reads in handlers).
    /// What: `Some(key)` enables LLM narrative; `None` causes `/analyze/deep`
    /// to return 400 `MissingApiKey` so the caller knows configuration is
    /// required.
    /// Test: covered by `deep_endpoint_requires_api_key`.
    pub api_key: Option<String>,
    /// Default LLM model identifier used for `POST /analyze/deep` calls when
    /// the request body does not override `model`.
    ///
    /// Why: model selection is deployment-specific; reading it once at
    /// startup avoids re-parsing env vars per request and lets ops switch
    /// models without touching code.
    /// What: defaults to `openai/gpt-4o-mini` when not configured.
    /// Test: covered transitively by `AnalyzerAppState::new`.
    pub llm_model: String,
}

impl AnalyzerAppState {
    /// Construct with the default registry and a BOW embedder. Use this when
    /// neural embeddings aren't required (tests, BOW-only deployments).
    ///
    /// Why (#5049): `scip_overlays` is a required constructor argument rather
    /// than a `with_*` override precisely so no caller can end up with a
    /// non-durable overlay store by omission — that omission was the bug.
    /// What: builds state around the supplied search client, fact store, and
    /// overlay store.
    /// Test: covered by every `service::tests` case via `make_state`.
    pub fn new(
        search: TrustySearchClient,
        facts: FactStore,
        scip_overlays: ScipOverlayStore,
    ) -> Self {
        Self {
            search,
            facts,
            registry: Arc::new(AnalyzerRegistry::default_registry()),
            embedder: Arc::new(BowEmbedder::default()),
            scip_overlays,
            smell_thresholds: SmellThresholds::default(),
            api_key: std::env::var(trusty_common::env_vars::ENV_OPENROUTER_API_KEY).ok(),
            llm_model: std::env::var("TRUSTY_LLM_MODEL")
                .unwrap_or_else(|_| "openai/gpt-4o-mini".to_string()),
        }
    }

    /// Construct with an explicit registry (useful for tests and plug-ins).
    /// Embedder defaults to BOW.
    pub fn with_registry(
        search: TrustySearchClient,
        facts: FactStore,
        registry: Arc<AnalyzerRegistry>,
        scip_overlays: ScipOverlayStore,
    ) -> Self {
        Self {
            search,
            facts,
            registry,
            embedder: Arc::new(BowEmbedder::default()),
            scip_overlays,
            smell_thresholds: SmellThresholds::default(),
            api_key: std::env::var(trusty_common::env_vars::ENV_OPENROUTER_API_KEY).ok(),
            llm_model: std::env::var("TRUSTY_LLM_MODEL")
                .unwrap_or_else(|_| "openai/gpt-4o-mini".to_string()),
        }
    }

    /// Override the OpenRouter API key on an existing state.
    ///
    /// Why: lets the binary pass an explicit key in at startup (or tests
    /// inject `None` deterministically) instead of relying on the
    /// environment at every handler call.
    /// What: replaces `api_key`; returns `self` for chaining.
    /// Test: covered by `deep_endpoint_requires_api_key`.
    pub fn with_api_key(mut self, key: Option<String>) -> Self {
        self.api_key = key;
        self
    }

    /// Override smell-detection thresholds for the lifetime of this daemon.
    ///
    /// Why: lets operators tune thresholds at startup (e.g. from CLI flags or a
    /// config file) without recompilation, fulfilling the README's "configurable
    /// thresholds" guarantee. Handlers read `state.smell_thresholds` rather than
    /// calling `detect_smells` (which uses `SmellThresholds::default()`), so this
    /// single override point controls all smell-detection paths.
    /// What: replaces `smell_thresholds`; returns `self` for chaining.
    /// Test: integration-level — set a low threshold on `AnalyzerAppState`, post
    /// a chunk via the HTTP API, assert the smell appears in the response.
    pub fn with_smell_thresholds(mut self, thresholds: SmellThresholds) -> Self {
        self.smell_thresholds = thresholds;
        self
    }

    /// Override the LLM model identifier.
    ///
    /// Why: callers may want to pin a specific model per deployment without
    /// relying on ambient env vars.
    /// What: replaces `llm_model`; returns `self` for chaining.
    /// Test: covered transitively by the binary wiring tests.
    pub fn with_llm_model(mut self, model: impl Into<String>) -> Self {
        self.llm_model = model.into();
        self
    }

    /// Replace the embedder on an existing state. Useful when the binary
    /// builds state first and then tries to load fastembed, falling back
    /// silently when the model isn't available.
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = embedder;
        self
    }
}

/// JSON-RPC code for "the thing you named does not exist here".
///
/// Why (#6287): `-32602 invalid_params` says the request was malformed and
/// `-32603 internal_error` says this daemon broke. Neither describes
/// `analyze.scip_status` for an index nobody has ingested, which is a correct
/// request about an absent thing — the exact distinction #5049 added the
/// endpoint to draw, previously carried by HTTP 404 versus 200-with-`nodes: 0`.
/// Collapsing it into either neighbour would put that distinction back into
/// prose a client has to pattern-match on.
/// What: `-32004`, inside JSON-RPC's `-32000..=-32099` implementation-defined
/// server-error band, so it can never collide with a reserved code.
/// Test: `rpc_scip_status_reports_not_found_when_never_ingested`.
pub const CODE_NOT_FOUND: i64 = -32004;

/// What kind of failure a handler is reporting.
///
/// Why (#6287): this was an `axum::http::StatusCode`, which described how the
/// failure would be ENCODED rather than what it was. ADR-0032 removed the HTTP
/// surface that did the encoding, so a status code here would name a transport
/// this daemon no longer speaks. The vocabulary itself is worth keeping —
/// "the caller asked wrongly", "the upstream is down" and "we broke" are
/// different facts, and [`RpcError`] is what carries them onto the wire now.
/// What: one variant per constructor `ApiError` already had.
/// Test: `rpc_reports_invalid_params_for_a_request_naming_no_index`,
/// `rpc_scip_status_reports_not_found_when_never_ingested`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiErrorKind {
    /// The caller's own request is wrong.
    BadRequest,
    /// The request is well-formed but names something that does not exist.
    NotFound,
    /// This daemon failed.
    Internal,
    /// An upstream this daemon depends on — trusty-search, GitHub — failed.
    BadGateway,
    /// The handler's own work exceeded its deadline (#6018).
    GatewayTimeout,
}

/// Uniform handler error: a kind plus a message.
///
/// Why: every handler in this crate reports failure the same way, so the
/// mapping onto the wire lands once ([`From<ApiError> for RpcError`]) rather
/// than at twenty call sites.
/// What: holds an [`ApiErrorKind`] and a human-readable message; one
/// constructor per kind.
/// Test: covered transitively — any handler returning an `ApiError` is
/// exercised by `service::rpc_tests`.
pub(crate) struct ApiError {
    pub kind: ApiErrorKind,
    pub message: String,
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::BadRequest,
            message: msg.into(),
        }
    }
    /// Used by `analyze.scip_status` for "no overlay ingested" (#5049).
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::NotFound,
            message: msg.into(),
        }
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::Internal,
            message: msg.into(),
        }
    }
    pub fn bad_gateway(msg: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::BadGateway,
            message: msg.into(),
        }
    }

    /// The handler's own work exceeded its per-request deadline (#6018).
    ///
    /// Why: `analyze.diagnostics` used to run unbounded, so a client hit its
    /// read timeout and saw a transport-level abandon — nothing to distinguish
    /// "still working" from "daemon dead". An error frame naming the request
    /// that was abandoned, and how to make it fit, is what replaces that
    /// silence.
    /// Test: the timeout branch is exercised through
    /// `dispatch_stops_at_deadline_and_reports_cutoff`.
    pub fn gateway_timeout(msg: impl Into<String>) -> Self {
        Self {
            kind: ApiErrorKind::GatewayTimeout,
            message: msg.into(),
        }
    }
}

/// Map a handler failure onto the JSON-RPC error frame the client reads.
///
/// Why (#6287): the router takes `Result<Resp, RpcError>`, and without this
/// every handler would spell the same mapping itself.
/// What: `BadRequest` is the caller's fault and reports `invalid_params`;
/// `NotFound` gets [`CODE_NOT_FOUND`] so #5049's distinction survives the
/// transport change; everything else — an upstream that is down, a deadline
/// that expired, a panic-adjacent internal failure — is `internal_error`,
/// because none of them is something the caller could have sent differently.
/// The message is carried verbatim: it is the only place a diagnostics cutoff
/// or an unreachable trusty-search names itself.
/// Test: `rpc_reports_invalid_params_for_a_request_naming_no_index`,
/// `rpc_scip_status_reports_not_found_when_never_ingested`,
/// `rpc_reports_internal_error_when_search_is_unreachable`.
impl From<ApiError> for RpcError {
    fn from(e: ApiError) -> Self {
        let code = match e.kind {
            ApiErrorKind::BadRequest => CODE_INVALID_PARAMS,
            ApiErrorKind::NotFound => CODE_NOT_FOUND,
            ApiErrorKind::Internal | ApiErrorKind::BadGateway | ApiErrorKind::GatewayTimeout => {
                CODE_INTERNAL_ERROR
            }
        };
        RpcError::new(code, e.message)
    }
}

/// Fetch all chunks for `id` from the search daemon.
///
/// Why: every handler that needs chunk data uses this shared helper to get a
/// consistent error shape (502) when trusty-search is unreachable, rather than
/// each handler duplicating the error mapping.
/// What: calls `TrustySearchClient::get_chunks`; maps errors to `ApiError::bad_gateway`.
/// Test: covered transitively by every route handler test that expects 502 when
/// the stub search client is unreachable.
pub(crate) async fn fetch_chunks(
    state: &AnalyzerAppState,
    id: &str,
) -> Result<Vec<crate::types::CodeChunk>, ApiError> {
    state.search.get_chunks(id).await.map_err(|e| {
        tracing::warn!("get_chunks({id}) failed: {e:#}");
        ApiError::bad_gateway(format!("get_chunks({id}): {e:#}"))
    })
}
