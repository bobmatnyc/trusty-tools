//! HTTP daemon: axum router exposing the trusty-search REST API.
//!
//! Why: Single shared `SearchAppState` (wrapped in `Arc`) lets every handler
//! read from the `IndexRegistry` concurrently. `DashMap` shard-locks per index
//! so different indexes never contend, and `Arc<RwLock<CodeIndexer>>` allows
//! many simultaneous readers per index.
//!
//! What: This module is a thin facade that declares submodules and re-exports
//! the public surface. Routes implement the API described in `CLAUDE.md`.
//!
//! Test: `cargo test -p trusty-search` boots the router with an in-process
//! registry and exercises each endpoint.

mod admin;
mod files;
mod health;
mod helpers;
mod indexes;
mod indexes_relocate;
mod reindex_handlers;
mod router;
mod routing;
mod search;
mod state;
mod state_impl;
mod status;
mod tickers;

// cfg(test) sub-modules — each < 500 lines
#[cfg(test)]
mod tests_1073;
#[cfg(test)]
mod tests_829;
#[cfg(test)]
mod tests_denylist;
#[cfg(test)]
mod tests_grep;
#[cfg(test)]
mod tests_health;
#[cfg(test)]
mod tests_index;
#[cfg(test)]
mod tests_list;
#[cfg(test)]
mod tests_search;
#[cfg(test)]
mod tests_stall;
#[cfg(test)]
mod tests_state;

// Re-export the public surface that was previously at `crate::service::server::*`.
// External callers (`daemon.rs`, `start.rs`, `service/mod.rs`) use these names.
pub use admin::LogsTailParams;
pub use files::ChunksParams;
pub use reindex_handlers::ReindexRequest;
pub use router::{CreateIndexRequest, IndexFileRequest, RemoveFileRequest};
pub use routing::SearchSimilarRequest;
pub use search::GlobalSearchRequest;
pub use state::{DaemonEvent, SearchAppState, WarmBootSummary};

use axum::{
    response::Redirect,
    routing::{delete, get, post},
    Router,
};
use std::sync::Arc;

use admin::{
    admin_stop_handler, get_config_handler, logs_tail_handler, patch_config_handler,
    status_stream_handler,
};
use files::{get_index_chunks_handler, index_file_handler, remove_file_handler};
use health::health_handler;
use indexes::{create_index_handler, list_indexes_handler, relocate_index_handler};
use reindex_handlers::{reindex_handler, reindex_stream_handler};
use routing::search_similar_handler;
use search::{delete_index_handler, global_search_handler, search_handler};
use status::{graph_handler, graph_stats_handler, index_status_handler};
use tickers::{spawn_disk_size_ticker, spawn_idle_chunk_eviction_ticker, spawn_status_ticker};

use files::{call_chain_handler, global_grep_handler, grep_handler};

use self::health::upgrade_handler;

