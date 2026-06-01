//! Sidecar HTTP daemon for trusty-analyzer.
//!
//! Why: Keeps analysis isolated from trusty-search. The daemon fetches chunks
//! from the search daemon over HTTP (`TrustySearchClient::get_chunks`) and
//! computes complexity / smells / quality / facts in-process. It does not
//! talk to trusty-search's redb files directly — the search daemon is the
//! single source of truth for chunk data.
//!
//! What: an axum router with a small surface:
//! - `GET  /health`
//! - `GET  /indexes`                            proxy to trusty-search
//! - `GET  /indexes/{id}/complexity_hotspots`   top-N by cyclomatic
//! - `GET  /indexes/{id}/smells`                chunks with at least one smell
//! - `GET  /indexes/{id}/quality`               aggregate report
//! - `GET  /facts`                              list / filter facts
//! - `POST /facts`                              upsert a fact
//! - `DELETE /facts/{id}`                       delete a fact
//!
//! Test: `cargo test -p trusty-analyzer-service` boots the router with a stub
//! search client and exercises every route end-to-end.

mod ui;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::core::complexity::{compute_complexity_for, detect_smells};
use crate::core::{
    analyze_refactor, bow_embedding, cluster as run_cluster, extract_doc_comments,
    extract_kg_from_scip, facts::new_fact, quality, AnalyzerRegistry, ClusterResult, FactStore,
    IndexSummary, NerExtractor, RefactorSuggestion, ScipIngestSummary, Severity,
    TrustySearchClient,
};
use crate::embedder::{BowEmbedder, Embedder, EmbedderKind};
use crate::types::{KgGraph, KgNode, RawEntity};
use anyhow::Result;
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Redirect, Response},
    routing::{delete, get, post},
    Router,
};
use serde::{Deserialize, Serialize};
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

/// Build the axum router around `state`.
///
/// Why: Composes the analyzer's HTTP surface in one place so callers (binary,
/// tests, embedded use) all get the same routes and middleware stack. The
/// shared `trusty_common::server::with_standard_middleware` layer keeps CORS,
/// tracing, and gzip behavior consistent across every trusty-* daemon.
/// What: Wires every route handler to its path (axum 0.8 `{name}` capture
/// syntax), binds the shared state, then applies the standard middleware
/// stack.
/// Test: `cargo test -p trusty-analyzer-service` drives every route through
/// the returned router; the middleware composition is smoke-tested
/// transitively (any layering regression breaks the suite).
pub fn build_router(state: AnalyzerAppState) -> Router {
    let router = Router::new()
        .route("/", get(|| async { Redirect::permanent("/ui/") }))
        .route("/health", get(health))
        .route("/sse", get(sse_handler))
        .route("/indexes", get(list_indexes))
        .route(
            "/indexes/{id}/complexity_hotspots",
            get(complexity_hotspots),
        )
        .route("/indexes/{id}/smells", get(smells))
        .route(
            "/indexes/{id}/refactor-suggestions",
            get(refactor_suggestions),
        )
        .route("/indexes/{id}/quality", get(quality_report))
        .route("/indexes/{id}/diagnostics", get(diagnostics_for_index))
        .route("/indexes/{id}/graph", get(graph_for_index))
        .route("/indexes/{id}/entities", get(entities_for_index))
        .route("/indexes/{id}/clusters", get(clusters_for_index))
        .route("/indexes/{id}/ner", get(ner_for_index))
        .route("/indexes/{id}/scip", post(ingest_scip))
        .route("/review", post(review_diff_handler))
        .route("/review/github-pr", post(review_github_pr_handler))
        .route("/analyze/deep", post(deep_analyze_handler))
        .route("/webhooks/github", post(github_webhook_handler))
        .route("/facts", get(list_facts).post(upsert_fact))
        .route("/facts/{id}", delete(delete_fact))
        .route("/ui", get(|| async { Redirect::permanent("/ui/") }))
        .route("/ui/", get(ui::ui_index_handler))
        .route("/ui/{*path}", get(ui::ui_asset_handler))
        .with_state(Arc::new(state));
    trusty_common::server::with_standard_middleware(router)
}

/// SSE endpoint pushing `AnalyzerEvent` frames to dashboard subscribers.
///
/// Why: lets the embedded admin UI react to mutations (facts upsert/delete,
/// SCIP ingest) without polling. Mirrors the trusty-memory `/sse` handler
/// exactly so client-side wiring is portable across daemons.
/// What: subscribes to `state.events`, emits an initial `connected` frame,
/// then forwards every event as `data: <json>\n\n`. Lagged subscribers
/// receive a `lag` frame; channel closure ends the stream.
/// Test: `sse_stream_emits_fact_upserted` confirms subscribe + emit + receive.
async fn sse_handler(State(state): State<Arc<AnalyzerAppState>>) -> impl IntoResponse {
    use futures::StreamExt;
    use tokio_stream::wrappers::BroadcastStream;

    let rx = state.events.subscribe();
    let initial = futures::stream::once(async {
        Ok::<axum::body::Bytes, std::io::Error>(axum::body::Bytes::from(
            "data: {\"type\":\"connected\"}\n\n",
        ))
    });
    let events = BroadcastStream::new(rx).map(|res| {
        let frame = match res {
            Ok(event) => match serde_json::to_string(&event) {
                Ok(json) => format!("data: {json}\n\n"),
                Err(e) => format!("data: {{\"type\":\"error\",\"message\":\"{e}\"}}\n\n"),
            },
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                format!("data: {{\"type\":\"lag\",\"skipped\":{n}}}\n\n")
            }
        };
        Ok::<axum::body::Bytes, std::io::Error>(axum::body::Bytes::from(frame))
    });
    let stream = initial.chain(events);

    axum::response::Response::builder()
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(axum::body::Body::from_stream(stream))
        .expect("valid SSE response")
}

/// Bind to `start_port` (or auto-pick a free port walking forward) and run
/// the daemon until the future returns. The actually-bound address is also
/// written to the shared trusty-* daemon address file so other tools can
/// discover the live port without re-implementing the search.
///
/// Why: port auto-detection and daemon-addr handshake are duplicated across
/// every trusty-* daemon. Using the shared `trusty_common` helpers keeps
/// behavior consistent (warn logging, fixed walk window, addr file shape).
/// What: walks up to 64 ports forward from `start_port`, logs the live URL,
/// then `axum::serve`s the router.
/// Test: integration tests bind their own listener — exercised by
/// `cargo test -p trusty-analyzer-service`.
pub async fn serve(state: AnalyzerAppState, start_port: u16) -> Result<()> {
    let start_addr: SocketAddr = ([127, 0, 0, 1], start_port).into();
    let listener = trusty_common::bind_with_auto_port(start_addr, 64).await?;
    let actual = listener.local_addr()?;
    trusty_common::write_daemon_addr("trusty-analyze", &actual.to_string())?;
    tracing::info!("trusty-analyze listening on http://{actual}");
    let app = build_router(state);
    // Why (issue #534): without `with_graceful_shutdown`, SIGTERM from
    // `launchctl bootout` kills the process before any cleanup code in the
    // caller (PID file removal, supervisor shutdown) can run, and in-flight
    // analysis requests are dropped mid-stream. The shared `shutdown_signal()`
    // helper waits for SIGTERM or SIGINT; when it resolves, axum drains active
    // connections before returning control here so cleanup runs normally.
    axum::serve(listener, app)
        .with_graceful_shutdown(trusty_common::shutdown_signal())
        .await?;
    Ok(())
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    search_reachable: bool,
}

/// Why: Reflects the hard runtime dependency on trusty-search — there is no
/// meaningful "ok" state when the search daemon is unreachable.
/// What: Probes trusty-search GET /health; returns 200 + "ok" when reachable,
/// 503 + "degraded" when not.
/// Test: point the client at a dead search URL and assert HTTP 503 with
/// `status == "degraded"` and `search_reachable == false`.
async fn health(
    State(state): State<Arc<AnalyzerAppState>>,
) -> Result<Json<HealthResponse>, (StatusCode, Json<HealthResponse>)> {
    let search_reachable = state.search.health().await.unwrap_or(false);
    let response = HealthResponse {
        status: if search_reachable { "ok" } else { "degraded" },
        version: env!("CARGO_PKG_VERSION"),
        search_reachable,
    };
    if search_reachable {
        Ok(Json(response))
    } else {
        Err((StatusCode::SERVICE_UNAVAILABLE, Json(response)))
    }
}

