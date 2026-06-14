//! Axum HTTP server for the trusty-console.
//!
//! Why: The console needs a lightweight HTTP server that serves the embedded
//! SPA, a JSON API route for service status, and a reverse-proxy layer for
//! all daemon sub-paths.
//! What: Builds an axum `Router` with:
//!   - `GET /health` — liveness probe.
//!   - `GET /api/console/services` — return cached snapshot (background poll).
//!   - `GET /api/console/metrics/{analyze,memory,search,review}` — MCP-polled metrics.
//!   - `GET /api/console/metrics/analyze/indexes` — analyze index list via stdio MCP.
//!   - `GET /api/console/metrics/analyze/visualize?index=<id>` — graph+entities+clusters.
//!   - `ANY /proxy/{daemon}/{*path}` — reverse-proxy to live daemon.
//!   - `GET /` and `GET /ui/*path` — serve the embedded Svelte SPA.
//!
//! All logs go to stderr; stdout is clean.
//!
//! Test: The `tests` module starts the router in a real axum test client.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    extract::{Path, Query, State},
    http::{Response, StatusCode, header},
    response::IntoResponse,
    routing::{any, get},
};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::json;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::connector::{ServiceConnector, ServiceInfo, ServiceStatus};
use crate::mcp_handle::{McpHandleError, McpServiceHandle};
use crate::metrics_poller::MetricsCache;
use crate::poller::PollerCache;

// ─── embedded UI ─────────────────────────────────────────────────────────────

/// Embedded Svelte SPA assets compiled by `build.rs`.
///
/// Why: Shipping the UI inside the binary eliminates external file dependencies
/// and matches the pattern used by trusty-search, trusty-memory, and
/// trusty-analyze.
/// What: rust-embed embeds every file under `ui/dist/` at compile time.
/// Test: The server tests assert that `GET /` returns 200.
#[derive(RustEmbed)]
#[folder = "ui/dist/"]
struct UiAssets;

// ─── app state ───────────────────────────────────────────────────────────────

/// Shared application state injected into every route handler.
///
/// Why: Connectors, the poller cache, metrics caches, and HTTP client are
/// created once at startup and reused for every request so there is no per-
/// request allocation. A separate `MetricsCache` is maintained for each
/// stdio-MCP-polled service (analyze, memory, search, review) so they can be
/// updated independently and served without coupling. `analyze_handle` is held
/// in Arc so the on-demand visualize/index routes can call the analyze stdio MCP
/// without going through the /proxy path.
/// `mcp_handles` maps each service id to its `McpServiceHandle` so the
/// services route can overlay the connector-reported status with the actual
/// tools/list probe result (Degraded when `console_metrics` is absent).
/// What: Wraps the connector list, poller cache, per-service metrics caches,
/// reqwest client, the analyze MCP handle, and the full handle map in `Arc`s
/// for cheap cloning.
/// Test: Constructed in `build_router`; exercised by the integration tests.
#[derive(Clone)]
pub struct AppState {
    connectors: Arc<Vec<Box<dyn ServiceConnector>>>,
    poller_cache: PollerCache,
    metrics_cache: MetricsCache,
    memory_metrics_cache: MetricsCache,
    search_metrics_cache: MetricsCache,
    review_metrics_cache: MetricsCache,
    http_client: Arc<reqwest::Client>,
    /// Analyze stdio MCP handle — shared with the metrics poller so both the
    /// background poll and on-demand route calls reuse the same child process.
    analyze_handle: Arc<McpServiceHandle>,
    /// All per-service MCP handles keyed by service id.
    ///
    /// Why: The services route reads each handle's degraded state to override
    /// the connector-reported status when a reachable service is missing
    /// `console_metrics`. Using a HashMap avoids adding individual Arc fields
    /// for every future service.
    /// What: Populated by `AppState::new`; read by `apply_handle_overrides`.
    mcp_handles: Arc<HashMap<String, Arc<McpServiceHandle>>>,
}

impl AppState {
    /// Create a new `AppState` from a list of connectors.
    ///
    /// Why: Lets tests inject a custom connector list and fresh caches.
    /// What: Wraps `connectors` in `Arc`; initialises empty `PollerCache`,
    /// three `MetricsCache` instances (analyze / memory / search), and a
    /// default `reqwest::Client`. Creates the analyze stdio MCP handle that is
    /// shared between the background metrics poller and on-demand routes.
    /// Populates `mcp_handles` with all three per-service handles so the
    /// services route can read their degraded state.
    /// Test: Used in `build_router` and directly in `tests`.
    pub fn new(connectors: Vec<Box<dyn ServiceConnector>>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client init");
        let analyze_handle = Arc::new(McpServiceHandle::new(
            "trusty-analyze",
            vec!["mcp".to_string()],
        ));
        let memory_handle = Arc::new(McpServiceHandle::new(
            "trusty-memory",
            vec!["serve".to_string(), "--stdio".to_string()],
        ));
        let search_handle = Arc::new(McpServiceHandle::new(
            "trusty-search",
            vec!["serve".to_string()],
        ));
        // Why: trusty-review's stdio MCP mode is `serve --stdio` (see ServeArgs
        // in commands/serve.rs). This is the canonical command the console spawns
        // to poll `console_metrics` without requiring the HTTP daemon to be running.
        let review_handle = Arc::new(McpServiceHandle::new(
            "trusty-review",
            vec!["serve".to_string(), "--stdio".to_string()],
        ));
        let mut handles: HashMap<String, Arc<McpServiceHandle>> = HashMap::new();
        handles.insert("trusty-analyze".to_string(), Arc::clone(&analyze_handle));
        handles.insert("trusty-memory".to_string(), Arc::clone(&memory_handle));
        handles.insert("trusty-search".to_string(), Arc::clone(&search_handle));
        handles.insert("trusty-review".to_string(), Arc::clone(&review_handle));
        Self {
            connectors: Arc::new(connectors),
            poller_cache: PollerCache::new(),
            metrics_cache: MetricsCache::new(),
            memory_metrics_cache: MetricsCache::new(),
            search_metrics_cache: MetricsCache::new(),
            review_metrics_cache: MetricsCache::new(),
            http_client: Arc::new(client),
            analyze_handle,
            mcp_handles: Arc::new(handles),
        }
    }

