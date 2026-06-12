//! Axum HTTP server for the trusty-console.
//!
//! Why: The console needs a lightweight HTTP server that serves the embedded
//! SPA, a JSON API route for service status, and a reverse-proxy layer for
//! all daemon sub-paths.
//! What: Builds an axum `Router` with:
//!   - `GET /health` — liveness probe.
//!   - `GET /api/console/services` — return cached snapshot (background poll).
//!   - `GET /api/console/metrics/{analyze,memory,search}` — MCP-polled metrics.
//!   - `GET /api/console/metrics/analyze/indexes` — analyze index list via stdio MCP.
//!   - `GET /api/console/metrics/analyze/visualize?index=<id>` — graph+entities+clusters.
//!   - `ANY /proxy/{daemon}/{*path}` — reverse-proxy to live daemon.
//!   - `GET /` and `GET /ui/*path` — serve the embedded Svelte SPA.
//!
//! All logs go to stderr; stdout is clean.
//!
//! Test: The `tests` module starts the router in a real axum test client.

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

use crate::connector::ServiceConnector;
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
/// stdio-MCP-polled service (analyze, memory, search) so they can be updated
/// independently and served without coupling. `analyze_handle` is held in Arc
/// so the on-demand visualize/index routes can call the analyze stdio MCP
/// without going through the /proxy path.
/// What: Wraps the connector list, poller cache, per-service metrics caches,
/// reqwest client, and the analyze MCP handle in `Arc`s for cheap cloning.
/// Test: Constructed in `build_router`; exercised by the integration tests.
#[derive(Clone)]
pub struct AppState {
    connectors: Arc<Vec<Box<dyn ServiceConnector>>>,
    poller_cache: PollerCache,
    metrics_cache: MetricsCache,
    memory_metrics_cache: MetricsCache,
    search_metrics_cache: MetricsCache,
    http_client: Arc<reqwest::Client>,
    /// Analyze stdio MCP handle — shared with the metrics poller so both the
    /// background poll and on-demand route calls reuse the same child process.
    analyze_handle: Arc<McpServiceHandle>,
}

impl AppState {
    /// Create a new `AppState` from a list of connectors.
    ///
    /// Why: Lets tests inject a custom connector list and fresh caches.
    /// What: Wraps `connectors` in `Arc`; initialises empty `PollerCache`,
    /// three `MetricsCache` instances (analyze / memory / search), and a
    /// default `reqwest::Client`. Creates the analyze stdio MCP handle that is
    /// shared between the background metrics poller and on-demand routes.
    /// Test: Used in `build_router` and directly in `tests`.
    pub fn new(connectors: Vec<Box<dyn ServiceConnector>>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client init");
        Self {
            connectors: Arc::new(connectors),
            poller_cache: PollerCache::new(),
            metrics_cache: MetricsCache::new(),
            memory_metrics_cache: MetricsCache::new(),
            search_metrics_cache: MetricsCache::new(),
            http_client: Arc::new(client),
            analyze_handle: Arc::new(McpServiceHandle::new(
                "trusty-analyze",
                vec!["mcp".to_string()],
            )),
        }
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
/// Test: `test_services_route_returns_json` and
/// `test_services_handler_returns_500_on_panic` below.
async fn services_handler(State(state): State<AppState>) -> axum::response::Response {
    if let Some(snap) = state.poller_cache().snapshot().await {
        return axum::Json(snap.services).into_response();
    }

    // First-boot fallback: run a one-shot detection synchronously.
    let connectors = state.connectors();
    match tokio::task::spawn_blocking(move || {
        connectors.iter().map(|c| c.detect()).collect::<Vec<_>>()
    })
    .await
    {
        Ok(infos) => axum::Json(infos).into_response(),
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
/// This route calls the analyze stdio MCP (via `McpServiceHandle::call_tool_raw`)
/// instead of the browser hitting the analyze daemon HTTP directly, honouring
/// the #1104 architecture principle: the console is a stdio MCP client only.
/// What: Calls the `list_analyze_indexes` MCP tool (which proxies `GET /indexes`
/// on the daemon). Returns the JSON array on 200, 503 when the analyze binary
/// is absent or in backoff, 502 on any other error.
/// Test: `test_analyze_indexes_absent_binary_returns_503` below.
async fn analyze_indexes_handler(State(state): State<AppState>) -> axum::response::Response {
    match state
        .analyze_handle()
        .call_tool_raw("list_analyze_indexes", serde_json::json!({}))
        .await
    {
        Ok(val) => axum::Json(val).into_response(),
        Err(McpHandleError::Absent | McpHandleError::Backoff { .. }) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
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
/// What: Calls `extract_graph`, `list_entities`, and `cluster_concepts` (k=8)
/// via `McpServiceHandle::call_tool_raw` and returns a combined JSON object:
/// `{"graph": ..., "entities": ..., "clusters": ...}`. Missing index param
/// returns 400 (BAD_REQUEST). Absent binary or backoff returns 503
/// (SERVICE_UNAVAILABLE). A hard graph error (non-absent/backoff) returns
/// 502 (BAD_GATEWAY).
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
    // concurrently, these three `call_tool_raw` calls share a single stdio
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
        handle.call_tool_raw("extract_graph", args.clone()),
        handle.call_tool_raw("list_entities", args.clone()),
        handle.call_tool_raw("cluster_concepts", {
            let mut a = args.clone();
            if let Some(m) = a.as_object_mut() {
                m.insert("k".to_string(), serde_json::json!(8));
            }
            a
        }),
    );

    // Classify the graph result: absent/backoff → 503, hard error → 502,
    // success → combine with best-effort entities and clusters.
    match &graph_res {
        Err(McpHandleError::Absent | McpHandleError::Backoff { .. }) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(e) => {
            tracing::warn!("analyze_visualize_handler graph error: {e:#}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
        Ok(_) => {}
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
}