async fn list_indexes(
    State(state): State<Arc<AnalyzerAppState>>,
) -> Result<Json<Vec<IndexSummary>>, ApiError> {
    state.search.list_indexes().await.map(Json).map_err(|e| {
        tracing::warn!("list_indexes proxy failed: {e:#}");
        ApiError::bad_gateway(format!("upstream search daemon: {e:#}"))
    })
}

#[derive(Deserialize)]
pub struct HotspotsParams {
    #[serde(default = "default_top_n")]
    pub top_n: usize,
}

fn default_top_n() -> usize {
    20
}

async fn complexity_hotspots(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<HotspotsParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let hotspots = quality::complexity_hotspots(&chunks, params.top_n);
    Ok(Json(serde_json::json!({
        "index_id": id,
        "top_n": params.top_n,
        "hotspots": hotspots,
    })))
}

async fn smells(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let smelly = quality::smelly_chunks(&chunks);
    Ok(Json(serde_json::json!({
        "index_id": id,
        "count": smelly.len(),
        "chunks": smelly,
    })))
}

#[derive(Deserialize)]
pub struct RefactorParams {
    /// Optional path filter — only suggest refactors for chunks in this file.
    pub file: Option<String>,
    /// Minimum severity to include (`"low"` / `"medium"` / `"high"` /
    /// `"critical"`). Defaults to `"low"`.
    pub min_severity: Option<String>,
    /// Cap on the number of suggestions returned. Defaults to 20.
    pub top_k: Option<usize>,
}

/// Why: callers want "what should I refactor and why" — not just raw
/// complexity numbers. This handler turns metrics + smells into actionable
/// `RefactorSuggestion`s and sorts them by severity so the worst offenders
/// surface first.
/// What: fetches chunks for `id`, computes complexity per chunk (language-
/// aware via file extension dispatch), runs `analyze_refactor`, filters by
/// `file` and `min_severity`, sorts by `(severity desc, complexity_before
/// desc)`, and truncates to `top_k`.
/// Test: a chunk with grade F + LongFunction returns one Critical
/// ExtractMethod suggestion; covered transitively via `core::refactor` tests.
async fn refactor_suggestions(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<RefactorParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let min_severity = params
        .min_severity
        .as_deref()
        .and_then(Severity::parse)
        .unwrap_or(Severity::Low);
    let top_k = params.top_k.unwrap_or(20);

    let mut out: Vec<RefactorSuggestion> = Vec::new();
    for chunk in &chunks {
        if let Some(file) = params.file.as_deref() {
            if chunk.file != file {
                continue;
            }
        }
        let lang = language_for_path(&chunk.file);
        let metrics = compute_complexity_for(&chunk.content, lang);
        let smells = detect_smells(&chunk.content);
        let mut suggestions = analyze_refactor(
            &chunk.id,
            &chunk.file,
            chunk.start_line as u32,
            chunk.end_line as u32,
            chunk.function_name.as_deref(),
            &metrics,
            &smells,
        );
        suggestions.retain(|s| s.severity >= min_severity);
        out.extend(suggestions);
    }

    out.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| b.complexity_before.cmp(&a.complexity_before))
    });
    out.truncate(top_k);

    Ok(Json(serde_json::json!({
        "index_id": id,
        "count": out.len(),
        "min_severity": min_severity_label(&min_severity),
        "suggestions": out,
    })))
}

fn language_for_path(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".rs") {
        "rust"
    } else if lower.ends_with(".tsx") {
        "tsx"
    } else if lower.ends_with(".ts") {
        "typescript"
    } else if lower.ends_with(".jsx") {
        "jsx"
    } else if lower.ends_with(".js") {
        "javascript"
    } else if lower.ends_with(".py") {
        "python"
    } else if lower.ends_with(".go") {
        "go"
    } else if lower.ends_with(".java") {
        "java"
    } else {
        "unknown"
    }
}

fn min_severity_label(s: &Severity) -> &'static str {
    match s {
        Severity::Low => "low",
        Severity::Medium => "medium",
        Severity::High => "high",
        Severity::Critical => "critical",
    }
}

async fn quality_report(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
) -> Result<Json<quality::QualityReport>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    Ok(Json(quality::aggregate_quality(&chunks)))
}

/// Query parameters for the on-demand diagnostics endpoint.
#[derive(Deserialize)]
pub struct DiagnosticsParams {
    /// Restrict analysis to a single language tag (`"rust"`, `"python"`, ...).
    pub language: Option<String>,
    /// Comma-separated list of tool names to run; defaults to all available.
    pub tools: Option<String>,
}

/// `GET /indexes/{id}/diagnostics` — run available external static-analysis
/// tools (clippy, ruff, biome, ...) across the index corpus on demand.
///
/// Why: tree-sitter heuristics are uniform but shallow; real linters catch
/// far more, but only when their binary is installed. This endpoint discovers
/// what is available and runs it, file by file.
/// What: fetches the corpus, reconstructs whole-file content from chunks,
/// writes each file to a scratch dir, and dispatches to `ToolRegistry`.
/// Test: `diagnostics_endpoint_returns_empty_when_no_tools` boots the router
/// with a stub client and confirms a well-formed empty response.
async fn diagnostics_for_index(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<DiagnosticsParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let tool_filter: Option<Vec<String>> = params.tools.as_ref().map(|s| {
        s.split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect()
    });

    // Reconstruct per-file content by stitching chunks in line order. Chunk
    // windows can overlap, so we keep the longest content seen per file as a
    // best-effort whole-file reconstruction.
    let mut by_file: HashMap<String, String> = HashMap::new();
    for chunk in &chunks {
        let entry = by_file.entry(chunk.file.clone()).or_default();
        if chunk.content.len() > entry.len() {
            *entry = chunk.content.clone();
        }
    }

    // Heavy work (process spawns, blocking I/O) runs off the async runtime.
    let language_filter = params.language.clone();
    let diagnostics: Vec<crate::core::ToolDiagnostic> = tokio::task::spawn_blocking(move || {
        run_diagnostics_blocking(by_file, language_filter, tool_filter)
    })
    .await
    .map_err(|e| ApiError::internal(format!("diagnostics task panicked: {e}")))?;

    Ok(Json(serde_json::json!({
        "index_id": id,
        "count": diagnostics.len(),
        "diagnostics": diagnostics,
    })))
}

/// Blocking core of the diagnostics endpoint: writes files to a scratch dir
/// and runs the discovered tools. Kept separate so it can run under
/// `spawn_blocking`.
fn run_diagnostics_blocking(
    by_file: HashMap<String, String>,
    language_filter: Option<String>,
    tool_filter: Option<Vec<String>>,
) -> Vec<crate::core::ToolDiagnostic> {
    use crate::core::global_registry;
    use crate::lang::LanguageDetector;

    let registry = global_registry();
    let scratch = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("failed to create scratch dir for diagnostics: {e}");
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for (file, content) in by_file {
        let Some(lang) = LanguageDetector::detect_file(&file) else {
            continue;
        };
        if let Some(want) = &language_filter {
            if &lang != want {
                continue;
            }
        }
        if registry.tools_for(&lang).is_empty() {
            continue;
        }

        // Preserve the original file name so tools key diagnostics correctly.
        let name = std::path::Path::new(&file)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "chunk.txt".to_string());
        let path = scratch.path().join(&name);
        if let Err(e) = std::fs::write(&path, &content) {
            tracing::warn!("failed to write scratch file {name}: {e}");
            continue;
        }

        let result = match &tool_filter {
            Some(names) => registry.run_named(&lang, names, &path, &content),
            None => registry.run_all(&lang, &path, &content),
        };
        match result {
            Ok(mut diags) => {
                // Rewrite the scratch path back to the index-relative path.
                for d in &mut diags {
                    d.file = file.clone();
                }
                out.extend(diags);
            }
            Err(e) => tracing::warn!("diagnostics for {file} failed: {e:#}"),
        }
    }
    out
}

