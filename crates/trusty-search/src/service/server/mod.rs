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
mod components;
mod contrib_graph;
// #4087: query-time guard so a corpus-failed index fails loudly instead of
// answering HTTP 200 with an empty result set.
mod degraded;
mod facet_route;
mod fanout;
mod files;
mod health;
pub(crate) mod helpers;
mod index_config;
// #5349: the single resolve-or-lazy-load path every index-scoped endpoint that
// can restore a cold-parked index routes through.
mod index_resolve;
mod indexes;
mod indexes_relocate;
mod reindex_handlers;
mod router;
mod routing;
mod search;
mod search_global;
mod state;
mod state_impl;
mod status;
mod tickers;
mod typeahead;

// cfg(test) sub-modules — each < 500 lines
#[cfg(test)]
mod collision_3993_tests;
#[cfg(test)]
mod facet_route_tests;
#[cfg(test)]
mod list_repo_identity_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests_1073;
#[cfg(test)]
mod tests_2336;
#[cfg(test)]
mod tests_2984;
#[cfg(test)]
mod tests_3304;
#[cfg(test)]
mod tests_allowlist_gate_767;
#[cfg(test)]
mod tests_same_id_root_mismatch;
// #3049: DELETE must quiesce in-flight writers and report what it actually did.
#[cfg(test)]
mod tests_3049;
#[cfg(test)]
mod tests_4087;
#[cfg(test)]
mod tests_4110;
#[cfg(test)]
mod tests_4123;
// Issue #4715: an index-scoped 404 must rule out the cold store.
#[cfg(test)]
mod tests_4715;
// #6363: a registration that exists only in `indexes.toml` must be deletable.
#[cfg(test)]
mod tests_6363;
// #4951: a reindex root_path override must not empty every search result.
#[cfg(test)]
mod tests_4951;
// #5357: a failed read of either trust-gate input must refuse the override
// rather than degrade to "trusted".
#[cfg(test)]
mod tests_5357;
// #5349: a write against a cold-parked index drives the load the read path
// drives, and a load that fails refuses the write instead of absorbing it.
#[cfg(test)]
mod tests_5349;
// #4250: timeout-parked index recovery and the /health un-latch.
#[cfg(test)]
mod tests_4250;
#[cfg(test)]
mod tests_829;
#[cfg(test)]
mod tests_chunks;
#[cfg(test)]
mod tests_components;
#[cfg(test)]
mod tests_contrib_graph;
#[cfg(test)]
mod tests_denylist;
#[cfg(test)]
mod tests_grep;
#[cfg(test)]
mod tests_health;
#[cfg(test)]
mod tests_health_contention;
#[cfg(test)]
mod tests_health_degraded;
// #5927: corpus-open failure vs. any-lane failure counter semantics.
#[cfg(test)]
mod stage_failed_5927_tests;
#[cfg(test)]
mod tests_health_switchable;
#[cfg(test)]
mod tests_index;
#[cfg(test)]
mod tests_index_config;
// #2203: a search that drops rows after fusion must report how many and why.
#[cfg(test)]
mod tests_dropped_results;
// #5917: a search over an index whose corpus cannot be read must be refused.
#[cfg(test)]
mod tests_corpus_read_5917;
// #5068 / #5061 / #4787 / #4839: the index-routing + status-reporting cluster.
#[cfg(test)]
mod tests_index_routing;
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
pub use search_global::GlobalSearchRequest;
pub use state::{DaemonEvent, ReconcileSummary, SearchAppState, WarmBootSummary};

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
use contrib_graph::{graph_neighbors_handler, ingest_graph_handler};
use files::{get_index_chunks_handler, index_file_handler, remove_file_handler};
use health::health_handler;
use index_config::{index_config_handler, patch_index_config_handler};
use indexes::{create_index_handler, list_indexes_handler, relocate_index_handler};
use reindex_handlers::{reindex_handler, reindex_stream_handler};
use routing::search_similar_handler;
use search::{delete_index_handler, global_search_handler, search_handler};
use status::{graph_handler, graph_stats_handler, index_status_handler};
use tickers::{
    spawn_disk_size_ticker, spawn_idle_chunk_eviction_ticker, spawn_memory_pressure_ticker,
    spawn_orphan_reaper_ticker, spawn_residency_sweep_ticker, spawn_status_ticker,
    spawn_watcher_idle_suspend_ticker,
};

