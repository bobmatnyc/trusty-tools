//! Single-index search and delete handlers.
//!
//! Why: Groups the per-index search and delete paths together. Global fan-out
//! search lives in `search_global.rs` (extracted to keep both files under the
//! 500-line cap after issue #993 added cold-store lazy-load logic here).
//! What: `delete_index_handler`, `search_handler`. Routing helpers
//! (`RoutingMode`, `compute_context_weights`) and `search_similar_handler`
//! live in `routing.rs`. Global fan-out lives in `search_global.rs`.
//! Test: `search_handler_meta_includes_stale_index_root_field` and related.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use std::sync::Arc;

use crate::core::{classifier::QueryClassifier, indexer::SearchQuery, registry::IndexId};
use crate::service::lazy_loader::{
    cold_reload_timeout, get_or_load_index, LazyLoadError, LAST_QUERIED_WRITE_INTERVAL_SECS,
};
use crate::service::lazy_restore::restore_index_on_demand;
use crate::service::warm_boot::restore_one_index_bounded;

use super::helpers::file_is_within_root;
use super::state::{DaemonEvent, SearchAppState};
use super::status::index_disk_and_mtime;

// Re-export global fan-out handler so the router in `mod.rs` can reach it
// through the `search` path without knowing about `search_global`.
pub(super) use super::search_global::global_search_handler;

pub(super) async fn delete_index_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let index_id = IndexId::new(id.clone());
    // Issue #1090 / #1097 atomicity: capture root_path and unregister in a
    // single DashMap `remove` so a concurrent PATCH cannot make the captured
    // root_path stale before the roots.toml cleanup below.
    let (removed, removed_handle) = state.registry.remove_and_get(&index_id);
    let root_path_for_cleanup = removed_handle.map(|h| h.root_path.clone());
    state.reindex_progress.remove(&index_id);
    if removed {
        // Issue #85: drop the on-disk footprint so the index doesn't come
        // back on the next daemon restart. Best-effort — log on failure.
        if let Err(e) = crate::service::persistence::remove_index_registry_entry(&id) {
            tracing::warn!("could not remove '{id}' from indexes.toml: {e}");
        }
        if let Err(e) = crate::service::persistence::remove_index_data_dir(&id) {
            tracing::warn!("could not remove on-disk data for '{id}': {e}");
        }
        // Issue #1090: remove the root from roots.toml so the warm-boot
        // colocated scan does not rediscover this root and resurrect the index.
        // Without this, roots.toml retains the entry and warm-boot re-registers
        // the deleted index from the leftover `.trusty-search/` data dir.
        if let Some(ref root) = root_path_for_cleanup {
            if let Err(e) = crate::service::roots_registry::remove_root(root) {
                tracing::warn!(
                    "could not remove '{id}' root {} from roots.toml: {e} \
                     (warm-boot may rediscover this index — issue #1090)",
                    root.display()
                );
            } else {
                tracing::debug!(
                    "delete[{id}]: removed root {} from roots.toml",
                    root.display()
                );
            }
        }
        // Push event so connected dashboards drop the row without refresh.
        state.emit(DaemonEvent::IndexRemoved { id: id.clone() });
        // Issue #41 Phase 1: keep the index-count gauge in sync.
        crate::service::metrics::set_index_count(state.registry.list().len());
    }
    Json(serde_json::json!({ "id": id, "removed": removed }))
}