#[derive(Deserialize)]
pub struct GraphQueryParams {
    /// Restrict to a single language (`"rust"`, `"typescript"`, ...).
    pub language: Option<String>,
}

/// Why: Phase 2 surfaces the language-neutral knowledge graph to consumers
/// (Claude Code, web UIs, etc.) so they can navigate symbols across files.
/// What: Fetch chunks for `index`, run the language registry, optionally
/// filter to `?language=`, and return the merged `KgGraph` as JSON.
/// Test: with a mock index containing a Rust chunk, GET returns at least
/// one Function node tagged `language=rust`.
async fn graph_for_index(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<GraphQueryParams>,
) -> Result<Json<KgGraph>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let res = state.registry.analyze(&chunks);
    let mut graph = res.graph;
    // Merge any SCIP-derived overlay that the user has uploaded for this
    // index. SCIP supplies fully-resolved cross-file symbols which the
    // tree-sitter adapters cannot derive on their own, so the union is
    // strictly more useful than either alone.
    if let Some(overlay) = state.scip_overlays.read().await.get(&id).cloned() {
        graph.merge(overlay);
        graph = crate::core::link(graph);
    }
    if let Some(lang) = params.language.as_deref() {
        let keep_nodes: std::collections::HashSet<String> = graph
            .nodes
            .iter()
            .filter(|n| n.language == lang)
            .map(|n| n.id.clone())
            .collect();
        graph.nodes.retain(|n| keep_nodes.contains(&n.id));
        graph
            .edges
            .retain(|e| keep_nodes.contains(&e.from) && keep_nodes.contains(&e.to));
    }
    Ok(Json(graph))
}

#[derive(Deserialize)]
pub struct EntitiesQueryParams {
    pub kind: Option<String>,
    pub language: Option<String>,
}

/// Why: Many consumers only want a flat node listing, sorted, for browsing
/// (autocomplete, file outlines).
/// What: Same pipeline as `/graph`, but returns just `Vec<KgNode>` sorted by
/// `(kind, name)`. Optional `?kind=` and `?language=` filters.
/// Test: filtering by `kind=Function` returns only Function nodes.
async fn entities_for_index(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<EntitiesQueryParams>,
) -> Result<Json<Vec<KgNode>>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let res = state.registry.analyze(&chunks);
    let mut nodes = res.graph.nodes;
    if let Some(lang) = params.language.as_deref() {
        nodes.retain(|n| n.language == lang);
    }
    if let Some(kind) = params.kind.as_deref() {
        nodes.retain(|n| format!("{:?}", n.kind) == kind);
    }
    nodes.sort_by(|a, b| {
        format!("{:?}", a.kind)
            .cmp(&format!("{:?}", b.kind))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(Json(nodes))
}

#[derive(Deserialize)]
pub struct ClusterQueryParams {
    /// Number of clusters to compute. Defaults to 8, clamped to [1, 50].
    pub k: Option<usize>,
    /// Embedding method: `"bow"` (default, deterministic 256-dim) or
    /// `"neural"` (fastembed all-MiniLM-L6-v2, 384-dim).
    #[serde(default)]
    pub method: Option<EmbedderKind>,
}

#[derive(Serialize)]
pub struct ClusterResponseItem {
    pub id: usize,
    pub label: String,
    pub members: Vec<String>,
    pub cohesion: f32,
    pub size: usize,
}

#[derive(Serialize)]
pub struct ClusterResponse {
    pub k: usize,
    /// Which embedder produced the vectors (`"bow"` or `"neural"`).
    pub method: String,
    /// Dimension of the embedding vectors used.
    pub dim: usize,
    pub iterations: usize,
    pub chunk_count: usize,
    pub clusters: Vec<ClusterResponseItem>,
}

fn cluster_items_from(r: ClusterResult) -> Vec<ClusterResponseItem> {
    r.clusters
        .into_iter()
        .map(|c| ClusterResponseItem {
            id: c.id,
            label: c.label,
            size: c.members.len(),
            members: c.members,
            cohesion: c.cohesion,
        })
        .collect()
}

/// Why: surfaces "what themes does this codebase contain?" without needing a
/// full knowledge graph or neural embedder. Useful for codebase exploration
/// and high-level summaries.
/// What: fetches chunks for `index`, derives a 256-dim bag-of-words vector
/// per chunk, runs seeded k-means, and returns the cluster assignments.
/// Test: covered indirectly by trusty-analyzer-core's `concept_cluster` tests;
/// the route wiring is exercised by `clusters_route_returns_502_when_search_down`.
async fn clusters_for_index(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<ClusterQueryParams>,
) -> Result<Json<ClusterResponse>, ApiError> {
    const BOW_DIM: usize = 256;
    let k = params.k.unwrap_or(8).clamp(1, 50);
    let method = params.method.clone().unwrap_or_default();
    let chunks = fetch_chunks(&state, &id).await?;
    if chunks.is_empty() {
        return Ok(Json(ClusterResponse {
            k,
            method: method.as_str().to_string(),
            dim: 0,
            iterations: 0,
            chunk_count: 0,
            clusters: Vec::new(),
        }));
    }

    // Resolve embedder. For neural, defer to the shared state embedder (which
    // may itself be BOW if fastembed failed to load at startup). For BOW,
    // construct a fresh stateless BowEmbedder so we never go through fastembed
    // when the user explicitly asked for BOW.
    let neural_embedder: Arc<dyn Embedder> = state.embedder.clone();
    let bow_embedder = BowEmbedder::with_dim(BOW_DIM);
    let effective_kind_initial: EmbedderKind = match method {
        EmbedderKind::Neural => neural_embedder.kind(),
        EmbedderKind::Bow => EmbedderKind::Bow,
    };

    // Why: `NeuralEmbedder::embed_batch` holds a `std::sync::Mutex` over ONNX
    // inference, which can block for tens-to-hundreds of milliseconds. Running
    // it directly on a tokio executor thread starves other async tasks queued
    // on that thread. `spawn_blocking` moves the call onto a dedicated blocking
    // thread pool so the executor stays responsive.
    // What: converts the chunk contents to owned `String`s (required to cross
    // the `'static` closure boundary), clones the `Arc<dyn Embedder>`, then
    // awaits the blocking join handle. Join-error is mapped to a warn + BOW
    // fallback so the endpoint never 500s on a temporary model hiccup.
    // Test: the existing cluster endpoint tests (e.g. `cluster_endpoint_bow`)
    // exercise this path; the spawn_blocking wrapping does not change observable
    // outputs, only prevents executor starvation.

    // Owned strings are needed both for the Neural spawn_blocking closure
    // (which requires 'static) and for the BOW fallback path.
    let owned_texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();

    let embed_result: anyhow::Result<(Vec<Vec<f32>>, EmbedderKind, usize)> = match method {
        EmbedderKind::Neural => {
            let embedder_arc = Arc::clone(&neural_embedder);
            let dim = embedder_arc.dim();
            let texts_for_task = owned_texts.clone();
            tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = texts_for_task.iter().map(String::as_str).collect();
                embedder_arc.embed_batch(&refs)
            })
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!("embed_batch task panicked: {e}")))
            .map(|v| (v, EmbedderKind::Neural, dim))
        }
        EmbedderKind::Bow => {
            let vecs: Vec<Vec<f32>> = owned_texts
                .iter()
                .map(|t| bow_embedding(t, BOW_DIM))
                .collect();
            Ok((vecs, EmbedderKind::Bow, BOW_DIM))
        }
    };
    let (vecs, effective_kind, dim) = match embed_result {
        Ok(triple) => triple,
        Err(e) => {
            tracing::warn!(
                "embedder ({:?}) failed ({e:#}); falling back to BOW",
                effective_kind_initial
            );
            let fallback: Vec<Vec<f32>> = owned_texts
                .iter()
                .map(|t| bow_embedding(t, BOW_DIM))
                .collect();
            (fallback, EmbedderKind::Bow, BOW_DIM)
        }
    };
    // Suppress unused-variable warning if bow_embedder was not directly used
    let _ = &bow_embedder;

    let embeddings: Vec<(String, Vec<f32>)> = chunks
        .iter()
        .zip(vecs)
        .map(|(c, v)| (c.id.clone(), v))
        .collect();
    let result = run_cluster(&embeddings, k, 100, 42);
    let iterations = result.iterations;
    Ok(Json(ClusterResponse {
        k,
        method: effective_kind.as_str().to_string(),
        dim,
        iterations,
        chunk_count: chunks.len(),
        clusters: cluster_items_from(result),
    }))
}