use files::{call_chain_handler, global_grep_handler, grep_handler};
use typeahead::typeahead_handler;

// Re-export for integration tests in `tests/typeahead.rs`.
//
// Why: the integration tests in `tests/typeahead.rs` call the handler directly
// (not via a running HTTP router), which requires importing both the handler
// function and its `TypeaheadParams` extractor. These names are public so the
// integration-test crate can see them, but `#[doc(hidden)]` keeps them off the
// crate's documented public API surface to avoid misleading downstream callers
// (issue #1560 nit 1).
#[doc(hidden)]
pub use typeahead::{
    typeahead_handler as typeahead_handler_for_tests, TypeaheadParams as TypeaheadParamsForTests,
};

use self::health::upgrade_handler;

// #6285: the one seam `service::socket` reads the health report through, so the
// socket and `GET /health` cannot report different things.
pub(crate) use health::health_report;

// #6285 slice 2: the read surface's transport-neutral bodies and the param
// types they take. `service::rpc::reads` registers one JSON-RPC method per
// entry here; the axum handlers above wrap the SAME functions, which is what
// makes a socket-versus-HTTP parity assertion meaningful rather than a
// comparison of two independent implementations.
pub(crate) use admin::{config_report, ConfigResponse};
pub(crate) use contrib_graph::{graph_neighbors_report, NeighborsParams};
pub(crate) use files::{call_chain_report, index_chunks_report, CallChainParams};
pub(crate) use index_config::{index_config_report, IndexConfigView};
pub(crate) use indexes::{list_indexes_report, ListIndexesParams};
pub(crate) use status::{graph_report, graph_stats_report, index_status_report, GraphQueryParams};

// #6285 slice 3: the query surface's transport-neutral bodies. Same contract as
// the slice-2 block above — `service::rpc::queries` registers one JSON-RPC
// method per entry, and the axum handlers wrap the SAME functions.
pub(crate) use files::{global_grep_report, grep_report};
pub(crate) use routing::search_similar_report;
pub(crate) use search::search_report;
pub(crate) use search_global::global_search_report;
pub(crate) use typeahead::{typeahead_report, TypeaheadParams};

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
    // #3304: loopback-only default. The daemon startup path
    // (`service::daemon`) calls `build_router_with_self_origins` with the
    // actually-resolved bind address so a non-loopback (Tailscale) bind still
    // trusts its own served origin; every existing test keeps this entry point.
    build_router_with_self_origins(state, trusty_common::server::SelfOrigins::default())
}

/// Build the router, additionally trusting the given bind-derived, non-loopback
/// self-origins for the router-wide same-origin write guard (#3304).
///
/// Why: the daemon serves destructive write routes (`POST /admin/stop`,
/// `DELETE /indexes/{id}`, `POST /indexes`, `POST /upgrade`, reindex) behind the
/// permissive-CORS shared stack; without a same-origin guard any page the
/// operator visits could drive them cross-origin (CSRF). This is the guarded
/// entry point; `build_router` delegates here with an empty (loopback-only)
/// allowlist so existing callers/tests are unchanged. The daemon startup path
/// passes its resolved bind address so a non-loopback bind trusts itself (#3269).
/// What: identical router to `build_router`, except the final middleware is
/// `with_guarded_middleware` (guard + standard stack) instead of
/// `with_standard_middleware`.
/// Test: `admin_stop_rejects_cross_origin_write` / `_allows_loopback_write` /
/// `_allows_missing_origin` / `read_route_allows_cross_origin` in `tests_2984`.
pub fn build_router_with_self_origins(
    state: SearchAppState,
    self_origins: trusty_common::server::SelfOrigins,
) -> Router {
    build_router_on(Arc::new(state), self_origins)
}

