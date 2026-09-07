//! The one place every route this binary serves is mounted (#6155).
//!
//! Why its own file: `server/mod.rs` carries `AppState` and the handlers that
//! read it, and adding the three `/tools/memory/` routes plus the two
//! `/api/memory/` ones pushed that file past the 500-SLOC cap. The router is the
//! cohesive half — one function that names every path, in the order matchit
//! resolves them — so it is what moved.
//! What: the three public builders, and the single private `build_router_inner`
//! they all delegate to so the mounted route set cannot drift between them.
//! Test: `server/tests.rs` drives the built router; `tests/memory_ui_mount.rs`,
//! `tests/memory_uds_bridge.rs` and their search counterparts drive the mounts
//! this file wires.

use axum::{
    Router,
    routing::{any, get, post},
};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use super::{
    AppState, analyze_indexes_handler, analyze_visualize_handler, health_handler, services_handler,
};

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

/// Build the router with the webhook ingress mounted (#5089 step 3).
///
/// Why: the ingress owns a spool directory, so constructing it can fail — and
/// it must fail loudly at startup rather than silently leaving
/// `/api/webhooks/{source}` unrouted, which would turn every delivery into a
/// `404` GitHub records as a failure nobody looks at. Keeping it a separate
/// parameter lets `run_serve` do that fallible construction once while the
/// existing infallible `build_router` call sites (and every test that does not
/// exercise webhooks) stay unchanged.
/// What: identical to [`build_router_with_self_origins`], plus
/// `POST /api/webhooks/{source}` and `GET /api/console/metrics/webhooks`, both
/// carrying `WebhookIngress` as their own state.
/// Test: the `route_*` and `metrics_route_*` cases in `webhook/tests.rs`.
pub fn build_router_with_webhooks(
    state: AppState,
    self_origins: crate::routes::origin_guard::SelfOrigins,
    ingress: crate::webhook::WebhookIngress,
) -> Router {
    build_router_inner(state, self_origins, Some(ingress))
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
    build_router_inner(state, self_origins, None)
}