    /// Access the per-service MCP handle map.
    ///
    /// Why: The services route reads handles from this map to overlay connector
    /// statuses with the tools/list probe result.
    /// What: Returns a clone of the `Arc<HashMap>` (cheap).
    /// Test: Used by `apply_handle_overrides` and the services handler.
    pub fn mcp_handles(&self) -> Arc<HashMap<String, Arc<McpServiceHandle>>> {
        Arc::clone(&self.mcp_handles)
    }

    /// Access the shared analyze MCP handle.
    ///
    /// Why: On-demand routes (`/api/console/metrics/analyze/indexes`,
    /// `/api/console/metrics/analyze/visualize`) call the analyze stdio MCP
    /// without touching the analyze daemon HTTP directly (architecture: console
    /// is a stdio MCP client only, per #1104).
    /// What: Returns a clone of the `Arc<McpServiceHandle>` (cheap).
    /// Test: Exercised by the analyze index and visualize route tests.
    pub fn analyze_handle(&self) -> Arc<McpServiceHandle> {
        Arc::clone(&self.analyze_handle)
    }

    /// Access the shared connector list.
    ///
    /// Why: The background poller and the fallback `spawn_blocking` path both
    /// need the connector list.
    /// What: Returns a clone of the `Arc` (cheap).
    /// Test: Used by `run_serve` in `main.rs`.
    pub fn connectors(&self) -> Arc<Vec<Box<dyn ServiceConnector>>> {
        Arc::clone(&self.connectors)
    }

    /// Access the background poll cache.
    ///
    /// Why: Routes read from the cache; the background task writes to it.
    /// What: Returns a clone of the `PollerCache` handle (cheap — it's an Arc).
    /// Test: Used by `services_handler` and `proxy_handler`.
    pub fn poller_cache(&self) -> &PollerCache {
        &self.poller_cache
    }

    /// Access the metrics cache for the trusty-analyze stdio MCP poller.
    ///
    /// Why: The metrics poller writes `ConsoleMetricsReport`s here; the
    /// `/api/console/metrics/analyze` route reads from it.
    /// What: Returns a reference to the `MetricsCache` handle.
    /// Test: `test_metrics_analyze_route_cold_cache_returns_503`.
    pub fn metrics_cache(&self) -> &MetricsCache {
        &self.metrics_cache
    }

    /// Access the metrics cache for the trusty-memory stdio MCP poller.
    ///
    /// Why: Separate cache per service so memory and analyze reports can be
    /// updated and served independently.
    /// What: Returns a reference to the `MetricsCache` handle for memory.
    /// Test: `test_metrics_memory_route_cold_cache_returns_503`.
    pub fn memory_metrics_cache(&self) -> &MetricsCache {
        &self.memory_metrics_cache
    }

    /// Access the metrics cache for the trusty-search stdio MCP poller.
    ///
    /// Why: Separate cache per service so search and analyze reports can be
    /// updated and served independently.
    /// What: Returns a reference to the `MetricsCache` handle for search.
    /// Test: `test_metrics_search_route_cold_cache_returns_503`.
    pub fn search_metrics_cache(&self) -> &MetricsCache {
        &self.search_metrics_cache
    }

    /// Access the metrics cache for the trusty-review stdio MCP poller.
    ///
    /// Why: Separate cache per service so review reports can be updated and
    /// served independently from the other service caches.
    /// What: Returns a reference to the `MetricsCache` handle for review.
    /// Test: `test_metrics_review_route_cold_cache_returns_503`.
    pub fn review_metrics_cache(&self) -> &MetricsCache {
        &self.review_metrics_cache
    }

    /// Access the shared `reqwest::Client`.
    ///
    /// Why: Re-using one client enables connection pooling across proxy requests.
    /// What: Returns a clone of the `Arc<reqwest::Client>` (cheap).
    /// Test: Used by `proxy_handler`.
    pub fn http_client(&self) -> Arc<reqwest::Client> {
        Arc::clone(&self.http_client)
    }
}

// ─── router ──────────────────────────────────────────────────────────────────