#[derive(Deserialize)]
pub struct NerQueryParams {
    /// Cap on the number of entities returned (after extraction).
    pub top_k: Option<usize>,
}

/// Why: surfaces named-entity candidates pulled from doc comments so callers
/// (Claude Code, UI dashboards) can browse natural-language concepts side by
/// side with structural symbols. The route is always available; the actual
/// ONNX NER model is feature-gated and opportunistically loaded at startup.
/// What: fetches chunks for `id`, runs `extract_doc_comments` on each chunk's
/// content, runs the NER extractor (no-op when the `ner` feature is disabled
/// or the model file is missing), and returns the entities truncated to
/// `top_k` (default 50).
/// Test: with a stub search client returning no chunks the handler returns an
/// empty array and HTTP 200; the NER feature flag is exercised by the core
/// crate's `ner` module tests.
async fn ner_for_index(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<NerQueryParams>,
) -> Result<Json<Vec<RawEntity>>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let top_k = params.top_k.unwrap_or(50);
    let extractor = NerExtractor::try_load();

    let mut entities: Vec<RawEntity> = Vec::new();
    for chunk in &chunks {
        let docs = extract_doc_comments(&chunk.content);
        if docs.is_empty() {
            continue;
        }
        entities.extend(extractor.extract(&docs, &chunk.file));
        if entities.len() >= top_k {
            break;
        }
    }
    entities.truncate(top_k);
    Ok(Json(entities))
}

#[derive(Serialize)]
pub struct ScipIngestResponse {
    pub index_id: String,
    #[serde(flatten)]
    pub summary: ScipIngestSummary,
}

/// Why: SCIP indexes carry fully-resolved cross-file symbols that the
/// tree-sitter adapters can't derive (call resolution, trait implementations
/// across files, generics). Ingesting them is how the analyzer goes from
/// "approximate" to "precise" for languages with a real SCIP indexer.
/// What: accepts a SCIP `Index` protobuf as raw bytes, converts it to a
/// `KgGraph`, stores it as a per-index overlay, and returns ingest stats.
/// The overlay is merged into `/indexes/{id}/graph` responses.
/// Test: `scip_ingest_round_trip` POSTs a hand-built SCIP index and verifies
/// the resulting graph appears in the `/graph` response.
async fn ingest_scip(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<ScipIngestResponse>, ApiError> {
    let (graph, summary) = extract_kg_from_scip(&body).map_err(|e| {
        tracing::warn!("SCIP ingest for {id} failed: {e:#}");
        ApiError::bad_request(format!("invalid SCIP protobuf: {e:#}"))
    })?;
    let symbols_ingested = summary.kg_nodes;
    state.scip_overlays.write().await.insert(id.clone(), graph);
    state.emit(AnalyzerEvent::ScipIngested {
        index_id: id.clone(),
        symbols_ingested,
    });
    Ok(Json(ScipIngestResponse {
        index_id: id,
        summary,
    }))
}

#[derive(Deserialize)]
pub struct ReviewQueryParams {
    /// Index ID to cross-reference the diff against in trusty-search. Required:
    /// review pulls the index's chunk corpus so the report reflects already-
    /// computed complexity for the touched files.
    pub index_id: Option<String>,
}

/// Why: PR review is most valuable before code lands; this endpoint lets CI
/// and tooling POST a raw unified diff and get a structured quality report.
/// Like every other analysis route, `/review` is backed by trusty-search — it
/// fetches the named index's chunk corpus so the report can surface
/// trusty-search's already-computed complexity for the files the diff touches.
/// What: reads the request body as a unified diff (`text/x-patch`), requires a
/// `?index_id=` query param (400 if missing), fetches the index corpus via the
/// shared `TrustySearchClient`, runs `analyze_diff_with_client`, and returns
/// the `ReviewReport` as JSON. This endpoint is deliberately deterministic and
/// LLM-free — opt into the LLM narrative via `POST /analyze/deep`.
/// Test: `review_endpoint_requires_index_id` checks the 400 path;
/// `review_endpoint_rejects_malformed_diff` checks malformed-diff handling.
async fn review_diff_handler(
    State(state): State<Arc<AnalyzerAppState>>,
    Query(params): Query<ReviewQueryParams>,
    body: Bytes,
) -> Result<Json<crate::core::ReviewReport>, ApiError> {
    let index_id = params
        .index_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request("missing required 'index_id' query parameter"))?;
    let diff = std::str::from_utf8(&body)
        .map_err(|e| ApiError::bad_request(format!("diff body is not valid UTF-8: {e}")))?;
    let report = crate::core::analyze_diff_with_client(diff, &state.search, index_id)
        .await
        .map_err(|e| match e {
            crate::core::ReviewError::MalformedHunkHeader(_) => {
                ApiError::bad_request(format!("invalid diff: {e}"))
            }
            crate::core::ReviewError::Search(_) => ApiError::bad_gateway(format!("{e}")),
        })?;
    Ok(Json(report))
}

/// Why: lets CI and tooling analyze a GitHub PR by number without having to
/// fetch the diff themselves — the daemon fetches it, runs the review, and
/// optionally posts a comment back.
/// What: reads `GITHUB_TOKEN` from the environment (400 if absent), fetches the
/// PR's unified diff from the GitHub API, runs `analyze_diff_with_client`
/// against the request's `index_id`, posts a markdown comment when
/// `post_comment` is true, and returns the `ReviewReport` JSON.
/// Test: `github_pr_endpoint_requires_token` checks the missing-token 400 path.
async fn review_github_pr_handler(
    State(state): State<Arc<AnalyzerAppState>>,
    Json(req): Json<crate::core::GithubPrRequest>,
) -> Result<Json<crate::core::ReviewReport>, ApiError> {
    let token = std::env::var("GITHUB_TOKEN").map_err(|_| {
        ApiError::bad_request("GITHUB_TOKEN environment variable is not set on the daemon")
    })?;
    // Why: GitHub API calls can take several seconds on large diffs; without
    // timeouts the handler thread hangs indefinitely, exhausting the axum
    // worker pool under concurrent PR review requests.
    // What: 30 s per-request + 5 s connect timeout, matching the pattern used
    // by `TrustySearchClient` in `src/core/client.rs`.
    // Test: `github_pr_endpoint_requires_token` exercises this code path.
    let client = reqwest::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("reqwest ClientBuilder is infallible with valid config");
    let diff = crate::core::fetch_pr_diff(&client, &req.owner, &req.repo, req.pr, &token)
        .await
        .map_err(|e| ApiError::bad_gateway(format!("fetch PR diff: {e}")))?;
    let report = crate::core::analyze_diff_with_client(&diff, &state.search, &req.index_id)
        .await
        .map_err(|e| match e {
            crate::core::ReviewError::MalformedHunkHeader(_) => {
                ApiError::bad_request(format!("invalid diff: {e}"))
            }
            crate::core::ReviewError::Search(_) => ApiError::bad_gateway(format!("{e}")),
        })?;
    if req.post_comment {
        let markdown = crate::core::format_review_as_markdown(&report);
        crate::core::post_pr_comment(&client, &req.owner, &req.repo, req.pr, &markdown, &token)
            .await
            .map_err(|e| ApiError::bad_gateway(format!("post PR comment: {e}")))?;
    }
    Ok(Json(report))
}