/// The one router definition both public builders delegate to, so the mounted
/// route set cannot drift between them.
fn build_router_inner(
    state: AppState,
    self_origins: crate::routes::origin_guard::SelfOrigins,
    webhook: Option<crate::webhook::WebhookIngress>,
) -> Router {
    let core = Router::new()
        .route("/health", get(health_handler))
        .route("/api/console/services", get(services_handler))
        // The five per-service handlers moved to `crate::routes::metrics` when
        // #6641's new routes pushed this file over the 500-SLOC cap.
        .route(
            "/api/console/metrics/analyze",
            get(crate::routes::metrics::metrics_analyze_handler),
        )
        .route(
            "/api/console/metrics/memory",
            get(crate::routes::metrics::metrics_memory_handler),
        )
        .route(
            "/api/console/metrics/search",
            get(crate::routes::metrics::metrics_search_handler),
        )
        .route(
            "/api/console/metrics/review",
            get(crate::routes::metrics::metrics_review_handler),
        )
        .route(
            "/api/console/metrics/mpm",
            get(crate::routes::metrics::metrics_mpm_handler),
        )
        // #6517: aggregated whole-machine host resources + per-service rollup.
        .route(
            "/api/console/machine-status",
            get(crate::routes::machine_status::machine_status_handler),
        )
        // #6641: the bounded 10-minute window behind the Phase 3 graphs, and the
        // live stream that keeps an open dashboard moving. Both are static
        // segments under `/machine-status`, so neither shadows nor is shadowed
        // by the point-in-time route above.
        .route(
            "/api/console/machine-status/history",
            get(crate::routes::machine_history::history_handler),
        )
        .route(
            "/api/console/machine-status/stream",
            get(crate::routes::machine_history::stream_handler),
        )
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
        // #6431: record-only bulk delete. A static segment, so it wins over the
        // `{id}` capture below — pinned by `bulk_delete_route_is_not_shadowed`.
        .route(
            "/api/console/sessions/bulk-delete",
            axum::routing::post(crate::routes::sessions::bulk_delete_handler),
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
        // #6360: operator-driven deletion of one palace / one index. Both call
        // the owning daemon's existing teardown and report what it actually did
        // — the console implements no deletion of its own. The router-wide
        // origin guard below covers them, as it does every other write route.
        .route(
            "/api/console/memory/palaces/{id}",
            axum::routing::delete(crate::routes::deletes::delete_palace_handler),
        )
        .route(
            "/api/console/search/indexes/{id}",
            axum::routing::delete(crate::routes::deletes::delete_index_handler),
        )
        // #6941: `POST /api/console/search/prune-indexes` and
        // `.../deregister-unjudged` are gone. The search dashboard carries that
        // panel now (DOC-73 §13) and calls trusty-search's own
        // `GET /registry/orphans` and `DELETE /indexes/{id}` directly, so the
        // console proxying a management POST would be a second path to the same
        // work — and console is display-only.
        .route(
            "/api/console/memory/palaces/{id}/compact",
            post(crate::routes::cleanup::compact_palace_handler),
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
        // #6285: `search` is NOT a reverse-proxy row any more. trusty-search
        // moved onto a Unix socket (ADR-0032) and stopped writing the
        // `http_addr` file the proxy resolves a base URL from, so this literal
        // route takes the prefix and translates each request into an RPC call.
        // matchit prefers the static `search` segment over the `{service}`
        // capture below, so the two cannot collide.
        .route(
            "/api/search/{*path}",
            any(crate::search_uds::routes::search_api_handler),
        )
        .route(
            "/proxy/search/{*path}",
            any(crate::search_uds::routes::deprecated_search_api_handler),
        )
        // #6155: `memory` is NOT a reverse-proxy row either — #6286 deleted it
        // when trusty-memory moved onto a Unix socket and stopped writing the
        // `http_addr` file the proxy resolves a base URL from. This literal
        // route takes the prefix and translates each request into an RPC call,
        // which is what makes the dashboard at /tools/memory/ reachable.
        .route(
            "/api/memory/{*path}",
            any(crate::memory_uds::routes::memory_api_handler),
        )
        .route(
            "/proxy/memory/{*path}",
            any(crate::memory_uds::routes::deprecated_memory_api_handler),
        )
        // #6155: nor is `analyze` — #6287 deleted its proxy row for the same
        // reason, when trusty-analyze moved onto a Unix socket and stopped
        // writing the `http_addr` file. This literal route takes the prefix and
        // translates each request into an RPC call, which is what makes the
        // dashboard at /tools/analyze/ reachable.
        .route(
            "/api/analyze/{*path}",
            any(crate::analyze_uds::routes::analyze_api_handler),
        )
        .route(
            "/proxy/analyze/{*path}",
            any(crate::analyze_uds::routes::deprecated_analyze_api_handler),
        )
        // Primary reverse-proxy: /api/{service}/{*path} (#1849 Phase 2).
        // {service} ∈ {review, mpm, agents}.
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
        // #6155: the trusty-search SPA, served from this binary under
        // /tools/search/. Its API calls resolve to /api/search/*, which the
        // proxy route above forwards — so the dashboard keeps working once
        // trusty-search drops its own HTTP surface (#6285, ADR-0032).
        .route("/tools/search", get(crate::tools_ui::search_ui_redirect))
        .route("/tools/search/", get(crate::tools_ui::search_ui_index))
        .route(
            "/tools/search/{*path}",
            get(crate::tools_ui::search_ui_asset),
        )
        // #6155: the trusty-memory SPA, served from this binary under
        // /tools/memory/. Its API calls resolve to /api/memory/*, which the
        // bridge above translates onto the daemon's socket — the only way in
        // since #6286 took its HTTP listener away.
        .route("/tools/memory", get(crate::tools_ui::memory_ui_redirect))
        .route("/tools/memory/", get(crate::tools_ui::memory_ui_index))
        .route(
            "/tools/memory/{*path}",
            get(crate::tools_ui::memory_ui_asset),
        )
        // #6155: the trusty-analyze SPA, served from this binary under
        // /tools/analyze/. Its API calls resolve to /api/analyze/*, which the
        // bridge above translates onto the daemon's socket — the only way in
        // since #6287 took its HTTP listener away.
        .route("/tools/analyze", get(crate::tools_ui::analyze_ui_redirect))
        .route("/tools/analyze/", get(crate::tools_ui::analyze_ui_index))
        .route(
            "/tools/analyze/{*path}",
            get(crate::tools_ui::analyze_ui_asset),
        )
        .route("/", get(crate::console_ui::spa_index_handler))
        // #6519: /ui/screensaver already reaches the shell through the SPA
        // fallback in the wildcard below; this is the top-level alias, which has
        // no wildcard to fall through and so needs its own route.
        .route("/screensaver", get(crate::console_ui::spa_index_handler))
        .route("/ui", get(crate::console_ui::spa_index_handler))
        .route("/ui/", get(crate::console_ui::spa_index_handler))
        .route("/ui/{*path}", get(crate::console_ui::spa_asset_handler))
        .with_state(state);

    // Webhook ingress (#5089 step 3, ADR-0034). Merged as its own state-typed
    // sub-router. `/api/webhooks/{source}` cannot be shadowed by the
    // `/api/{service}/{*path}` proxy above: matchit 0.8 prefers a static
    // segment over a `{param}` capture at the same position, so `webhooks`
    // wins regardless of declaration order — the same precedence rule the
    // `/api/console/*` routes already rely on.
    let router = match webhook {
        Some(ingress) => core.merge(
            Router::new()
                .route(
                    "/api/webhooks/{source}",
                    axum::routing::post(crate::webhook::webhook_handler),
                )
                .route(
                    "/api/console/metrics/webhooks",
                    get(crate::webhook::metrics_webhooks_handler),
                )
                .with_state(ingress)
                // axum's DefaultBodyLimit is 2 MiB, which silently 413s a real
                // delivery before the handler runs: no spool entry, no metric,
                // no ack — the exact invisible drop this route exists to
                // prevent. GitHub payloads are legal to 25 MB and `push` /
                // `pull_request` bodies routinely pass 2 MiB. Scoped to this
                // sub-router so the proxy and SPA routes keep the default.
                // (`trusty-search` sets 64 MiB the same way, at
                // `service/server/mod.rs:251`.)
                .layer(axum::extract::DefaultBodyLimit::max(
                    crate::webhook::MAX_WEBHOOK_BODY_BYTES,
                )),
        ),
        None => core,
    };

    router
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
