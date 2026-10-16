//! The daemon's state, tickers and transport-neutral report cores.
//!
//! Why: Single shared `SearchAppState` (wrapped in `Arc`) lets every caller
//! read from the `IndexRegistry` concurrently. `DashMap` shard-locks per index
//! so different indexes never contend, and `Arc<RwLock<CodeIndexer>>` allows
//! many simultaneous readers per index.
//!
//! What: This module is a thin facade that declares submodules and re-exports
//! the public surface. The socket (`service::rpc`) serves the `*_report`
//! cores. #9214 (ruling D1): the daemon binds no TCP listener; #9535 deleted
//! the test-only axum router. The `cfg(test)` handlers that remain are called
//! directly by tests.
//!
//! Test: `cargo test -p trusty-search` drives the report cores and the
//! remaining handlers against an in-process registry.

mod admin;
mod components;
mod contrib_graph;
mod create_layout;
// #8167/#8232: a delete closes the index's redb and HNSW files first.
mod delete_close;
// #4087: query-time guard so a corpus-failed index fails loudly instead of
// answering HTTP 200 with an empty result set.
mod degraded;
// #6380: the delete-time root-path re-check `search.index.delete` applies.
mod delete_guard;
mod embedding_pause;
#[cfg(test)]
#[path = "embedding_pause_tests.rs"]
mod embedding_pause_tests;
mod facet_route;
mod fanout;
// #9027: per-index deadline and one query embed for the all-index fan-out.
mod fanout_deadline;
// #9029: `search.file.get`, one indexed file and its diff.
mod file_get;
mod files;
// #9214 (D1): the `files` handlers, test-only; tests call them directly.
#[cfg(test)]
mod files_http;
// #8959/#9179: runs the symbol-graph rebuild single-file writes defer.
mod graph_refresh_ticker;
mod health;
pub(crate) mod helpers;
mod index_config;
// #5349: the single resolve-or-lazy-load path every index-scoped endpoint that
// can restore a cold-parked index routes through.
mod index_resolve;
mod indexes;
mod indexes_relocate;
// #7434: `POST /indexes/:id/roots` — the multi-root mutation.
mod indexes_roots;
mod lexical_lane;
// #6822: the scalar-precision backfill route.
// #9169 PR-B1: what a parked index answers, and its park/unpark side effects.
mod project_park;
#[cfg(test)]
mod project_park_tests;
mod quantize_handlers;
mod reindex_handlers;
// #9236: maps a remove-file path onto the stored index-relative key.
mod remove_path;
mod residency_sweep;
// #4289: the create-index-time containment guard over registered index roots.
mod root_overlap;
mod router;
mod routing;
mod search;
mod search_global;
mod state;
mod state_impl;
mod status;
mod tickers;
mod typeahead;
// #9535: the `search.typeahead` core's tests, moved in from `tests/typeahead.rs`.
#[cfg(test)]
mod typeahead_tests;
// #6699: the vector-lane health both `/indexes/:id/status` and `/indexes` report.
mod vector_health;
// #9027: warm every registered index ahead of an all-index search.
mod warm_all;