/// Request body for `POST /analyze/deep`.
///
/// Why: deep analysis is opt-in and parameterised, so the endpoint takes a
/// JSON body rather than a query string. Callers either pass a pre-computed
/// [`crate::core::ReviewReport`] (to avoid the re-review cost) or omit it,
/// in which case the endpoint synthesises a report by aggregating the index's
/// chunk corpus with the same complexity / smell math used by `/review`.
/// What: `index_id` is required; `report` is optional; `model` overrides the
/// daemon-default LLM model.
/// Test: `deep_endpoint_requires_index_id` covers the missing-field 400 path;
/// `deep_endpoint_requires_api_key` covers the no-key 400 path.
#[derive(Debug, Deserialize)]
pub struct DeepAnalyzeRequest {
    pub index_id: String,
    #[serde(default)]
    pub report: Option<crate::core::ReviewReport>,
    #[serde(default)]
    pub model: Option<String>,
}

/// Why: turns a deterministic [`crate::core::ReviewReport`] into a
/// [`crate::core::DeepAnalysisReport`] by running an OpenRouter chat call. The
/// LLM pass is deliberately separated from `/review` so the deterministic
/// surface stays cheap, reproducible, and free of network/AI dependencies.
/// What: requires `index_id` in the JSON body; either uses the provided
/// `report` or builds one from the index's chunk corpus (no diff: the
/// synthesised report treats the whole indexed corpus as one big "file" set
/// for grading purposes). Reads frameworks from the analyzer's `FactStore`
/// (predicate `"uses_framework"`), calls `deep_analysis`, and returns the
/// wrapper report. Requires `OPENROUTER_API_KEY` to be configured at startup
/// — returns 400 with `MissingApiKey` otherwise.
/// Test: `deep_endpoint_requires_api_key`, `deep_endpoint_requires_index_id`.
async fn deep_analyze_handler(
    State(state): State<Arc<AnalyzerAppState>>,
    Json(req): Json<DeepAnalyzeRequest>,
) -> Result<Json<crate::core::DeepAnalysisReport>, ApiError> {
    if req.index_id.trim().is_empty() {
        return Err(ApiError::bad_request("missing required 'index_id' field"));
    }

    // Determine the effective model id so we can decide whether an API key is
    // required. Bedrock models (prefixed with "bedrock/") use AWS credential
    // chain auth — no OPENROUTER_API_KEY needed.
    let effective_model = req
        .model
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&state.llm_model);

    let uses_bedrock = effective_model.starts_with(crate::core::explain::BEDROCK_MODEL_PREFIX);

    let api_key = if uses_bedrock {
        // Bedrock path: no OpenRouter key needed.
        None
    } else {
        let key = state.api_key.as_deref().filter(|s| !s.is_empty());
        if key.is_none() {
            return Err(ApiError::bad_request(
                "OPENROUTER_API_KEY is not configured on the daemon; \
                 set OPENROUTER_API_KEY in the environment and restart the daemon, \
                 or use a bedrock/<model-id> model instead",
            ));
        }
        key
    };

    // Either use the caller-supplied report, or synthesise one from the index
    // corpus. Synthesis: treat the whole indexed corpus as one big "no-diff"
    // review by running the deterministic complexity/smell math over every
    // chunk and rolling up per-file metrics. This keeps the LLM input shaped
    // identically to the diff-based path.
    let report = match req.report {
        Some(r) => r,
        None => synthesise_review_from_index(&state, &req.index_id).await?,
    };

    // Pull detected frameworks from the FactStore (recorded by `record_frameworks`).
    let frameworks = lookup_frameworks(&state, &req.index_id);

    let model_override = req.model.as_deref();
    let report = crate::core::deep_analysis(
        &req.index_id,
        report,
        frameworks,
        api_key,
        model_override.or(Some(&state.llm_model)),
    )
    .await
    .map_err(|e| match e {
        crate::core::DeepAnalysisError::MissingApiKey => ApiError::bad_request(format!("{e}")),
        crate::core::DeepAnalysisError::BedrockAuth => ApiError::bad_request(format!("{e}")),
        crate::core::DeepAnalysisError::Chat(_) => ApiError::bad_gateway(format!("{e}")),
    })?;
    Ok(Json(report))
}

/// Build a [`crate::core::ReviewReport`] from an index's chunk corpus without
/// any diff input.
///
/// Why: `POST /analyze/deep` accepts an optional `report` field — when the
/// caller omits it, we still need a deterministic report shape to feed the
/// LLM. Synthesising one from the indexed corpus gives the LLM the same
/// metrics it would see for a diff that touched every file in the index.
/// What: fetches the corpus, groups chunks by file, computes per-file
/// complexity / smells / grade, and aggregates them into a [`ReviewReport`]
/// with `source = NewFile` (since we have no diff to anchor "modified chunks"
/// against).
/// Test: covered indirectly by `deep_endpoint_requires_api_key` (the synth
/// step succeeds against the stub search; the 400 then comes from the key
/// guard). A unit test covers `synthesise_review_from_chunks` directly.
async fn synthesise_review_from_index(
    state: &AnalyzerAppState,
    index_id: &str,
) -> Result<crate::core::ReviewReport, ApiError> {
    let chunks = state.search.get_chunks(index_id).await.map_err(|e| {
        ApiError::bad_gateway(format!("get_chunks({index_id}) for deep analysis: {e:#}"))
    })?;
    Ok(synthesise_review_from_chunks(&chunks))
}