/// Build the axum router with the shared state.
///
/// Why: Wraps `state` in an `Arc` so every handler clones the pointer cheaply.
/// What: Mounts every route, applies the concurrency limiter to expensive
/// endpoints, applies a query deadline to interactive routes only (issue #907),
/// installs the Prometheus metrics route when a recorder is wired, and wraps
/// the whole router in the standard CORS/tracing/gzip middleware from
/// `trusty-common`.
///
/// Route grouping (issue #907):
/// - `interactive_limited`: concurrency-limited AND query-timeout-bounded;
///   contains search/grep/search_similar routes only.
/// - `bulk_limited`: concurrency-limited only (no per-request deadline);
///   contains reindex/index-file/remove-file which are legitimately long-running.
///
/// Test: each handler test builds the router via this function using `oneshot`.
pub fn build_router(state: SearchAppState) -> Router {
    use crate::service::query_timeout::{apply_query_timeout, QueryTimeoutConfig};
    use crate::service::ui::{
        chat_handler, list_chat_providers, ui_asset_handler, ui_index_handler,
    };
    let state_arc = Arc::new(state);
    spawn_status_ticker(Arc::clone(&state_arc));
    spawn_disk_size_ticker(Arc::clone(&state_arc));
    spawn_idle_chunk_eviction_ticker(Arc::clone(&state_arc));

    let limiter = crate::service::concurrency::ConcurrencyLimiter::from_env();
    let query_timeout_cfg = QueryTimeoutConfig::from_env();

    // Interactive routes: concurrency-limited AND query-deadline-bounded.
    // MUST NOT include reindex / index-file — those are legitimately long-running.
    let interactive_limited = Router::new()
        .route("/search", post(global_search_handler))
        .route("/grep", post(global_grep_handler))
        .route("/indexes/{id}/grep", post(grep_handler))
        .route("/indexes/{id}/search", post(search_handler))
        .route("/indexes/{id}/search_similar", post(search_similar_handler))
        // Concurrency limiter is outermost (evaluated first; bounds the queue
        // wait). Query timeout is inner (starts after admission; bounds handler
        // execution). In axum, each successive `.route_layer` call wraps the
        // previously stacked layers, so the limiter — added last — becomes the
        // outer layer that a request reaches first.
        .route_layer(axum::middleware::from_fn(apply_query_timeout))
        .layer(axum::Extension(Arc::clone(&query_timeout_cfg)))
        .route_layer(axum::middleware::from_fn(
            crate::service::concurrency::apply_limiter,
        ))
        .layer(axum::Extension(Arc::clone(&limiter)))
        .with_state(Arc::clone(&state_arc));

    // Bulk / long-running routes: concurrency-limited but NO per-request
    // query deadline — reindex and index-file can legitimately run for minutes.
    let bulk_limited = Router::new()
        .route("/indexes/{id}/index-file", post(index_file_handler))
        .route("/indexes/{id}/remove-file", post(remove_file_handler))
        .route("/indexes/{id}/reindex", post(reindex_handler))
        .route_layer(axum::middleware::from_fn(
            crate::service::concurrency::apply_limiter,
        ))
        .layer(axum::Extension(Arc::clone(&limiter)))
        .with_state(Arc::clone(&state_arc));

    let free = Router::new()
        .route("/", get(|| async { Redirect::permanent("/ui/") }))
        .route("/health", get(health_handler))
        .route("/logs/tail", get(logs_tail_handler))
        .route("/admin/stop", post(admin_stop_handler))
        .route("/status/stream", get(status_stream_handler))
        .route(
            "/indexes",
            get(list_indexes_handler).post(create_index_handler),
        )
        .route(
            "/indexes/{id}",
            delete(delete_index_handler).patch(relocate_index_handler),
        )
        .route("/ui", get(|| async { Redirect::permanent("/ui/") }))
        .route("/ui/", get(ui_index_handler))
        .route("/ui/{*path}", get(ui_asset_handler))
        .route("/chat", post(chat_handler))
        .route("/api/chat/providers", get(list_chat_providers))
        .route("/indexes/{id}/status", get(index_status_handler))
        .route("/indexes/{id}/graph", get(graph_handler))
        .route("/indexes/{id}/graph/stats", get(graph_stats_handler))
        .route("/indexes/{id}/reindex/stream", get(reindex_stream_handler))
        .route("/indexes/{id}/chunks", get(get_index_chunks_handler))
        .route("/indexes/{id}/call_chain", get(call_chain_handler))
        .route(
            "/config",
            get(get_config_handler).patch(patch_config_handler),
        )
        .route("/upgrade", post(upgrade_handler))
        .with_state(Arc::clone(&state_arc));

    let mut router = free.merge(interactive_limited).merge(bulk_limited);

    if let Some(metrics_state) = state_arc.metrics.clone() {
        router = router
            .route("/metrics", get(crate::service::metrics::metrics_handler))
            .layer(axum::Extension(metrics_state));
    }

    router = router.layer(axum::middleware::from_fn(
        crate::service::metrics::request_metrics_middleware,
    ));

    trusty_common::server::with_standard_middleware(router)
}