// cfg(test) sub-modules — each < 500 lines
#[cfg(test)]
mod collision_3993_tests;
#[cfg(test)]
mod facet_route_tests;
#[cfg(test)]
mod list_last_used_tests;
#[cfg(test)]
mod list_repo_identity_tests;
// #6699: vector-lane health on the index list, and its agreement with status.
#[cfg(test)]
mod list_vector_health_tests;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests_1073;
#[cfg(test)]
mod tests_2336;
#[cfg(test)]
mod tests_2984;
#[cfg(test)]
mod tests_allowlist_gate_767;
#[cfg(test)]
// #6822: the quantize route + the live-precision status field.
mod tests_quantize_6822;
#[cfg(test)]
mod tests_same_id_root_mismatch;
// #7434: the add-roots route, create-time `roots`, and the root list on status.
#[cfg(test)]
mod tests_7434_roots;
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
// #6380: a delete whose expected root moved must be refused, not applied.
#[cfg(test)]
mod tests_6380;
// #8438: `delete_data` removes the directory the registry names, per source.
#[cfg(test)]
mod tests_8438;
// #8499: indexing never modifies a tracked file; the index survives git clean.
#[cfg(test)]
mod tests_8499;
// #8499 round 2: store placement, registration claims, relocate vs reindex.
#[cfg(test)]
mod registration_8499_tests;
// #8147: `POST /indexes` honours `colocated: false`.
#[cfg(all(test, unix))]
mod colocated_8147_tests;
// #8777: a created index is stamped at the current schema version.
#[cfg(test)]
mod tests_schema_stamp_8777;
// #8499 round 3: enclosing work tree, busy store, relocate under the claim.
#[cfg(test)]
mod work_tree_8499_tests;
// #8148: `PATCH …/config {"vector": true}` is the embed-only catch-up trigger.
#[cfg(test)]
mod tests_8148;
// #8134: vectors restored over an empty corpus never register as ready.
#[cfg(test)]
mod tests_8134;
// #8105: a reindex of a write-quarantined index is refused, not queued.
#[cfg(test)]
mod tests_8105;
// #8883: a serve-only index refuses every reindex and gets no watcher.
#[cfg(test)]
mod serve_only_8883_tests;
// #8889: one reindex per index through the HTTP handler.
#[cfg(test)]
mod tests_8889;
// #8167: a delete releases the index's files while a handle clone survives.
#[cfg(test)]
mod tests_8167;
// #8664: a delete closes the files a queued deferred-embed job holds.
#[cfg(test)]
mod tests_8664;
// #8727: parked registrations are listed; an overlap 409 names its blocker.
#[cfg(test)]
mod tests_8727;
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
// #4289: the create-index-time root-containment guard.
#[cfg(test)]
mod tests_4289;
#[cfg(test)]
mod tests_829;
// #9828: the pid forwarder follows a lazy spawn, a recycle and a 0 gap.
#[cfg(test)]
mod tests_9828_pid_forwarder;
#[cfg(test)]
mod tests_chunks;
#[cfg(test)]
pub(crate) mod tests_components;
#[cfg(test)]
mod tests_contrib_graph;
#[cfg(test)]
mod tests_denylist;
#[cfg(test)]
mod tests_grep;
// #7674: the grep `glob` parameter must select the same files for two
// sibling directories, and a glob that selects none must say so.
#[cfg(test)]
mod tests_grep_glob_7674;
#[cfg(test)]
mod tests_health;
// #112 routing tests, split from `tests_health` for the line cap (#9684).
#[cfg(test)]
mod routing_mode_tests;
#[cfg(test)]
mod tests_health_chat;
#[cfg(test)]
mod tests_health_contention;
#[cfg(test)]
mod tests_health_degraded;
// #5927: corpus-open failure vs. any-lane failure counter semantics.
#[cfg(test)]
mod stage_failed_5927_tests;
// #6821: /health's resident-index cap and its provenance.
#[cfg(test)]
mod resident_index_cap_6821_tests;
#[cfg(test)]
mod tests_health_switchable;
#[cfg(test)]
mod tests_index;
#[cfg(test)]
mod tests_index_config;
// #2203: a search that drops rows after fusion must report how many and why.
#[cfg(test)]
mod tests_dropped_results;
// #7675: a search whose top hit was decided by the exact-match floor must say so.
#[cfg(test)]
mod tests_exact_match_7675;
#[cfg(test)]
mod tests_lexical_lane_9258;
// #5917: a search over an index whose corpus cannot be read must be refused.
#[cfg(test)]
mod tests_corpus_read_5917;
// #8266: a cold, narrow grep must not rehydrate the whole corpus.
#[cfg(test)]
mod tests_grep_cold_8266;
// #5068 / #5061 / #4787 / #4839: the index-routing + status-reporting cluster.
#[cfg(test)]
mod tests_index_routing;
// #8348: a failed query embed degrades to lexical instead of a 500.
#[cfg(test)]
mod tests_8348;
// #9027: the all-index fan-out's per-index deadline and one query embed.
#[cfg(test)]
mod tests_9027;
// #9235: status and search meta report BM25 corpus-cap truncation.
#[cfg(test)]
mod tests_9235;
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
pub use state::{DaemonEvent, DaemonTransport, ReconcileSummary, SearchAppState, WarmBootSummary};

use std::sync::Arc;

// #9214 (ruling D1): the handlers are test-only; tests reach them through
// `super::*`.
#[cfg(test)]
use admin::{admin_stop_handler, logs_tail_handler};
#[cfg(test)]
use files::{get_index_chunks_handler, global_grep_handler, grep_handler};
#[cfg(test)]
use health::health_handler;
#[cfg(test)]
use indexes::{create_index_handler, list_indexes_handler};
#[cfg(test)]
use reindex_handlers::reindex_handler;
#[cfg(test)]
use search::search_handler;
#[cfg(test)]
use search_global::global_search_handler;
#[cfg(test)]
use status::{graph_handler, index_status_handler};
use tickers::{
    spawn_disk_size_ticker, spawn_idle_chunk_eviction_ticker, spawn_memory_pressure_ticker,
    spawn_orphan_reaper_ticker, spawn_residency_sweep_ticker, spawn_status_ticker,
    spawn_watcher_idle_suspend_ticker,
};

// #6285: the one seam `service::socket` reads the health report through, so the
// socket and `GET /health` cannot report different things.
pub(crate) use health::health_report;