/// Pure helper: aggregate a chunk corpus into a [`crate::core::ReviewReport`].
///
/// Why: extracted into a free function so it can be unit-tested without an
/// HTTP client.
/// What: groups chunks by file path, runs `compute_complexity_for` + smell
/// detection per file, builds `FileReview`s with `ReviewSource::NewFile`,
/// rolls up the worst grade and total smell count.
/// Test: `synthesise_review_from_chunks_groups_by_file`.
fn synthesise_review_from_chunks(chunks: &[crate::types::CodeChunk]) -> crate::core::ReviewReport {
    use crate::core::complexity::{compute_complexity_for, detect_smells};
    use crate::core::review::{FileReview, ReviewComplexity, ReviewSource, SmellHit};
    use crate::types::complexity::CodeSmell;
    use std::collections::BTreeMap;

    // Snake_case projection for code smells. Mirrors review.rs's
    // smell_projection, kept local to avoid widening the review.rs public
    // surface for this synth-only consumer.
    fn project(s: &CodeSmell) -> (&'static str, &'static str) {
        match s {
            CodeSmell::LongFunction { .. } => ("long_method", "medium"),
            CodeSmell::DeepNesting { .. } => ("deep_nesting", "high"),
            CodeSmell::TooManyParams { .. } => ("too_many_params", "medium"),
            CodeSmell::MissingDocstring => ("missing_docstring", "low"),
        }
    }

    let mut by_file: BTreeMap<String, Vec<&crate::types::CodeChunk>> = BTreeMap::new();
    for c in chunks {
        by_file.entry(c.file.clone()).or_default().push(c);
    }

    let mut files: Vec<FileReview> = Vec::with_capacity(by_file.len());
    let mut total_smells = 0usize;
    let mut total_lines = 0usize;
    for (path, group) in by_file {
        let joined: String = group
            .iter()
            .map(|c| c.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let lang = match path.rsplit('.').next().unwrap_or("") {
            "rs" => "rust",
            "ts" => "typescript",
            "tsx" => "tsx",
            "js" => "javascript",
            "jsx" => "jsx",
            "py" => "python",
            "go" => "go",
            "java" => "java",
            _ => "unknown",
        };
        let metrics = compute_complexity_for(&joined, lang);
        let raw_smells = detect_smells(&joined);
        let smells: Vec<SmellHit> = raw_smells
            .iter()
            .map(|s| {
                let (category, severity) = project(s);
                SmellHit {
                    category: category.to_string(),
                    line: group.first().map(|c| c.start_line as u32).unwrap_or(0),
                    severity: severity.to_string(),
                }
            })
            .collect();
        total_smells += smells.len();
        total_lines += joined.lines().count();
        files.push(FileReview {
            path,
            grade: metrics.grade,
            complexity: ReviewComplexity {
                cyclomatic: metrics.cyclomatic,
                cognitive: metrics.cognitive,
            },
            smells,
            recommendations: Vec::new(),
            source: ReviewSource::NewFile,
        });
    }

    let overall_grade = files
        .iter()
        .map(|f| f.grade)
        .max()
        .unwrap_or(crate::types::ComplexityGrade::A);
    let summary = format!(
        "{} file(s) synthesised from index corpus; {} smell(s); overall grade {}",
        files.len(),
        total_smells,
        overall_grade
    );

    crate::core::ReviewReport {
        files,
        overall_grade,
        changed_lines: total_lines,
        smell_count: total_smells,
        summary,
    }
}

/// Look up framework names recorded for `index_id` in the FactStore.
///
/// Why: framework detection runs as a separate setup step
/// (`record_frameworks`) and persists results as `(index_id, "uses_framework",
/// <name>)` triples. The deep-analysis path reads them back here so the LLM
/// prompt is framework-aware without having to re-scan the filesystem.
/// What: queries facts with `predicate = "uses_framework"` filtered by
/// `index_id` (via the `subject` column which the recorder uses as the index
/// id key), returning the deduplicated, sorted list of object values.
/// Test: covered transitively by the `deep_endpoint_*` tests; failures fall
/// back to an empty list rather than hard-erroring.
fn lookup_frameworks(state: &AnalyzerAppState, index_id: &str) -> Vec<String> {
    use std::collections::BTreeSet;
    let Ok(hits) = state
        .facts
        .query(Some(index_id), Some("uses_framework"), None)
    else {
        return Vec::new();
    };
    let mut names: BTreeSet<String> = BTreeSet::new();
    for fact in hits {
        names.insert(fact.object);
    }
    names.into_iter().collect()
}

/// Why: GitHub can push `pull_request` events to this endpoint so PRs are
/// reviewed automatically the moment they open or update — no CI step needed.
/// What: verifies the `X-Hub-Signature-256` HMAC against `GITHUB_WEBHOOK_SECRET`
/// (skipped with a warning when the secret is unset), checks the event is a
/// `pull_request` with an actionable `action`, extracts the PR coordinates,
/// spawns a background task to fetch+analyze+comment, and returns 202 Accepted
/// immediately so GitHub's delivery doesn't time out.
/// Test: `webhook_rejects_bad_signature` (401 path) and
/// `webhook_ignores_non_pr_event` (202 + no work) cover the guard rails.
async fn github_webhook_handler(
    State(state): State<Arc<AnalyzerAppState>>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    // 1. Signature verification (when a secret is configured). The secret
    //    comes from app state if set, otherwise from GITHUB_WEBHOOK_SECRET.
    let secret = state
        .webhook_secret
        .clone()
        .or_else(|| std::env::var("GITHUB_WEBHOOK_SECRET").ok())
        .filter(|s| !s.is_empty());
    match secret {
        Some(secret) => {
            let sig = headers
                .get("X-Hub-Signature-256")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !crate::core::verify_webhook_signature(&secret, &body, sig) {
                return Err(ApiError {
                    status: StatusCode::UNAUTHORIZED,
                    message: "X-Hub-Signature-256 verification failed".to_string(),
                });
            }
        }
        None => {
            tracing::warn!(
                "no webhook secret configured — skipping webhook signature verification"
            );
        }
    }

    // 2. Only handle pull_request events.
    let event = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if event != "pull_request" {
        // Acknowledge so GitHub stops retrying, but do no work.
        return Ok(StatusCode::ACCEPTED);
    }

    // 3. Parse the payload and filter to actionable actions.
    let payload: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("webhook body is not valid JSON: {e}")))?;
    let action = payload.get("action").and_then(|v| v.as_str()).unwrap_or("");
    if !matches!(action, "opened" | "synchronize" | "reopened") {
        return Ok(StatusCode::ACCEPTED);
    }

    // 4. Extract PR coordinates.
    let pr = payload
        .get("pull_request")
        .and_then(|p| p.get("number"))
        .and_then(|n| n.as_u64());
    let owner = payload
        .get("repository")
        .and_then(|r| r.get("owner"))
        .and_then(|o| o.get("login"))
        .and_then(|l| l.as_str())
        .map(str::to_owned);
    let repo = payload
        .get("repository")
        .and_then(|r| r.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_owned);
    let head_sha = payload
        .get("pull_request")
        .and_then(|p| p.get("head"))
        .and_then(|h| h.get("sha"))
        .and_then(|s| s.as_str())
        .unwrap_or("unknown")
        .to_string();

    let (Some(pr), Some(owner), Some(repo)) = (pr, owner, repo) else {
        return Err(ApiError::bad_request(
            "webhook payload missing pull_request.number or repository owner/name",
        ));
    };

    // 5. Spawn the analysis off the request path so GitHub gets a fast 202.
    let search = state.search.clone();
    tokio::spawn(async move {
        if let Err(e) = process_pr_webhook(search, &owner, &repo, pr, &head_sha).await {
            tracing::warn!("github webhook PR {owner}/{repo}#{pr} processing failed: {e:#}");
        }
    });

    Ok(StatusCode::ACCEPTED)
}

/// Background worker for an accepted PR webhook: fetch the diff, run the
/// review, and post a comment.
///
/// Why: keeps the webhook handler's response path fast — all the slow I/O
/// (GitHub API, trusty-search) happens here in a spawned task.
/// What: requires `GITHUB_TOKEN`; uses `repo` itself as the trusty-search
/// index ID (the conventional 1:1 mapping). The `head_sha` is logged as a
/// cache/correlation key.
/// Test: covered indirectly — the webhook handler tests exercise the guard
/// rails; this function is only reached with a valid token + reachable search.
async fn process_pr_webhook(
    search: TrustySearchClient,
    owner: &str,
    repo: &str,
    pr: u64,
    head_sha: &str,
) -> Result<()> {
    let token = std::env::var("GITHUB_TOKEN")
        .map_err(|_| anyhow::anyhow!("GITHUB_TOKEN not set; cannot process webhook PR"))?;
    tracing::info!("processing webhook PR {owner}/{repo}#{pr} (head {head_sha})");
    // Why: this background task fetches a potentially large diff and posts a
    // comment — without timeouts it hangs indefinitely on a slow GitHub API,
    // leaking the spawned task for the lifetime of the process.
    // What: 30 s per-request + 5 s connect timeout, matching the pattern in
    // `review_github_pr_handler` and `TrustySearchClient`.
    let client = reqwest::ClientBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("reqwest ClientBuilder is infallible with valid config");
    let diff = crate::core::fetch_pr_diff(&client, owner, repo, pr, &token).await?;
    let report = crate::core::analyze_diff_with_client(&diff, &search, repo).await?;
    let markdown = crate::core::format_review_as_markdown(&report);
    crate::core::post_pr_comment(&client, owner, repo, pr, &markdown, &token).await?;
    tracing::info!("posted webhook review comment to {owner}/{repo}#{pr}");
    Ok(())
}

async fn fetch_chunks(
    state: &AnalyzerAppState,
    id: &str,
) -> Result<Vec<crate::types::CodeChunk>, ApiError> {
    state.search.get_chunks(id).await.map_err(|e| {
        tracing::warn!("get_chunks({id}) failed: {e:#}");
        ApiError::bad_gateway(format!("get_chunks({id}): {e:#}"))
    })
}

#[derive(Deserialize)]
pub struct FactQueryParams {
    pub subject: Option<String>,
    pub predicate: Option<String>,
    pub object: Option<String>,
}

