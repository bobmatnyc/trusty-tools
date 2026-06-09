//! Router construction, server lifecycle, and foundational route handlers.
//!
//! Why: Extracted from `service/mod.rs` to isolate "how the HTTP surface is
//! assembled and started" from "what each analysis handler does". This module
//! owns `build_router` (wiring) and `serve` (startup), plus the two simplest
//! handlers (`health`, `list_indexes`) that don't belong with any particular
//! feature group.
//!
//! What: `build_router` composes the full axum `Router`; `serve` finds a free
//! port, writes the daemon-addr file, and drives axum with graceful shutdown.
//! `health` probes trusty-search reachability; `list_indexes` proxies to it.
//!
//! Test: `health_degraded_when_search_unreachable` and
//! `list_indexes_proxies_failure_to_502` in `service/tests.rs`.

use std::net::SocketAddr;
use std::sync::Arc;

use crate::core::IndexSummary;
use crate::service::events::{AnalyzerAppState, ApiError};
use anyhow::Result;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json, Redirect},
    routing::{delete, get, post},
    Router,
};
use futures::StreamExt;
use serde::Serialize;
use tokio_stream::wrappers::BroadcastStream;

use super::handlers;
use super::ui;

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
            get(handlers::analysis::complexity_hotspots),
        )
        .route("/indexes/{id}/smells", get(handlers::analysis::smells))
        .route(
            "/indexes/{id}/refactor-suggestions",
            get(handlers::analysis::refactor_suggestions),
        )
        .route(
            "/indexes/{id}/quality",
            get(handlers::analysis::quality_report),
        )
        .route(
            "/indexes/{id}/diagnostics",
            get(handlers::analysis::diagnostics_for_index),
        )
        .route("/indexes/{id}/graph", get(handlers::graph::graph_for_index))
        .route(
            "/indexes/{id}/entities",
            get(handlers::graph::entities_for_index),
        )
        .route(
            "/indexes/{id}/clusters",
            get(handlers::graph::clusters_for_index),
        )
        .route("/indexes/{id}/ner", get(handlers::graph::ner_for_index))
        .route("/indexes/{id}/scip", post(handlers::graph::ingest_scip))
        .route("/review", post(handlers::review::review_diff_handler))
        .route(
            "/review/github-pr",
            post(handlers::review::review_github_pr_handler),
        )
        .route("/analyze/deep", post(handlers::deep::deep_analyze_handler))
        .route(
            "/webhooks/github",
            post(handlers::review::github_webhook_handler),
        )
        .route(
            "/facts",
            get(handlers::facts::list_facts).post(handlers::facts::upsert_fact),
        )
        .route("/facts/{id}", delete(handlers::facts::delete_fact))
        .route("/ui", get(|| async { Redirect::permanent("/ui/") }))
        .route("/ui/", get(ui::ui_index_handler))
        .route("/ui/{*path}", get(ui::ui_asset_handler))
        .with_state(Arc::new(state));
    trusty_common::server::with_standard_middleware(router)
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
    // Best-effort removal of the daemon address file on clean shutdown so the
    // next `trusty-analyze port` invocation does not return a stale address.
    // Mirrors trusty-search's `daemon.rs` cleanup (see service/daemon.rs).
    // The read_daemon_addr() guard that was here previously was removed:
    // remove_file already ignores NotFound, so the guard only introduced a
    // TOCTOU window (another process could create the file between the read
    // and the remove). Errors are intentionally ignored — the lockfile, not
    // the addr file, is what gates the next daemon instance.
    if let Ok(dir) = trusty_common::resolve_data_dir("trusty-analyze") {
        let _ = std::fs::remove_file(dir.join("http_addr"));
    }
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