// #6285 slice 2: the read surface's transport-neutral bodies and the param
// types they take. `service::rpc::reads` registers one JSON-RPC method per
// entry here.
pub(crate) use admin::{config_report, ConfigResponse};
pub(crate) use contrib_graph::{graph_neighbors_report, NeighborsParams};
pub(crate) use files::{call_chain_report, index_chunks_report, CallChainParams};
pub(crate) use index_config::{index_config_report, IndexConfigView};
pub(crate) use indexes::{list_indexes_report, ListIndexesParams};
pub(crate) use status::{graph_report, graph_stats_report, index_status_report, GraphQueryParams};

// #6285 slice 3: the query surface's transport-neutral bodies. Same contract as
// the slice-2 block above — `service::rpc::queries` registers one JSON-RPC
// method per entry.
pub(crate) use files::{global_grep_report, grep_report};
pub(crate) use routing::search_similar_report;
pub(crate) use search::search_report;
pub(crate) use search_global::global_search_report;
pub(crate) use typeahead::{typeahead_report, TypeaheadParams};

// #6285 slice 4: the write surface's transport-neutral bodies and the request
// types they take. Same contract as the two blocks above, and it is what carries
// the failure arms across: every refusal these cores return is the ONE decision
// the socket renders, so a socket write cannot report a landing that did not
// happen.
pub(crate) use contrib_graph::{ingest_graph_report, IngestGraphRequest};
pub(crate) use embedding_pause::{pause_embedding_report, resume_embedding_report};
pub(crate) use files::{index_file_report, remove_file_report};
pub(crate) use indexes::create_index_report;
pub(crate) use indexes_relocate::{relocate_index_report, RelocateIndexRequest};
// #7434: the `search.index.roots.add` socket method.
pub(crate) use indexes_roots::{add_index_roots_report, AddRootsRequest};
// #6285 consumer move: the quantize backfill's socket twin, `search.index.quantize`.
pub(crate) use quantize_handlers::{quantize_report, QuantizeRequest};
// #9169 PR-B1: the socket's project-map methods drive these.
pub(crate) use project_park::{detach_parked, reindex_after_unpark, with_map_parked};
pub(crate) use reindex_handlers::reindex_report;
#[cfg(test)]
pub(crate) use reindex_handlers::start_release_catch_up;
pub(crate) use search::{delete_index_report, DeleteIndexParams};

// #6285 slice 5.5: the four routes with a named consumer that no earlier slice
// claimed. Same contract as the blocks above; the two config cores are WRITES
// and carry the slice-4 failure-arm bar with them.
pub(crate) use admin::{
    admin_stop_report, logs_tail_report, patch_config_report, PatchConfigRequest,
};
pub(crate) use index_config::{patch_index_config_report, PatchIndexConfigRequest};

// #9027: warm-all, served on the socket.
pub(crate) use warm_all::{warm_start_report, warm_status_report};
// #9029: the socket-only file read.
pub(crate) use file_get::{file_get_report, FileGetParams, FILE_GET_MAX_CONCURRENT};
pub use warm_all::{WarmStartRequest, WarmState, WarmTracker};

/// Spawn every background ticker the daemon runs over `state_arc`.
///
/// Why (#9214): the tickers used to start only as a side effect of building
/// the HTTP router. The daemon builds no router, so `run_daemon` starts them
/// here, or it would serve the socket with no residency sweep, no
/// memory-pressure enforcement and no status events.
/// What: the nine `spawn_*` calls the deleted router made inline. Each holds a
/// `Weak`, so the tickers stop once the last `Arc` drops.
/// Test: `run_daemon_serves_only_the_socket` waits for a `status_changed`
/// event from a daemon that built no router.
pub fn spawn_daemon_tickers(state_arc: &Arc<SearchAppState>) {
    spawn_status_ticker(Arc::clone(state_arc));
    spawn_disk_size_ticker(Arc::clone(state_arc));
    spawn_idle_chunk_eviction_ticker(Arc::clone(state_arc));
    spawn_watcher_idle_suspend_ticker(Arc::clone(state_arc));
    spawn_orphan_reaper_ticker(Arc::clone(state_arc));
    spawn_residency_sweep_ticker(Arc::clone(state_arc));
    spawn_memory_pressure_ticker(Arc::clone(state_arc));
    graph_refresh_ticker::spawn_graph_refresh_ticker(Arc::clone(state_arc));
    // #4250: drive indexes parked by a warm-boot restore timeout back into the
    // registry. Nothing else will — they are absent from `list_indexes`, so a
    // client that discovers indexes by listing never names them, and boot
    // reconcile walks registered handles only.
    crate::service::timeout_recovery::spawn_timeout_recovery_ticker(Arc::clone(state_arc));
}