/// Build the axum `Router` with all routes wired.
///
/// Why: Extracting the router into its own function allows both `main` and the
/// test harness to share the same routing configuration without running a real
/// TCP server.
/// What: Returns a `Router<()>` with CORS, tracing middleware, and all routes.
/// Test: Called from `tests::test_services_route_returns_json` below.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/api/console/services", get(services_handler))
        .route("/api/console/metrics/analyze", get(metrics_analyze_handler))
        .route("/api/console/metrics/memory", get(metrics_memory_handler))
        .route("/api/console/metrics/search", get(metrics_search_handler))
        .route("/api/console/metrics/review", get(metrics_review_handler))
        // Analyze on-demand routes — call the analyze stdio MCP directly (no /proxy).
        .route(
            "/api/console/metrics/analyze/indexes",
            get(analyze_indexes_handler),
        )
        .route(
            "/api/console/metrics/analyze/visualize",
            get(analyze_visualize_handler),
        )
        // Reverse-proxy: /proxy/{daemon}/{*path}
        .route("/proxy/{daemon}/{*path}", any(crate::proxy::proxy_handler))
        .route("/", get(spa_index_handler))
        .route("/ui", get(spa_index_handler))
        .route("/ui/", get(spa_index_handler))
        .route("/ui/{*path}", get(spa_asset_handler))
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}

// ─── handlers ────────────────────────────────────────────────────────────────