pub(super) async fn search_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(mut query): Json<SearchQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    // Issue #882: reject empty / whitespace-only queries before touching the
    // index. An empty query falls through to a pure k-NN vector search that
    // returns arbitrary top-k results — not useful and potentially expensive.
    if query.text.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "query must not be empty" })),
        ));
    }
    let index_id = IndexId::new(id);
    // Issue #993: try hot registry first, fall back to cold-store lazy load.
    // Short-circuit with 503 when the embedder has not finished initializing:
    // `restore_index_on_demand` requires a live embedder to rebuild the HNSW
    // lane. Callers should retry once `/health` reports `embedder: "ready"`.
    let handle = {
        // Try hot path first — avoids the embedder check for warm indexes.
        if let Some(h) = state.registry.get(&index_id) {
            h
        } else if !state.cold_store.contains(&index_id) {
            return Err((
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": format!("unknown index: {}", index_id.0) })),
            ));
        } else {
            // Cold index: need the embedder to restore.
            let Some(embedder) = state.current_embedder().await else {
                return Err((
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "error": "embedder_initializing",
                        "message": "embedder not yet ready — retry after /health reports embedder:ready",
                    })),
                ));
            };
            let s = Arc::clone(&state);
            let load_result = get_or_load_index(
                &index_id,
                &state.registry,
                &state.cold_store,
                cold_reload_timeout(),
                move |entry| async move {
                    let e = Arc::clone(&embedder);
                    restore_one_index_bounded(entry, move |en| async move {
                        restore_index_on_demand(&s, &e, en).await;
                    })
                    .await
                },
            )
            .await;
            match load_result {
                Ok(h) => h,
                Err(LazyLoadError::NotFound) => {
                    return Err((
                        StatusCode::NOT_FOUND,
                        Json(serde_json::json!({
                            "error": format!("unknown index: {}", index_id.0),
                        })),
                    ));
                }
                Err(LazyLoadError::Loading { retry_after_secs }) => {
                    return Err((
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "error": "index_loading",
                            "retry_after_secs": retry_after_secs,
                        })),
                    ));
                }
            }
        }
    };
    // Issue #993: rate-limited write of last_queried_unix (max once per
    // LAST_QUERIED_WRITE_INTERVAL_SECS) so the LRU sort key stays current for
    // future selective warm-boots without hammering indexes.toml on every query.
    //
    // PR #1103 PERF: the previous code called `persistence::read_last_queried_unix`
    // here, which opens + parses indexes.toml synchronously on the async handler
    // for EVERY query to a warm index. Replace with the in-memory
    // `last_queried_write_cache` DashMap so the hot path does zero disk I/O.
    // The background write task below updates the map after a successful write so
    // the rate-limit semantics are fully preserved.
    {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Consult the in-memory cache — no disk read.
        let stale = state
            .last_queried_write_cache
            .get(&index_id)
            .map(|prev| now_unix.saturating_sub(*prev) >= LAST_QUERIED_WRITE_INTERVAL_SECS)
            .unwrap_or(true); // absent = never written for this session → write now
        if stale {
            let id_str = index_id.0.clone();
            // Update the in-memory cache immediately so concurrent queries within
            // the same interval don't all race to spawn a write task.
            state
                .last_queried_write_cache
                .insert(index_id.clone(), now_unix);
            tokio::spawn(async move {
                if let Err(e) =
                    crate::service::persistence::update_last_queried_unix(&id_str, now_unix)
                {
                    tracing::debug!("last_queried_unix update failed for '{id_str}': {e}");
                }
            });
        }
    }
    // Use the same domain-aware classifier as `CodeIndexer::search` so the
    // intent reported back to the caller matches what was used for routing.
    let intent = QueryClassifier::classify_with_domain(&query.text, &handle.domain_terms);
    // Issue #109 Phase 1: derive lane availability from the staged-pipeline
    // status surface. The search handler MUST consult `search_capabilities`
    // (NOT the legacy top-level `status` field) when deciding whether the
    // semantic / KG lanes are queryable. The indexer's `search` honours
    // `query.stage = Some(Lexical)`, so we down-shift the query to lexical
    // when either (a) the caller explicitly asked for it, or (b) the
    // semantic stage is not yet ready. Doing this here keeps the indexer
    // unaware of the index-handle-level capability surface.
    let caps = { handle.stages.read().await.search_capabilities() };
    let semantic_ready = caps.contains(&"vector");
    if query.stage.is_none() && !semantic_ready {
        // Force lexical lane until the embedder catches up. The caller's
        // request is preserved if they explicitly asked for `mode = all`
        // / similar; we only override the lane selector, not the file-type
        // filter.
        query.stage = Some(crate::core::indexer::SearchStage::Lexical);
    }
    // Issue #109 Phase 1 backpressure stub: ping the per-index pressure
    // notifier so the background Stage-2 task briefly yields. The notifier
    // is a hint — the embedder loop waits at most 100 ms.
    handle.search_pressure.notify_one();
    let started = std::time::Instant::now();
    let indexer = handle.indexer.read().await;
    let mut results = indexer.search(&query).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "internal search error" })),
        )
    })?;
    // Issue #64: defense-in-depth post-filter. Chunks are stored with `file`
    // paths relative to the index root, so anything that escapes the root
    // (absolute path pointing elsewhere, `..` traversal, or simply a path
    // that's also absolute and outside `root_path`) is a sign of stale data
    // from a previously-misregistered index (see #63) or a bug elsewhere in
    // the pipeline. Drop those rows rather than returning cross-project
    // results to the caller. `file_is_within_root` uses a cheap lexical
    // check first; only absolute-path results that fail the fast path pay the
    // `canonicalize` syscall cost (issue #541 approach b).
    let root = handle.root_path.clone();
    let before = results.len();
    results.retain(|r| file_is_within_root(&r.file, &root));
    let filtered_out = before.saturating_sub(results.len());
    if filtered_out > 0 {
        // Issue #541: increment the process-wide Prometheus counter so operators
        // can alert on a rising drop rate without log scraping.
        metrics::counter!(
            "trusty_search_dropped_out_of_root_total",
            "index_id" => index_id.0.clone(),
        )
        .increment(filtered_out as u64);
        tracing::warn!(
            index_id = %index_id,
            root = %root.display(),
            dropped = filtered_out,
            "search_handler: dropped {} result(s) whose file path falls outside index root {} \
             — index root is stale (symlink rename or daemon restart without \
             re-canonicalization). Re-register to fix: `trusty-search index {}`",
            filtered_out,
            root.display(),
            root.display(),
        );
    }
    drop(indexer);

    let latency_ms = started.elapsed().as_millis() as u64;
    tracing::info!(
        index_id = %index_id,
        intent = %format!("{intent:?}"),
        latency_ms = latency_ms,
        results = results.len(),
        query = %&query.text[..query.text.len().min(80)],
        "search"
    );

    // Issue #75: surface index freshness in the response `meta` block so
    // callers can show staleness banners without a follow-up status call.
    //
    // `last_indexed` is the mtime of `chunks.json` (rewritten on every
    // successful commit) and matches what `GET /indexes/:id/status`
    // already returns.
    //
    // `results_may_be_stale` compares the current git HEAD SHA against the
    // SHA captured at index-registration time. False whenever either SHA
    // is unavailable (non-git directory, missing git binary) or the SHAs
    // match — i.e. defaults to "not stale" rather than scaring callers
    // about indexes whose freshness we cannot verify.
    let (_disk_bytes, last_indexed) = index_disk_and_mtime(&index_id.0);
    let indexed_sha = handle.indexed_head_sha.read().await.clone();
    let current_sha = crate::core::git::head_sha(&handle.root_path);
    let results_may_be_stale = match (indexed_sha.as_deref(), current_sha.as_deref()) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    };
    Ok(Json(serde_json::json!({
        "results": results,
        "intent": format!("{:?}", intent),
        "latency_ms": latency_ms,
        "meta": {
            "last_indexed": last_indexed,
            "results_may_be_stale": results_may_be_stale,
            // Issue #109 Phase 1: surface which lanes contributed to this
            // result set. Lets clients display "lexical-only" badges or
            // retry once the semantic lane is ready.
            "search_capabilities": caps,
            // Issue #541: machine-readable signal that results were dropped
            // because the index root is stale. Clients (Claude Code, UI) can
            // show a remediation banner without log scraping. `false` is the
            // normal case (no drops); `true` means the operator should run
            // `trusty-search index <path>` to re-register with a fresh root.
            "stale_index_root": filtered_out > 0,
        },
    })))
}