/// [`build_router_with_self_origins`], over a state the caller already shares.
///
/// Why (#6285, ADR-0032): the daemon now serves the same registry on two
/// transports — this router and `service::socket`'s RPC listener. Both have to
/// read the ONE `SearchAppState` the tickers mutate; a second `Arc::new` would
/// give the socket its own registry, its own disk-size ticker, and its own
/// residency sweep, and the two doors would disagree about which indexes exist.
/// What: the whole former body of [`build_router_with_self_origins`], taking
/// the `Arc` instead of making it. The by-value entry point above is unchanged
/// for every existing caller and test.
/// Test: `health_over_the_socket_matches_the_http_body` in
/// `service::socket::tests`.
pub fn build_router_on(
    state_arc: Arc<SearchAppState>,
    self_origins: trusty_common::server::SelfOrigins,
) -> Router {
    use crate::service::query_timeout::apply_query_timeout;
    use crate::service::ui::{
        chat_handler, list_chat_providers, ui_asset_handler, ui_index_handler,
    };
    spawn_status_ticker(Arc::clone(&state_arc));
    spawn_disk_size_ticker(Arc::clone(&state_arc));
    spawn_idle_chunk_eviction_ticker(Arc::clone(&state_arc));
    spawn_watcher_idle_suspend_ticker(Arc::clone(&state_arc));
    spawn_orphan_reaper_ticker(Arc::clone(&state_arc));
    spawn_residency_sweep_ticker(Arc::clone(&state_arc));
    spawn_memory_pressure_ticker(Arc::clone(&state_arc));
    // #4250: drive indexes parked by a warm-boot restore timeout back into the
    // registry. Nothing else will — they are absent from `list_indexes`, so a
    // client that discovers indexes by listing never names them, and boot
    // reconcile walks registered handles only.
    crate::service::timeout_recovery::spawn_timeout_recovery_ticker(Arc::clone(&state_arc));

    // #6285 slice 3: both live on the state so the socket gates the same six
    // query methods on the SAME semaphore and the SAME deadline. Building them
    // here would give each transport its own.
    let limiter = Arc::clone(&state_arc.query_limiter);
    let query_timeout_cfg = Arc::clone(&state_arc.query_timeout);

    // Interactive routes: concurrency-limited AND query-deadline-bounded.
    // MUST NOT include reindex / index-file — those are legitimately long-running.
    let interactive_limited = Router::new()
        .route("/search", post(global_search_handler))
        .route("/grep", post(global_grep_handler))
        .route("/indexes/{id}/grep", post(grep_handler))
        .route("/indexes/{id}/search", post(search_handler))
        .route("/indexes/{id}/search_similar", post(search_similar_handler))
        .route("/indexes/{id}/typeahead", get(typeahead_handler))
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
        // Contributed-graph ingest (ADR-0009): bulk lane (store + graph
        // rebuild can run for seconds). Body limit 64 MiB — observed maxima
        // from large pilot corpora are ~20 MB, so this is ~3x headroom while
        // bounding the per-request RAM/DoS surface (PR #1129 review,
        // finding 2); revisit with a streaming ingest path if producers
        // outgrow it.
        .route(
            "/indexes/{id}/graph",
            post(ingest_graph_handler)
                .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
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
        .route(
            "/indexes/{id}/config",
            get(index_config_handler).patch(patch_index_config_handler),
        )
        .route("/indexes/{id}/graph", get(graph_handler))
        .route("/indexes/{id}/graph/stats", get(graph_stats_handler))
        .route(
            "/indexes/{id}/graph/neighbors",
            get(graph_neighbors_handler),
        )
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

    // #3304: router-wide same-origin write guard, applied AFTER all route
    // registration so every destructive route (incl. any merged in) is covered.
    trusty_common::server::with_guarded_middleware(router, self_origins)
}