async fn list_facts(
    State(state): State<Arc<AnalyzerAppState>>,
    Query(p): Query<FactQueryParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Why: `FactStore::query` opens a synchronous redb read transaction. Even
    // though reads use `begin_read()`, redb serialises read-transaction
    // *acquisition* against any in-flight write commit; calling it directly
    // on the tokio runtime worker thread stalled the executor whenever an
    // `upsert_fact` was mid-commit, producing the ~900ms p99 spike seen in
    // issue #67 while p50 stayed at 0.25ms.
    // What: move the blocking redb call onto the blocking pool via
    // `spawn_blocking` so the async worker stays responsive and concurrent
    // requests don't pile up behind a single slow read.
    // Test: covered by the existing `upsert_then_list_facts_round_trip` (the
    // round-trip still works); the latency improvement is observable under
    // concurrent load (not asserted in unit tests).
    let facts = state.facts.clone();
    let hits = tokio::task::spawn_blocking(move || {
        facts.query(
            p.subject.as_deref(),
            p.predicate.as_deref(),
            p.object.as_deref(),
        )
    })
    .await
    .map_err(|e| ApiError::internal(format!("query facts task panicked: {e}")))?
    .map_err(|e| ApiError::internal(format!("query facts: {e:#}")))?;
    let count = hits.len();
    Ok(Json(serde_json::json!({ "facts": hits, "count": count })))
}

#[derive(Deserialize)]
pub struct UpsertFactRequest {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub index_id: String,
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    #[serde(default)]
    pub provenance: Vec<String>,
}

fn default_confidence() -> f32 {
    1.0
}

async fn upsert_fact(
    State(state): State<Arc<AnalyzerAppState>>,
    Json(req): Json<UpsertFactRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let subject = req.subject.clone();
    let predicate = req.predicate.clone();
    let mut fact = new_fact(req.subject, req.predicate, req.object, req.index_id);
    fact.confidence = req.confidence.clamp(0.0, 1.0);
    fact.provenance = req.provenance;
    let id = fact.id;
    // Why: redb write transactions block the calling thread for the entire
    // commit fsync. Holding the tokio worker hostage starves every other
    // task on that worker (the same root cause that produced the #67 p99
    // spike for `list_facts`). Pushing the write to the blocking pool keeps
    // the async runtime responsive.
    // What: clone the Arc-backed store, run the upsert under `spawn_blocking`,
    // and re-raise both join errors and store errors as 500s.
    // Test: covered by `upsert_then_list_facts_round_trip`.
    let facts = state.facts.clone();
    tokio::task::spawn_blocking(move || facts.upsert(fact))
        .await
        .map_err(|e| ApiError::internal(format!("upsert fact task panicked: {e}")))?
        .map_err(|e| ApiError::internal(format!("upsert fact: {e:#}")))?;
    state.emit(AnalyzerEvent::FactUpserted { subject, predicate });
    Ok(Json(serde_json::json!({ "id": id, "upserted": true })))
}

async fn delete_fact(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<u64>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // Why: same blocking-redb concern as `upsert_fact` — `Database::delete`
    // opens a write transaction and fsyncs on commit. Running it directly
    // on the async runtime worker risked starving other handlers.
    // What: dispatch to the blocking pool via `spawn_blocking`.
    // Test: covered transitively by the facts integration tests.
    let facts = state.facts.clone();
    let removed = tokio::task::spawn_blocking(move || facts.delete(id))
        .await
        .map_err(|e| ApiError::internal(format!("delete fact task panicked: {e}")))?
        .map_err(|e| ApiError::internal(format!("delete fact: {e:#}")))?;
    if removed {
        state.emit(AnalyzerEvent::FactDeleted { id: id.to_string() });
    }
    Ok(Json(serde_json::json!({ "id": id, "removed": removed })))
}

