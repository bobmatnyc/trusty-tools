//! Axum HTTP server for the trusty-console.
//!
//! Why: The console needs a lightweight HTTP server that serves the embedded
//! SPA, a JSON API route for service status, and a reverse-proxy layer for
//! all daemon sub-paths.
//! What: Builds an axum `Router` with:
//!   - `GET /health` — liveness probe.
//!   - `GET /api/console/services` — return cached snapshot (background poll).
//!   - `GET /api/console/metrics/{analyze,memory,search,review,mpm}` — MCP-polled metrics.
//!   - `GET /api/console/metrics/analyze/indexes` — analyze index list via stdio MCP.
//!   - `GET /api/console/metrics/analyze/visualize?index=<id>` — graph+entities+clusters.
//!   - `…/api/console/sessions/*` — the single HTTP front door for the trusty-mpm
//!     session manager (#1222); handlers live in `crate::routes::sessions`.
//!   - `ANY /api/{service}/{*path}` — reverse-proxy to live daemon via clean path
//!     (#1849 Phase 2); `{service}` ∈ {search, memory, analyze, review, mpm}.
//!   - `ANY /proxy/{daemon}/{*path}` — DEPRECATED alias; routes to the same
//!     handler with a trace-level deprecation note.
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
    /// trusty-mpm `console_metrics` cache (#1222). Populated by the background
    /// poller; served by `GET /api/console/metrics/mpm`.
    mpm_metrics_cache: MetricsCache,
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
    /// `reqwest::Client` with idle-connection pooling disabled (#1984 — see the
    /// builder comment below). Creates the analyze stdio MCP handle that is
    /// shared between the background metrics poller and on-demand routes.
    /// Populates `mcp_handles` with all three per-service handles so the
    /// services route can read their degraded state.
    /// Test: Used in `build_router` and directly in `tests`.
    pub fn new(connectors: Vec<Box<dyn ServiceConnector>>) -> Self {
        // Why pool_max_idle_per_host(0): the proxy client must survive an upstream
        // daemon restart (#1984). With the default keep-alive pool, the FIRST
        // proxied request after an upstream restart reuses a stale idle connection
        // to the now-dead process and fails — an instant RST → 502, a half-open
        // hang → 30s-timeout → 502, or a partial write the restarted daemon
        // rejects → 500 — even though a direct curl (which never pools across
        // invocations) always opens a fresh connection and succeeds. reqwest does
        // NOT retry a non-idempotent POST on a broken pooled connection, so the
        // failure is surfaced to the caller (e.g. `tm session new`). Disabling
        // idle-connection reuse forces every proxied request to open a fresh
        // connection to whatever process currently owns the port, eliminating the
        // stale-reuse failure at the root. Loopback connect cost is negligible.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(0)
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
        // Why: trusty-mpm's stdio MCP mode is `serve --stdio` (the #1221 bridge
        // that auto-starts the durable daemon and forwards JSON-RPC to its
        // loopback POST /rpc). The console spawns this to render the Sessions tab
        // natively (#1222) without ever touching the daemon's HTTP port (#1104).
        let mpm_handle = Arc::new(McpServiceHandle::new(
            "trusty-mpm",
            vec!["serve".to_string(), "--stdio".to_string()],
        ));
        let mut handles: HashMap<String, Arc<McpServiceHandle>> = HashMap::new();
        handles.insert("trusty-analyze".to_string(), Arc::clone(&analyze_handle));
        handles.insert("trusty-memory".to_string(), Arc::clone(&memory_handle));
        handles.insert("trusty-search".to_string(), Arc::clone(&search_handle));
        handles.insert("trusty-review".to_string(), Arc::clone(&review_handle));
        handles.insert("trusty-mpm".to_string(), Arc::clone(&mpm_handle));
        Self {
            connectors: Arc::new(connectors),
            poller_cache: PollerCache::new(),
            metrics_cache: MetricsCache::new(),
            memory_metrics_cache: MetricsCache::new(),
            search_metrics_cache: MetricsCache::new(),
            review_metrics_cache: MetricsCache::new(),
            mpm_metrics_cache: MetricsCache::new(),
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

    /// Access the metrics cache for the trusty-mpm stdio MCP poller (#1222).
    ///
    /// Why: separate cache per service so the mpm session/supervisor report can
    /// be updated and served independently from the other service caches.
    /// What: returns a reference to the `MetricsCache` handle for mpm.
    /// Test: `test_metrics_mpm_route_cold_cache_returns_503`.
    pub fn mpm_metrics_cache(&self) -> &MetricsCache {
        &self.mpm_metrics_cache
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

/// Build the axum `Router` with all routes wired, trusting only loopback as
/// the write-origin self-origin.
///
/// Why: Extracting the router into its own function allows both `main` and the
/// test harness to share the same routing configuration without running a real
/// TCP server. This loopback-only entry point is what every existing test and
/// `Local`/`Explicit` (non-Tailscale) bind mode use; Tailscale deployments use
/// [`build_router_with_self_origins`] instead so their own bind address is
/// also trusted (#3269).
/// What: Returns a `Router<()>` with CORS, tracing middleware, and all routes.
/// Test: Called from `tests::test_services_route_returns_json` below.
pub fn build_router(state: AppState) -> Router {
    build_router_with_self_origins(state, crate::routes::origin_guard::SelfOrigins::default())
}

/// Build the axum `Router` with all routes wired, additionally trusting the
/// given bind-derived, non-loopback self-origins for the write-origin guard.
///
/// Why: #3269 — in Tailscale bind mode the console's own write UI is served
/// from a non-loopback address; the guard must trust that exact address
/// (derived from the server's actually-resolved bind addresses) without
/// opening up to arbitrary remote origins. Splitting this out from
/// `build_router` keeps every existing (loopback-only) call site and test
/// unchanged.
/// What: Identical router to `build_router`, except the write-origin guard
/// (see below) is constructed with `self_origins` instead of the default
/// empty set.
/// Test: `server/tests.rs` tests `proxy_route_allows_self_origin_write` /
/// `proxy_route_rejects_cross_origin_write`; `bind.rs`/`lib.rs` wire the real
/// resolved addresses in `run_serve`.
pub fn build_router_with_self_origins(
    state: AppState,
    self_origins: crate::routes::origin_guard::SelfOrigins,
) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/api/console/services", get(services_handler))
        .route("/api/console/metrics/analyze", get(metrics_analyze_handler))
        .route("/api/console/metrics/memory", get(metrics_memory_handler))
        .route("/api/console/metrics/search", get(metrics_search_handler))
        .route("/api/console/metrics/review", get(metrics_review_handler))
        .route("/api/console/metrics/mpm", get(metrics_mpm_handler))
        // ── trusty-mpm session-manager surface (#1222: P2 tab + P3 front door) ──
        // The console is the SINGLE HTTP front door for the session REST API;
        // every handler calls a trusty-mpm MCP tool via the stdio bridge — never
        // the daemon's HTTP port (#1104).
        //
        // Route precedence (verified, NOT declaration-order dependent): axum 0.8
        // routes via matchit 0.8, which prioritises a literal/static path segment
        // over a `{param}` capture at the same position regardless of the order
        // routes are added. So `/sessions/supervisor` and
        // `/sessions/supervisor/auto-resume` always win over `/sessions/{id}` —
        // a request for `…/supervisor` reaches `supervisor_handler`, never
        // `get_handler` with id="supervisor". This is asserted directly by
        // `routes::sessions::tests::supervisor_route_is_not_shadowed_by_id_capture`
        // and `…::auto_resume_route_is_not_shadowed`.
        .route(
            "/api/console/sessions",
            get(crate::routes::sessions::list_handler).post(crate::routes::sessions::new_handler),
        )
        .route(
            "/api/console/sessions/supervisor",
            get(crate::routes::sessions::supervisor_handler),
        )
        .route(
            "/api/console/sessions/supervisor/auto-resume",
            axum::routing::post(crate::routes::sessions::auto_resume_handler),
        )
        .route(
            "/api/console/sessions/{id}",
            get(crate::routes::sessions::get_handler)
                .delete(crate::routes::sessions::decommission_handler),
        )
        .route(
            "/api/console/sessions/{id}/activity",
            get(crate::routes::sessions::activity_handler),
        )
        .route(
            "/api/console/sessions/{id}/stop",
            axum::routing::post(crate::routes::sessions::stop_handler),
        )
        .route(
            "/api/console/sessions/{id}/resume",
            axum::routing::post(crate::routes::sessions::resume_handler),
        )
        // #1220 Config tab: read/write the `~/.trusty-tools/trusty-mpm/config.yaml`
        // convention via the trusty-mpm `config_read` / `config_write` MCP tools.
        // The POST is a state-changing write; the router-wide origin guard
        // (see the `.layer()` call near the bottom of this router) covers it.
        .route(
            "/api/console/config/mpm",
            get(crate::routes::config::get_handler).post(crate::routes::config::post_handler),
        )
        // Analyze on-demand routes — call the analyze stdio MCP directly (no /proxy).
        .route(
            "/api/console/metrics/analyze/indexes",
            get(analyze_indexes_handler),
        )
        .route(
            "/api/console/metrics/analyze/visualize",
            get(analyze_visualize_handler),
        )
        // Primary reverse-proxy: /api/{service}/{*path} (#1849 Phase 2).
        // {service} ∈ {search, memory, analyze, review, mpm}.
        // No collision with /api/console/*: axum (matchit 0.8) routes literal
        // segments before wildcard captures, so /api/console/* always wins.
        // The proxy handler also rejects service_key == "console" explicitly as // pragma: allowlist secret
        // a routing-independent second layer of defence.
        .route("/api/{service}/{*path}", any(crate::proxy::proxy_handler))
        // Deprecated alias: /proxy/{daemon}/{*path} → same handler with a trace log.
        // Kept for backward compatibility; callers should migrate to /api/{service}/*.
        .route(
            "/proxy/{daemon}/{*path}",
            any(crate::proxy::deprecated_proxy_handler),
        )
        .route("/", get(spa_index_handler))
        .route("/ui", get(spa_index_handler))
        .route("/ui/", get(spa_index_handler))
        .route("/ui/{*path}", get(spa_asset_handler))
        .with_state(state)
        // Same-origin guard for ALL destructive write routes, applied
        // router-wide (#3268 fix). The console serves a permissive CORS
        // policy (open reads), so without this guard any web page the
        // operator visited could fire a cross-origin `fetch` and
        // spawn/stop/decommission sessions, or — since this is a plain
        // `.layer()`, not `route_layer` — reach destructive daemon endpoints
        // through the reverse-proxy routes above (`/api/{service}/{*path}`,
        // `/proxy/{daemon}/{*path}`), which a route-scoped `route_layer`
        // placed earlier in the chain would miss entirely (the #3268 root
        // cause). The middleware is method-aware — it only blocks
        // state-changing methods whose `Origin` header is present and
        // neither loopback nor a trusted self-origin, so GET reads (and the
        // read-only daemon proxy traffic) pass through untouched.
        .layer(axum::middleware::from_fn_with_state(
            self_origins,
            crate::routes::origin_guard::guard_write_origin,
        ))
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

/// `GET /api/console/metrics/mpm` — return the latest trusty-mpm metrics report.
///
/// Why: surfaces trusty-mpm session-fleet + supervisor health to the SPA without
/// per-request MCP calls (the background poller keeps the cache warm). This is
/// the coarse, low-frequency health cache; the Sessions tab polls the live
/// `/api/console/sessions` list at a faster cadence for active monitoring.
/// What: returns the cached `ConsoleMetricsReport` as JSON (200) or 503 when no
/// poll has completed yet (binary absent or first boot).
/// Test: `test_metrics_mpm_route_cold_cache_returns_503` below.
async fn metrics_mpm_handler(State(state): State<AppState>) -> axum::response::Response {
    match state.mpm_metrics_cache().get().await {
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

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