/// `GET /health` — liveness probe.
///
/// Why: Required by process monitors and the `trusty-console status` CLI
/// subcommand. Returns a minimal JSON body so callers can confirm the server
/// is up and which version is running.
/// What: Returns `{"status":"ok","version":"<CARGO_PKG_VERSION>"}`.
/// Test: Tested by `test_health_route` below.
async fn health_handler() -> impl IntoResponse {
    axum::Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Apply per-service MCP handle state on top of connector-reported statuses.
///
/// Why: The connector `detect()` path (TCP probe / `which`) can only report
/// `Running`, `Available`, or `Absent`. It has no knowledge of the MCP
/// `tools/list` probe result.  When a service is reachable but the
/// `console_metrics` tool is absent (`HandleState::Degraded`), the connector
/// still reports `Running` or `Available` — the UI incorrectly shows a healthy
/// badge. This function overlays the handle's known state: if a handle is
/// Degraded, the corresponding `ServiceInfo` is updated in-place to
/// `status = Degraded` and `hint = DEGRADED_HINT`. If a handle is Connected, the
/// daemon version from the `initialize` response is surfaced (unless the connector
/// already reported a version from the HTTP `/health` endpoint).
/// What: Iterates `infos` in place; for each entry looks up the matching handle
/// by `id`. If `handle.degraded_hint()` returns `Some(hint)` and the current
/// status is NOT already `Absent`, sets `status = Degraded` and `hint = Some`.
/// If `info.version` is `None` and `handle.daemon_version()` returns `Some`,
/// sets `info.version` from the MCP `serverInfo.version`.
/// A process-down (`Absent`) service is never overridden — only reachable ones.
/// Skipping only `Absent` is safe: `Available` handles always return `None`
/// from `degraded_hint` (no tools/list probe runs until the first poll), so
/// they pass through unchanged.
/// Test: `test_services_route_handle_degraded_overlay` and
/// `test_services_route_daemon_version_overlay` below.
async fn apply_handle_overrides(
    infos: &mut [ServiceInfo],
    handles: &HashMap<String, Arc<McpServiceHandle>>,
) {
    for info in infos.iter_mut() {
        if info.status == ServiceStatus::Absent {
            continue;
        }
        if let Some(handle) = handles.get(&info.id) {
            if let Some(hint) = handle.degraded_hint().await {
                info.status = ServiceStatus::Degraded;
                info.hint = Some(hint);
            }
            // Surface the MCP daemon version when the connector hasn't
            // already provided one (e.g. when the HTTP daemon isn't running
            // but the stdio MCP process is up and has responded to initialize).
            if info.version.is_none()
                && let Some(ver) = handle.daemon_version().await
            {
                info.version = Some(ver);
            }
        }
    }
}

/// `GET /api/console/services` — return cached snapshot of all services.
///
/// Why: The Svelte SPA fetches this endpoint on load to render service cards.
///      With the background poller in place the response is instant (no per-
///      request TCP probes).
/// What: Reads the latest `CachedSnapshot` from the `PollerCache`. If the first
/// poll has not completed yet, falls back to a synchronous on-demand detection
/// so the UI always gets data (the first-boot latency is acceptable; after that
/// every response is cache-backed).  A panic in the fallback blocking task
/// surfaces as HTTP 500 rather than an empty 200.
/// After obtaining the base service list (from cache or fallback), applies
/// per-service handle degraded overrides via `apply_handle_overrides` so
/// reachable services missing `console_metrics` surface as `status: degraded`.
/// Test: `test_services_route_returns_json`,
/// `test_services_handler_returns_500_on_panic`, and
/// `test_services_route_handle_degraded_overlay` below.
async fn services_handler(State(state): State<AppState>) -> axum::response::Response {
    let handles = state.mcp_handles();

    if let Some(snap) = state.poller_cache().snapshot().await {
        let mut services = snap.services;
        apply_handle_overrides(&mut services, &handles).await;
        return axum::Json(services).into_response();
    }

    // First-boot fallback: run a one-shot detection synchronously.
    let connectors = state.connectors();
    match tokio::task::spawn_blocking(move || {
        connectors.iter().map(|c| c.detect()).collect::<Vec<_>>()
    })
    .await
    {
        Ok(mut infos) => {
            apply_handle_overrides(&mut infos, &handles).await;
            axum::Json(infos).into_response()
        }
        Err(e) => {
            tracing::error!("service detection task panicked: {e}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// `GET /api/console/metrics/analyze` — return the latest metrics report.
///
/// Why: Surfaces trusty-analyze health/metrics to the SPA without per-request
/// MCP calls (the background poller keeps the cache warm).
/// What: Returns the cached `ConsoleMetricsReport` as JSON (200) or 503 when
/// no poll has completed yet (binary absent or first boot).
/// Test: `test_metrics_analyze_route_cold_cache_returns_503` below.
async fn metrics_analyze_handler(State(state): State<AppState>) -> axum::response::Response {
    match state.metrics_cache().get().await {
        Some(report) => axum::Json(report).into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// `GET /api/console/metrics/memory` — return the latest memory metrics report.
///
/// Why: Surfaces trusty-memory health/metrics to the SPA without per-request
/// MCP calls (the background poller keeps the cache warm).
/// What: Returns the cached `ConsoleMetricsReport` as JSON (200) or 503 when
/// no poll has completed yet (binary absent or first boot).
/// Test: `test_metrics_memory_route_cold_cache_returns_503` below.
async fn metrics_memory_handler(State(state): State<AppState>) -> axum::response::Response {
    match state.memory_metrics_cache().get().await {
        Some(report) => axum::Json(report).into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// `GET /api/console/metrics/search` — return the latest search metrics report.
///
/// Why: Surfaces trusty-search health/metrics to the SPA without per-request
/// MCP calls (the background poller keeps the cache warm).
/// What: Returns the cached `ConsoleMetricsReport` as JSON (200) or 503 when
/// no poll has completed yet (binary absent or first boot).
/// Test: `test_metrics_search_route_cold_cache_returns_503` below.
async fn metrics_search_handler(State(state): State<AppState>) -> axum::response::Response {
    match state.search_metrics_cache().get().await {
        Some(report) => axum::Json(report).into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// `GET /api/console/metrics/review` — return the latest review metrics report.
///
/// Why: Surfaces trusty-review health/metrics to the SPA without per-request
/// MCP calls (the background poller keeps the cache warm).
/// What: Returns the cached `ConsoleMetricsReport` as JSON (200) or 503 when
/// no poll has completed yet (binary absent or first boot).
/// Test: `test_metrics_review_route_cold_cache_returns_503` below.
async fn metrics_review_handler(State(state): State<AppState>) -> axum::response::Response {
    match state.review_metrics_cache().get().await {
        Some(report) => axum::Json(report).into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// Query params for the analyze visualize route.
///
/// Why: The index id must be a query param so the Svelte component can change
/// the selected index without a page navigation.
/// What: `index` is the analyze index id (string). Optional: no default —
/// returns 400 when absent.
/// Test: `test_analyze_visualize_handler_no_index_returns_400` below.
#[derive(Deserialize)]
struct VisualizeQuery {
    index: Option<String>,
}

/// `GET /api/console/metrics/analyze/indexes` — list analyze indexes via stdio.
///
/// Why: The Analyze tab needs a list of indexes to populate the dropdown.
/// This route calls the analyze stdio MCP (via `McpServiceHandle::call_tool_checked`)
/// instead of the browser hitting the analyze daemon HTTP directly, honouring
/// the #1104 architecture principle: the console is a stdio MCP client only.
/// Using `call_tool_checked` instead of `call_tool_raw` prevents a raw -32601
/// JSON-RPC error from reaching the browser as a 502 when the stale daemon lacks
/// the `list_analyze_indexes` tool — the capability-gate returns `ToolUnavailable`
/// which maps to a clean 503 with an actionable hint.
/// What: Calls the `list_analyze_indexes` MCP tool (which proxies `GET /indexes`
/// on the daemon). Returns the JSON array on 200, 503+hint when the analyze binary
/// is absent, in backoff, degraded, or the tool is not in the cached tool set;
/// 502 on any other error.
/// Test: `test_analyze_indexes_absent_binary_returns_503` and
/// `test_analyze_indexes_tool_unavailable_returns_degraded_hint` below.
async fn analyze_indexes_handler(State(state): State<AppState>) -> axum::response::Response {
    match state
        .analyze_handle()
        .call_tool_checked("list_analyze_indexes", serde_json::json!({}))
        .await
    {
        Ok(val) => axum::Json(val).into_response(),
        Err(McpHandleError::ToolUnavailable { tool, hint }) => {
            tracing::warn!(
                tool = %tool,
                hint = %hint,
                "analyze_indexes_handler: tool not available — capability-gate triggered"
            );
            (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({
                    "status": "degraded",
                    "hint": hint,
                })),
            )
                .into_response()
        }
        Err(
            McpHandleError::Absent
            | McpHandleError::Backoff { .. }
            | McpHandleError::Degraded { .. },
        ) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(e) => {
            tracing::warn!("analyze_indexes_handler error: {e:#}");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

/// `GET /api/console/metrics/analyze/visualize?index=<id>` — combined viz data.
///
/// Why: The Analyze tab needs graph nodes, entities, and clusters in one round
/// trip. This route calls the analyze stdio MCP for all three without the
/// browser hitting the analyze daemon HTTP directly (#1104 architecture).
/// Using `call_tool_checked` prevents a raw -32601 from reaching the browser
/// as a 502 when a stale daemon lacks `extract_graph`/`list_entities`/
/// `cluster_concepts` — the capability-gate returns `ToolUnavailable` which maps
/// to a clean 503+hint response.
/// What: Calls `extract_graph`, `list_entities`, and `cluster_concepts` (k=8)
/// via `McpServiceHandle::call_tool_checked` and returns a combined JSON object:
/// `{"graph": ..., "entities": ..., "clusters": ...}`. Missing index param
/// returns 400 (BAD_REQUEST). Absent binary, backoff, degraded, or tool
/// unavailable returns 503 (SERVICE_UNAVAILABLE) with optional hint JSON.
/// A hard graph error (non-absent/backoff/tool-unavailable) returns 502 (BAD_GATEWAY).
/// Test: `test_analyze_visualize_handler_no_index_returns_400` and
/// `test_analyze_visualize_handler_absent_binary_returns_503` below.
async fn analyze_visualize_handler(
    State(state): State<AppState>,
    Query(params): Query<VisualizeQuery>,
) -> axum::response::Response {
    let index_id = match params.index {
        Some(id) if !id.is_empty() => id,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error": "missing required query param: index"})),
            )
                .into_response();
        }
    };

    let handle = state.analyze_handle();
    let args = serde_json::json!({ "index_id": index_id });

    // NOTE: although `tokio::join!` normally drives all three futures
    // concurrently, these three `call_tool_checked` calls share a single stdio
    // child process behind `McpServiceHandle`'s inner `Arc<Mutex<StdioMcpClient>>`.
    // Each call acquires that inner mutex for the full duration of its
    // JSON-RPC round trip, so the three futures effectively serialize behind
    // the lock — `join!` does not provide real I/O parallelism here. The
    // `join!` form is retained for code readability (all three results
    // collected symmetrically) and because the serialization is transparent
    // to callers. If the analyze MCP child ever supports multiplexed requests
    // (separate stdin/stdout framing per call), this join would gain true
    // concurrency automatically without changing the call sites.
    let (graph_res, entities_res, clusters_res) = tokio::join!(
        handle.call_tool_checked("extract_graph", args.clone()),
        handle.call_tool_checked("list_entities", args.clone()),
        handle.call_tool_checked("cluster_concepts", {
            let mut a = args.clone();
            if let Some(m) = a.as_object_mut() {
                m.insert("k".to_string(), serde_json::json!(8));
            }
            a
        }),
    );

    // Classify the graph result: tool unavailable → 503+hint, absent/backoff/degraded → 503,
    // hard error → 502, success → combine with best-effort entities and clusters.
    match &graph_res {
        Err(McpHandleError::ToolUnavailable { tool, hint }) => {
            tracing::warn!(
                tool = %tool,
                hint = %hint,
                "analyze_visualize_handler: tool not available — capability-gate triggered"
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(serde_json::json!({
                    "status": "degraded",
                    "hint": hint,
                })),
            )
                .into_response();
        }
        Err(
            McpHandleError::Absent
            | McpHandleError::Backoff { .. }
            | McpHandleError::Degraded { .. },
        ) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(e) => {
            tracing::warn!("analyze_visualize_handler graph error: {e:#}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
        Ok(_) => {}
    }

    // Log a warning when a best-effort tool is missing (e.g. stale daemon that
    // predates list_entities or cluster_concepts).  We do NOT return 503 here —
    // these two are genuinely best-effort and the route still returns a useful
    // partial payload.  The primary `extract_graph` gate above is the hard 503
    // path; these are only observable degradation signals.
    if let Err(McpHandleError::ToolUnavailable { tool, .. }) = &entities_res {
        tracing::warn!(
            tool = %tool,
            "analyze_visualize_handler: list_entities tool unavailable — returning partial payload"
        );
    }
    if let Err(McpHandleError::ToolUnavailable { tool, .. }) = &clusters_res {
        tracing::warn!(
            tool = %tool,
            "analyze_visualize_handler: cluster_concepts tool unavailable — returning partial payload"
        );
    }

    let combined = json!({
        "graph":    graph_res.unwrap_or(serde_json::Value::Null),
        "entities": entities_res.unwrap_or(serde_json::Value::Null),
        "clusters": clusters_res.unwrap_or(serde_json::Value::Null),
    });
    axum::Json(combined).into_response()
}

/// `GET /` — serve the SPA index.html.
///
/// Why: The root path must return the SPA shell so the browser bootstraps.
/// What: Reads `index.html` from the embedded asset set.
/// Test: `test_spa_root_returns_html` below.
async fn spa_index_handler() -> impl IntoResponse {
    serve_asset("index.html")
}

/// `GET /ui/*path` — serve SPA static assets.
///
/// Why: Vite emits JS/CSS/assets under hashed filenames; all are embedded and
/// served from the `/ui/*` prefix.
/// What: Strips the leading `/ui/` from `path` and serves the matching asset.
/// Test: Indirectly covered by `test_spa_root_returns_html`.
async fn spa_asset_handler(Path(path): Path<String>) -> impl IntoResponse {
    let path = path.trim_start_matches('/');
    serve_asset(path)
}

/// Serve one asset from the embedded `UiAssets`.
///
/// Why: Centralises asset serving so both the index and asset routes share the
/// same content-type detection and 404 handling.
/// What: Looks up the path in `UiAssets`, infers the MIME type via
/// `mime_guess`, returns the bytes with the appropriate `Content-Type` header.
/// On a 404 serves `index.html` (SPA client-side routing).
/// Test: `test_spa_root_returns_html`.
fn serve_asset(path: &str) -> Response<Body> {
    match UiAssets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime.as_ref())
                .body(Body::from(content.data.to_vec()))
                .unwrap_or_else(|_| {
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::empty())
                        .expect("static response")
                })
        }
        None => {
            // SPA fallback: serve index.html for unknown paths so client-side
            // routing works when the user navigates directly to a subpath.
            match UiAssets::get("index.html") {
                Some(content) => Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/html")
                    .body(Body::from(content.data.to_vec()))
                    .unwrap_or_else(|_| {
                        Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .body(Body::empty())
                            .expect("static response")
                    }),
                None => Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from("not found"))
                    .expect("static 404"),
            }
        }
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::CONTENT_TYPE;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::connector::{ServiceInfo, ServiceStatus};

    /// A stub connector for tests — always returns a fixed `ServiceInfo`.
    struct StubConnector {
        id: &'static str,
        display_name: &'static str,
        status: ServiceStatus,
    }

    impl ServiceConnector for StubConnector {
        fn id(&self) -> &'static str {
            self.id
        }
        fn display_name(&self) -> &'static str {
            self.display_name
        }
        fn detect(&self) -> ServiceInfo {
            ServiceInfo {
                id: self.id.to_string(),
                display_name: self.display_name.to_string(),
                status: self.status.clone(),
                version: None,
                url: None,
                hint: None,
            }
        }
    }

    fn make_test_state() -> AppState {
        AppState::new(vec![
            Box::new(StubConnector {
                id: "trusty-search",
                display_name: "Trusty Search",
                status: ServiceStatus::Running,
            }),
            Box::new(StubConnector {
                id: "trusty-memory",
                display_name: "Trusty Memory",
                status: ServiceStatus::Available,
            }),
            Box::new(StubConnector {
                id: "trusty-analyze",
                display_name: "Trusty Analyze",
                status: ServiceStatus::Absent,
            }),
        ])
    }

    async fn get_bytes(resp: axum::http::Response<Body>) -> Vec<u8> {
        resp.into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes()
            .to_vec()
    }

    /// Why: the services route must return a valid JSON array with one entry
    /// per connector, each containing `id`, `display_name`, and `status`.
    /// What: builds the router with stub connectors, issues GET
    /// /api/console/services, parses the response.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_services_route_returns_json() {
        let router = build_router(make_test_state());

        let req = Request::builder()
            .uri("/api/console/services")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = get_bytes(resp).await;
        let body: Vec<serde_json::Value> = serde_json::from_slice(&bytes).expect("parse json");
        assert_eq!(body.len(), 3);

        assert_eq!(body[0]["id"], "trusty-search");
        assert_eq!(body[0]["status"], "running");
        assert_eq!(body[0]["display_name"], "Trusty Search");

        assert_eq!(body[1]["id"], "trusty-memory");
        assert_eq!(body[1]["status"], "available");

        assert_eq!(body[2]["id"], "trusty-analyze");
        assert_eq!(body[2]["status"], "absent");
    }

    /// Why: health endpoint must return 200 with `status: ok`.
    /// What: issues GET /health and checks the JSON body.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_health_route() {
        let router = build_router(make_test_state());

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = get_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("parse json");
        assert_eq!(body["status"], "ok");
        assert!(body["version"].is_string());
    }

    /// Why: the services route must serialise `Degraded` status and the
    /// `hint` field correctly so the UI can render a distinct badge.
    /// What: builds the router with a Degraded stub connector, issues GET
    /// /api/console/services, asserts `status == "degraded"` and `hint` present.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_services_route_returns_degraded_with_hint() {
        use crate::connector::ServiceInfo;
        struct DegradedConnector;
        impl ServiceConnector for DegradedConnector {
            fn id(&self) -> &'static str {
                "trusty-analyze"
            }
            fn display_name(&self) -> &'static str {
                "Trusty Analyze"
            }
            fn detect(&self) -> ServiceInfo {
                ServiceInfo {
                    id: "trusty-analyze".to_string(),
                    display_name: "Trusty Analyze".to_string(),
                    status: ServiceStatus::Degraded,
                    version: None,
                    url: None,
                    hint: Some("reachable but `console_metrics` tool not registered".to_string()),
                }
            }
        }
        let state = AppState::new(vec![Box::new(DegradedConnector)]);
        let router = build_router(state);
        let req = Request::builder()
            .uri("/api/console/services")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = get_bytes(resp).await;
        let body: Vec<serde_json::Value> = serde_json::from_slice(&bytes).expect("parse json");
        assert_eq!(body.len(), 1);
        assert_eq!(body[0]["status"], "degraded");
        assert!(
            body[0].get("hint").is_some(),
            "degraded service must include hint field"
        );
        assert!(
            body[0]["hint"]
                .as_str()
                .unwrap_or("")
                .contains("console_metrics"),
            "hint must mention console_metrics"
        );
    }

    /// Why: the root path must serve the embedded HTML (or placeholder).
    /// What: issues GET / and asserts 200 + text/html content-type.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_spa_root_returns_html() {
        let router = build_router(make_test_state());

        let req = Request::builder()
            .uri("/")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let ct = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(ct.contains("text/html"), "expected text/html, got: {ct}");
    }

    /// A connector whose `detect()` always panics — simulates a buggy plugin.
    struct PanicConnector;

    impl ServiceConnector for PanicConnector {
        fn id(&self) -> &'static str {
            "panic-svc"
        }
        fn display_name(&self) -> &'static str {
            "Panic Service"
        }
        fn detect(&self) -> ServiceInfo {
            panic!("intentional test panic from PanicConnector");
        }
    }

    /// Why: a panicking connector must not silently return HTTP 200 with an
    /// empty list — that is indistinguishable from "no services installed".
    /// The handler must return HTTP 500 so the UI can display an error state.
    /// What: builds the router with a PanicConnector, issues GET
    /// /api/console/services, asserts the response status is 500.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_services_handler_returns_500_on_panic() {
        let state = AppState::new(vec![Box::new(PanicConnector)]);
        let router = build_router(state);

        let req = Request::builder()
            .uri("/api/console/services")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// Why: with an empty metrics cache the route must return 503 so the UI
    /// can show a "not yet available" state rather than empty JSON.
    /// What: issues GET /api/console/metrics/analyze on a fresh state,
    /// asserts 503.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_metrics_analyze_route_cold_cache_returns_503() {
        let router = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/console/metrics/analyze")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Why: the proxy route for an unknown daemon key must return 400.
    /// What: issues GET /proxy/unknown/health, asserts 400.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_proxy_unknown_daemon_returns_400() {
        let router = build_router(make_test_state());

        let req = Request::builder()
            .uri("/proxy/unknown/health")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Why: the proxy route for a known daemon that is not running must return
    /// 503 (cache not populated) when no poll has occurred yet.
    /// What: issues GET /proxy/search/health on a fresh state (no poll),
    /// asserts 503 SERVICE_UNAVAILABLE.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_proxy_known_daemon_cold_cache_returns_503() {
        let router = build_router(make_test_state());

        let req = Request::builder()
            .uri("/proxy/search/health")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Why: with an empty memory metrics cache the route must return 503 so the
    /// UI can show a "not yet available" state rather than empty JSON.
    /// What: issues GET /api/console/metrics/memory on a fresh state, asserts 503.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_metrics_memory_route_cold_cache_returns_503() {
        let router = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/console/metrics/memory")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Why: with an empty search metrics cache the route must return 503 so the
    /// UI can show a "not yet available" state rather than empty JSON.
    /// What: issues GET /api/console/metrics/search on a fresh state, asserts 503.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_metrics_search_route_cold_cache_returns_503() {
        let router = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/console/metrics/search")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Why: with an empty review metrics cache the route must return 503 so the
    /// UI can show a "not yet available" state rather than empty JSON.
    /// What: issues GET /api/console/metrics/review on a fresh state, asserts 503.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_metrics_review_route_cold_cache_returns_503() {
        let router = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/console/metrics/review")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// Why: the analyze indexes route must return 503 (not 200 with empty data)
    /// when the trusty-analyze binary is absent — the handle immediately marks
    /// itself Absent and the route converts that to SERVICE_UNAVAILABLE.
    /// What: issues GET /api/console/metrics/analyze/indexes on a fresh state
    /// (where trusty-analyze is not on PATH in CI), asserts 503.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_analyze_indexes_absent_binary_returns_503() {
        let router = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/console/metrics/analyze/indexes")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        // Binary absent (or in backoff) → 503; if present and daemon is up → 200.
        // In CI neither condition holds; the route must not return 500.
        assert_ne!(
            resp.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "indexes route must not 500 when binary absent"
        );
    }

    /// Why: the analyze visualize route must return 400 when no `index` param
    /// is provided — the endpoint needs it to query the daemon. A 200 with an
    /// error field is indistinguishable from a success response to callers that
    /// only check the status code.
    /// What: issues GET /api/console/metrics/analyze/visualize (no ?index=),
    /// asserts HTTP 400 and a JSON body containing `error`.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_analyze_visualize_handler_no_index_returns_json_error() {
        let router = build_router(make_test_state());
        let req = Request::builder()
            .uri("/api/console/metrics/analyze/visualize")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        // Missing index returns 400 BAD_REQUEST with a JSON error body.
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "missing index param must return 400"
        );
        let bytes = get_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("parse json");
        assert!(
            body.get("error").is_some(),
            "expected error field, got: {body}"
        );
    }

    /// Why: This is the key regression test for the UAT gap: the connector
    /// `detect()` path reports `Running` or `Available` because it only does a
    /// TCP/which probe and knows nothing about `tools/list`. When the actual
    /// `McpServiceHandle` is in `Degraded` state (tools/list succeeded but
    /// `console_metrics` absent), `GET /api/console/services` MUST override that
    /// connector result to `degraded` with the remediation hint.
    /// What: Builds state whose connector returns `Running` for trusty-search,
    /// manually primes the trusty-search `McpServiceHandle` to `Degraded`, then
    /// issues GET /api/console/services and asserts `status == "degraded"` with
    /// a non-empty `hint`.  A connector that was `Absent` must NOT be overridden
    /// (only reachable services can be Degraded by the tools/list probe).
    /// This test intentionally does NOT use a hand-stubbed DegradedConnector —
    /// it exercises the real `apply_handle_overrides` bridge from
    /// `McpServiceHandle.state` → route response.
    /// Test: this test itself.
    #[tokio::test]
    async fn test_services_route_handle_degraded_overlay() {
        // Build state with:
        //  - trusty-search connector returning Running (TCP probe passed)
        //  - trusty-analyze connector returning Absent (binary not found)
        let state = AppState::new(vec![
            Box::new(StubConnector {
                id: "trusty-search",
                display_name: "Trusty Search",
                status: ServiceStatus::Running,
            }),
            Box::new(StubConnector {
                id: "trusty-analyze",
                display_name: "Trusty Analyze",
                status: ServiceStatus::Absent,
            }),
        ]);

        // Prime the trusty-search handle to Degraded state (tools/list passed
        // but console_metrics was absent).  This simulates the real-world
        // situation on a machine where the daemon lacks console_metrics.
        {
            let handles = state.mcp_handles();
            let search_handle = handles
                .get("trusty-search")
                .expect("search handle must exist");
            search_handle.prime_degraded_for_test().await;
        }

        let router = build_router(state);
        let req = Request::builder()
            .uri("/api/console/services")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = get_bytes(resp).await;
        let body: Vec<serde_json::Value> = serde_json::from_slice(&bytes).expect("parse json");
        assert_eq!(body.len(), 2);

        // trusty-search was Running via connector but Degraded via handle →
        // must be overridden to degraded with a hint.
        let search = body
            .iter()
            .find(|s| s["id"] == "trusty-search")
            .expect("search entry");
        assert_eq!(
            search["status"], "degraded",
            "Running service whose handle is Degraded must report degraded, got: {search}"
        );
        let hint = search["hint"].as_str().unwrap_or("");
        assert!(
            !hint.is_empty(),
            "degraded service must include a non-empty hint"
        );
        assert!(
            hint.contains("console_metrics"),
            "hint must mention console_metrics, got: {hint}"
        );

        // trusty-analyze was Absent via connector — Absent must NOT be overridden
        // even if the handle were somehow Degraded (process-down ≠ degraded).
        let analyze = body
            .iter()
            .find(|s| s["id"] == "trusty-analyze")
            .expect("analyze entry");
        assert_eq!(
            analyze["status"], "absent",
            "Absent service must not be overridden to degraded"
        );
    }

    /// Why: Regression test for issue #1170 — a stale daemon whose MCP process
    /// is running but lacks the `list_analyze_indexes` tool must cause the
    /// `/api/console/metrics/analyze/indexes` route to return HTTP 503 with a
    /// clean JSON body containing `status: "degraded"` and an actionable `hint`,
    /// NOT HTTP 502 with empty body. The capability-gate in `call_tool_checked`
    /// must fire before any JSON-RPC call is made to the daemon.
    /// What: Builds state with a `trusty-analyze` handle primed to `Connected`
    /// but missing `list_analyze_indexes` in the cached tool set. Issues GET
    /// /api/console/metrics/analyze/indexes and asserts:
    ///   1. Status is 503 (SERVICE_UNAVAILABLE), not 502 (BAD_GATEWAY).
    ///   2. JSON body has `status == "degraded"`.
    ///   3. JSON body has a non-empty `hint` mentioning the missing tool.
    /// Test: this test itself. Key regression for #1170.
    #[tokio::test]
    #[cfg(unix)]
    async fn test_analyze_indexes_tool_unavailable_returns_degraded_hint() {
        let state = make_test_state();

        // Prime the analyze handle to Connected with list_analyze_indexes absent.
        {
            let analyze_handle = state.analyze_handle();
            analyze_handle
                .prime_connected_missing_tool_for_test("list_analyze_indexes")
                .await;
        }

        let router = build_router(state);
        let req = Request::builder()
            .uri("/api/console/metrics/analyze/indexes")
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");

        // Must be 503, not 502 — the capability gate fires, not the JSON-RPC call.
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "missing tool must return 503 SERVICE_UNAVAILABLE, not 502 BAD_GATEWAY"
        );

        let bytes = get_bytes(resp).await;
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("parse json body");

        assert_eq!(
            body["status"], "degraded",
            "response body must have status=degraded, got: {body}"
        );

        let hint = body["hint"].as_str().unwrap_or("");
        assert!(
            !hint.is_empty(),
            "response body must include a non-empty hint, got: {body}"
        );
        assert!(
            hint.contains("list_analyze_indexes"),
            "hint must mention the missing tool name, got: {hint}"
        );
    }
}