/// Re-export so the binary can construct facts via the same path.
pub use crate::types::FactRecord as PublicFactRecord;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{Method, Request};
    use tempfile::TempDir;
    use tower::ServiceExt;

    fn make_state() -> (AnalyzerAppState, TempDir) {
        let tmp = TempDir::new().unwrap();
        let facts = FactStore::open(&tmp.path().join("facts.redb")).unwrap();
        let search = TrustySearchClient::new("http://127.0.0.1:1");
        (AnalyzerAppState::new(search, facts), tmp)
    }

    async fn json_get(app: Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let value = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, value)
    }

    #[tokio::test]
    async fn health_degraded_when_search_unreachable() {
        // The stub search client points at port 1 (nothing listening).
        // Expect: 503 SERVICE_UNAVAILABLE, status == "degraded",
        // search_reachable == false.
        let (state, _tmp) = make_state();
        let app = build_router(state);
        let (status, body) = json_get(app, "/health").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["status"], "degraded");
        assert_eq!(body["search_reachable"], false);
    }

    #[tokio::test]
    async fn health_response_includes_version() {
        let (state, _tmp) = make_state();
        let app = build_router(state);
        let (_status, body) = json_get(app, "/health").await;
        // Version is always present regardless of search reachability.
        assert!(body["version"].is_string());
        assert!(!body["version"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn sse_subscriber_receives_emitted_event() {
        // Why: confirms the broadcast wiring is correct end-to-end —
        // subscribe via state.events, emit an event, and verify the
        // receiver gets the same payload.
        let (state, _tmp) = make_state();
        let mut rx = state.events.subscribe();
        state.emit(AnalyzerEvent::FactUpserted {
            subject: "fn auth".into(),
            predicate: "uses".into(),
        });
        let evt = rx
            .recv()
            .await
            .expect("subscriber should receive emitted event");
        match evt {
            AnalyzerEvent::FactUpserted { subject, predicate } => {
                assert_eq!(subject, "fn auth");
                assert_eq!(predicate, "uses");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn sse_route_returns_event_stream_content_type() {
        // Why: routes should advertise text/event-stream so browsers /
        // clients negotiate the SSE protocol correctly.
        let (state, _tmp) = make_state();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/sse")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.starts_with("text/event-stream"), "got {ct}");
    }

    #[test]
    fn run_diagnostics_blocking_skips_unknown_languages() {
        // Why: a file with no recognized extension must not crash the
        // diagnostics pipeline; it should simply be skipped.
        let mut by_file = HashMap::new();
        by_file.insert("notes.txt".to_string(), "hello world".to_string());
        let diags = run_diagnostics_blocking(by_file, None, None);
        assert!(diags.is_empty());
    }

    #[test]
    fn run_diagnostics_blocking_respects_language_filter() {
        // A Rust file filtered to `python` yields nothing even if clippy is
        // installed, because the language filter excludes it.
        let mut by_file = HashMap::new();
        by_file.insert("main.rs".to_string(), "fn main() {}".to_string());
        let diags = run_diagnostics_blocking(by_file, Some("python".to_string()), None);
        assert!(diags.is_empty());
    }

    #[tokio::test]
    async fn diagnostics_endpoint_surfaces_search_failure_as_502() {
        // The stub search client is unreachable, so fetching the corpus fails
        // and the endpoint must return a 502 rather than panic.
        let (state, _tmp) = make_state();
        let app = build_router(state);
        let (status, _body) = json_get(app, "/indexes/demo/diagnostics").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn upsert_then_list_facts_round_trip() {
        let (state, _tmp) = make_state();
        let app = build_router(state);

        let body = serde_json::json!({
            "subject": "fn search",
            "predicate": "implements",
            "object": "trait Searcher",
            "index_id": "test"
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/facts")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let (status, listing) = json_get(app, "/facts").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listing["count"], 1);
    }

    #[tokio::test]
    async fn scip_ingest_accepts_valid_index_and_stores_overlay() {
        use protobuf::{EnumOrUnknown, Message};
        use scip::types::{
            symbol_information::Kind as ScipKind, Document, Index, Occurrence, SymbolInformation,
        };

        let (state, _tmp) = make_state();
        let overlays = state.scip_overlays.clone();
        let app = build_router(state);

        // Build a one-symbol SCIP index.
        let mut sym = SymbolInformation::new();
        sym.symbol = "rust . . hello().".into();
        sym.kind = EnumOrUnknown::new(ScipKind::Function);
        sym.display_name = "hello".into();
        let mut occ = Occurrence::new();
        occ.symbol = sym.symbol.clone();
        occ.symbol_roles = 0x1;
        occ.range = vec![1, 0, 5];
        let mut doc = Document::new();
        doc.relative_path = "src/lib.rs".into();
        doc.language = "rust".into();
        doc.symbols.push(sym);
        doc.occurrences.push(occ);
        let mut index = Index::new();
        index.documents.push(doc);
        let bytes = index.write_to_bytes().expect("encode scip index");

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/indexes/myidx/scip")
                    .header("content-type", "application/octet-stream")
                    .body(Body::from(bytes))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["index_id"], "myidx");
        assert_eq!(parsed["documents"], 1);
        assert_eq!(parsed["kg_nodes"], 1);

        // The overlay should be persisted in state.
        let overlays = overlays.read().await;
        let g = overlays.get("myidx").expect("overlay stored");
        assert_eq!(g.node_count(), 1);
        assert_eq!(g.nodes[0].name, "hello");
    }

    #[tokio::test]
    async fn scip_ingest_rejects_garbage_bytes() {
        let (state, _tmp) = make_state();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/indexes/x/scip")
                    .header("content-type", "application/octet-stream")
                    .body(Body::from(vec![0xFF, 0xFF, 0xFF, 0xFF]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn review_endpoint_requires_index_id() {
        // Why: review is backed by trusty-search and needs an index to query;
        // POSTing without ?index_id= must fail fast with 400 before any work.
        let (state, _tmp) = make_state();
        let app = build_router(state);
        let diff = "+++ b/src/foo.rs\n@@ -0,0 +1,2 @@\n+/// doc\n+fn f() {}\n";
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/review")
                    .header("content-type", "text/x-patch")
                    .body(Body::from(diff))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn review_endpoint_surfaces_search_failure_as_502() {
        // With index_id supplied but the search daemon down (stub at port 1),
        // the chunk fetch fails and the endpoint reports 502 BAD_GATEWAY.
        let (state, _tmp) = make_state();
        let app = build_router(state);
        let diff = "+++ b/src/foo.rs\n@@ -0,0 +1,2 @@\n+/// doc\n+fn f() {}\n";
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/review?index_id=my-idx")
                    .header("content-type", "text/x-patch")
                    .body(Body::from(diff))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn review_endpoint_rejects_malformed_diff() {
        // A malformed hunk header is caught during parse, before any search
        // call, so the endpoint returns 400 even though index_id is present.
        let (state, _tmp) = make_state();
        let app = build_router(state);
        let diff = "+++ b/x.rs\n@@ totally bogus @@\n+fn x() {}\n";
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/review?index_id=my-idx")
                    .header("content-type", "text/x-patch")
                    .body(Body::from(diff))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn deep_endpoint_requires_index_id() {
        // POST /analyze/deep with an empty `index_id` must 400 before any
        // network or LLM work.
        let (state, _tmp) = make_state();
        let state = state.with_api_key(Some("test-key".into()));
        let app = build_router(state);
        let body = serde_json::json!({ "index_id": "" }).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/analyze/deep")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn deep_endpoint_requires_api_key() {
        // POST /analyze/deep with no API key configured must 400 — the daemon
        // can't run the LLM call without a key.
        let (state, _tmp) = make_state();
        let state = state.with_api_key(None);
        let app = build_router(state);
        let body = serde_json::json!({ "index_id": "my-idx" }).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/analyze/deep")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn synthesise_review_from_chunks_groups_by_file() {
        // Synthesis should produce one FileReview per distinct chunk.file,
        // with NewFile source and no spurious recommendations.
        use crate::core::review::ReviewSource;
        use crate::types::CodeChunk;
        let chunks = vec![
            CodeChunk {
                id: "a:1:5".into(),
                file: "src/a.rs".into(),
                start_line: 1,
                end_line: 5,
                content: "fn a() {}".into(),
                ..Default::default()
            },
            CodeChunk {
                id: "a:10:20".into(),
                file: "src/a.rs".into(),
                start_line: 10,
                end_line: 20,
                content: "fn aa() {}".into(),
                ..Default::default()
            },
            CodeChunk {
                id: "b:1:3".into(),
                file: "src/b.rs".into(),
                start_line: 1,
                end_line: 3,
                content: "fn b() {}".into(),
                ..Default::default()
            },
        ];
        let report = synthesise_review_from_chunks(&chunks);
        assert_eq!(report.files.len(), 2);
        let paths: Vec<&str> = report.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"src/a.rs"));
        assert!(paths.contains(&"src/b.rs"));
        for f in &report.files {
            assert_eq!(f.source, ReviewSource::NewFile);
            assert!(f.recommendations.is_empty());
        }
    }

    #[test]
    fn synthesise_review_from_chunks_empty_corpus_is_grade_a() {
        let report = synthesise_review_from_chunks(&[]);
        assert!(report.files.is_empty());
        assert_eq!(report.overall_grade, crate::types::ComplexityGrade::A);
        assert_eq!(report.smell_count, 0);
    }

    #[test]
    fn lookup_frameworks_reads_stored_facts() {
        // record_frameworks → lookup_frameworks round-trip: the deep handler
        // must be able to read back the framework names that registry.rs
        // recorded under the (`index_id`, `uses_framework`, ...) triple.
        use crate::core::facts::new_fact;
        let (state, _tmp) = make_state();
        for fw in ["React", "Next.js"] {
            let f = new_fact(
                "my-idx".to_string(),
                "uses_framework".to_string(),
                fw.to_string(),
                "my-idx".to_string(),
            );
            state.facts.upsert(f).unwrap();
        }
        let mut got = lookup_frameworks(&state, "my-idx");
        got.sort();
        assert_eq!(got, vec!["Next.js".to_string(), "React".to_string()]);
    }

    #[tokio::test]
    async fn webhook_ignores_non_pr_event() {
        // A `push` event is acknowledged with 202 but triggers no analysis.
        // No webhook secret injected → signature verification is skipped, so
        // the test is hermetic regardless of ambient env.
        let (state, _tmp) = make_state();
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/webhooks/github")
                    .header("X-GitHub-Event", "push")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn webhook_ignores_non_actionable_pr_action() {
        // A `pull_request` event with action `closed` is acknowledged but
        // does not trigger a review. No webhook secret → verification skipped.
        let (state, _tmp) = make_state();
        let app = build_router(state);
        let body = serde_json::json!({ "action": "closed" }).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/webhooks/github")
                    .header("X-GitHub-Event", "pull_request")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn webhook_rejects_bad_signature() {
        // With a secret injected via app state, a wrong signature must 401.
        // Injecting through state (not env) keeps the test hermetic and
        // free of cross-test env-var races.
        let (state, _tmp) = make_state();
        let state = state.with_webhook_secret(Some("test-secret".to_string()));
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/webhooks/github")
                    .header("X-GitHub-Event", "pull_request")
                    .header("X-Hub-Signature-256", "sha256=deadbeef")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webhook_accepts_valid_signature() {
        // With a secret injected and a correctly-computed signature, the
        // request passes verification (and is then 400 for missing PR data).
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let (state, _tmp) = make_state();
        let state = state.with_webhook_secret(Some("test-secret".to_string()));
        let app = build_router(state);
        let body = serde_json::json!({ "action": "closed" }).to_string();
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test-secret").unwrap();
        mac.update(body.as_bytes());
        let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/webhooks/github")
                    .header("X-GitHub-Event", "pull_request")
                    .header("X-Hub-Signature-256", &sig)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Valid signature → past auth; `closed` action → 202 (ignored).
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn webhook_rejects_malformed_pr_payload() {
        // pull_request + opened, but no PR number / repo → 400.
        // No webhook secret → verification skipped.
        let (state, _tmp) = make_state();
        let app = build_router(state);
        let body = serde_json::json!({ "action": "opened" }).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/webhooks/github")
                    .header("X-GitHub-Event", "pull_request")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn list_indexes_proxies_failure_to_502() {
        // Search daemon at port 1 won't answer — proxy should surface 502.
        let (state, _tmp) = make_state();
        let app = build_router(state);
        let (status, _) = json_get(app, "/indexes").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }
}
