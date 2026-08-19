//! SSE event types, shared daemon state, and lightweight HTTP error wrapper.
//!
//! Why: Extracted from `service/mod.rs` to keep each module focused. This
//! module owns the "observable state" side of the service — what events are
//! broadcast and what shared data every handler receives — separated from the
//! route-handler logic that acts on that state.
//!
//! What: Defines `AnalyzerEvent` (the broadcast enum), `DEFAULT_PORT`,
//! `AnalyzerAppState` (cloneable shared state threaded through axum), and
//! `ApiError` (the uniform HTTP error type).
//!
//! Test: `sse_subscriber_receives_emitted_event` and
//! `sse_route_returns_event_stream_content_type` in `service/tests.rs`.

use std::sync::Arc;

use crate::core::{AnalyzerRegistry, FactStore, ScipOverlayStore, TrustySearchClient};
use crate::embedder::{BowEmbedder, Embedder};
use crate::types::SmellThresholds;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use tokio::sync::broadcast;

/// Live event broadcast over `/sse` for any dashboard subscribers.
///
/// Why: lets mutating endpoints (analysis, facts, SCIP ingest) push real-time
/// updates to the embedded admin UI without polling. Mirrors the
/// `DaemonEvent` pattern in `trusty-memory` so dashboards can be built with
/// shared client-side wiring.
/// What: tagged JSON enum serialized as `{"type": "...", ...fields}` for
/// each event class.
/// Test: `sse_stream_emits_fact_upserted` (see tests below) subscribes and
/// observes one event after `POST /facts`.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnalyzerEvent {
    AnalysisStarted {
        index_id: String,
    },
    AnalysisCompleted {
        index_id: String,
        chunk_count: usize,
        duration_ms: u64,
    },
    FactUpserted {
        subject: String,
        predicate: String,
    },
    FactDeleted {
        id: String,
    },
    ScipIngested {
        index_id: String,
        symbols_ingested: usize,
    },
}

/// Default port the analyzer daemon binds to. Picked to sit next to
/// trusty-search's 7878.
pub const DEFAULT_PORT: u16 = 7879;

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
    /// Broadcast sender for live `AnalyzerEvent` pushes to `/sse` subscribers.
    ///
    /// Why: mirrors trusty-memory's `events` channel so dashboards can react
    /// to mutations without polling. Cap of 128 buffers transient slow
    /// readers; lag emits a `lag` frame.
    /// What: cloneable `broadcast::Sender`. Subscribers obtained via
    /// `events.subscribe()` in the `/sse` handler.
    /// Test: `sse_stream_emits_fact_upserted` confirms a subscriber observes
    /// an emitted event after a successful POST.
    pub events: broadcast::Sender<AnalyzerEvent>,
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
        let (events_tx, _) = broadcast::channel(128);
        Self {
            search,
            facts,
            registry: Arc::new(AnalyzerRegistry::default_registry()),
            embedder: Arc::new(BowEmbedder::default()),
            scip_overlays,
            events: events_tx,
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
        let (events_tx, _) = broadcast::channel(128);
        Self {
            search,
            facts,
            registry,
            embedder: Arc::new(BowEmbedder::default()),
            scip_overlays,
            events: events_tx,
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

    /// Send an `AnalyzerEvent` to all connected SSE subscribers.
    ///
    /// Why: mutating handlers call this after a successful write so the
    /// dashboard can update without polling. Best-effort —
    /// `broadcast::Sender::send` returns `Err` only when there are no live
    /// receivers, which is fine (no listeners == no work to do).
    /// What: drops the send result so callers don't need to care.
    /// Test: covered transitively by SSE integration tests.
    pub fn emit(&self, event: AnalyzerEvent) {
        let _ = self.events.send(event);
    }
}

/// Lightweight error type for HTTP handlers — converts to JSON
/// `{"error": "..."}` with an appropriate status code.
///
/// Why: aligns the analyzer's handler shape with trusty-memory so client
/// SDKs and the embedded UI can rely on the same `{ error }` shape across
/// every trusty-* daemon.
/// What: holds a `StatusCode` and a message; constructors for 400/404/500.
/// Test: covered transitively — any handler returning an `ApiError` is
/// exercised by the integration suite.
pub(crate) struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    /// 404 — used by `GET /indexes/{id}/scip` for "no overlay ingested" (#5049).
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }
    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
    pub fn bad_gateway(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: msg.into(),
        }
    }

    /// 504 — the handler's own work exceeded its per-request deadline (#6018).
    ///
    /// Why: `GET /indexes/{id}/diagnostics` used to run unbounded, so a client
    /// hit its read timeout and saw a transport-level abandon — zero bytes, no
    /// status line, nothing to distinguish "still working" from "daemon dead".
    /// A 504 with a JSON body says which request was abandoned and how to make
    /// it fit.
    /// What: builds an `ApiError` at `GATEWAY_TIMEOUT`; `IntoResponse` renders
    /// it as `{"error": "<msg>"}` like every other variant.
    /// Test: the timeout branch is exercised through
    /// `dispatch_stops_at_deadline_and_reports_cutoff`; the response shape is
    /// shared with the other constructors.
    pub fn gateway_timeout(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::GATEWAY_TIMEOUT,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
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
