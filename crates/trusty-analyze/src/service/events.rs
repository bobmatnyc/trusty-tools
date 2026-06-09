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

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::{AnalyzerRegistry, FactStore, TrustySearchClient};
use crate::embedder::{BowEmbedder, Embedder};
use crate::types::KgGraph;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use tokio::sync::{broadcast, RwLock};

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
    /// Neural / BOW embedder used by `/indexes/{id}/clusters` when the request
    /// asks for `method=neural`. Falls back to a fresh `BowEmbedder` when the
    /// request asks for `method=bow` (the default).
    pub embedder: Arc<dyn Embedder>,
    /// Per-index SCIP-derived knowledge graph overlay, populated by
    /// `POST /indexes/{id}/scip`. Merged into the response of
    /// `GET /indexes/{id}/graph` so consumers see the union of tree-sitter
    /// extraction and any precise SCIP indexes the user has uploaded.
    pub scip_overlays: Arc<RwLock<HashMap<String, KgGraph>>>,
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
    /// Optional GitHub webhook HMAC secret override.
    ///
    /// Why: `POST /webhooks/github` verifies the `X-Hub-Signature-256` HMAC.
    /// In production the secret comes from `GITHUB_WEBHOOK_SECRET`, but env
    /// vars are process-global and unsafe to mutate from concurrent tests.
    /// Threading the secret through state lets tests inject it deterministically
    /// while production still falls back to the env var.
    /// What: `Some(secret)` forces verification; `None` falls back to the env
    /// var (and skips verification when that is also unset).
    /// Test: `webhook_rejects_bad_signature` injects `Some(...)` here.
    pub webhook_secret: Option<String>,
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
    pub fn new(search: TrustySearchClient, facts: FactStore) -> Self {
        let (events_tx, _) = broadcast::channel(128);
        Self {
            search,
            facts,
            registry: Arc::new(AnalyzerRegistry::default_registry()),
            embedder: Arc::new(BowEmbedder::default()),
            scip_overlays: Arc::new(RwLock::new(HashMap::new())),
            events: events_tx,
            webhook_secret: None,
            api_key: std::env::var("OPENROUTER_API_KEY").ok(),
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
    ) -> Self {
        let (events_tx, _) = broadcast::channel(128);
        Self {
            search,
            facts,
            registry,
            embedder: Arc::new(BowEmbedder::default()),
            scip_overlays: Arc::new(RwLock::new(HashMap::new())),
            events: events_tx,
            webhook_secret: None,
            api_key: std::env::var("OPENROUTER_API_KEY").ok(),
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

    /// Override the GitHub webhook HMAC secret.
    ///
    /// Why: lets tests inject a deterministic secret and lets the binary pass
    /// `GITHUB_WEBHOOK_SECRET` in once at startup instead of re-reading the
    /// environment on every webhook request.
    /// What: sets `webhook_secret` and returns `self` for chaining.
    /// Test: `webhook_rejects_bad_signature` uses this to force verification.
    pub fn with_webhook_secret(mut self, secret: Option<String>) -> Self {
        self.webhook_secret = secret;
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
    #[allow(dead_code)]
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
