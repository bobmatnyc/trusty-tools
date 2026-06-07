//! Index-status and symbol-graph handlers.
//!
//! Why: `GET /indexes/:id/status` and `GET /indexes/:id/graph[/stats]` are
//! read-only inspectors that share disk-mtime helpers; grouping them keeps
//! both the handlers and helpers together.
//! What: `index_disk_and_mtime`, `first_existing_mtime_rfc3339`,
//! `index_status_handler`, `graph_handler`, `graph_stats_handler`.
//! Test: `index_disk_and_mtime_handles_missing_dir`,
//! `graph_handler_exports_nodes_and_edges`, etc.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use std::sync::Arc;

use crate::core::registry::IndexId;
use crate::service::reindex::ReindexStatus;

use super::state::SearchAppState;

pub(super) fn index_disk_and_mtime(index_id: &str) -> (Option<u64>, Option<String>) {
    // Why: `persistence::index_data_dir` creates the directory as a side effect,
    // which would defeat the "missing dir → None" contract this helper relies
    // on. Compute the path manually (mirroring the persistence layer's logic)
    // and only touch the filesystem to *read* metadata.
    let Ok(data_dir) = crate::service::persistence::data_dir() else {
        return (None, None);
    };
    let dir = data_dir
        .join("indexes")
        .join(crate::service::persistence::sanitize_id_for_path(index_id));
    if !dir.exists() {
        return (None, None);
    }
    let disk_bytes = Some(trusty_common::sys_metrics::dir_size_bytes(&dir));
    // Issue #80: after the redb cutover (issue #28), `chunks.json` is no
    // longer rewritten on every commit — the durable corpus lives in
    // `index.redb`. The previous implementation read `chunks.json` mtime
    // unconditionally and returned `null` for every post-cutover index,
    // making `last_indexed` permanently stale.
    //
    // Why: callers (admin UI, MCP `index_status`) rely on this field to
    // show "indexed N seconds ago"; a permanent null hides actual freshness.
    // What: probe `index.redb` first (current authoritative file rewritten
    // by every redb commit / atomic swap), then fall back to `chunks.json`
    // for un-migrated indexes (the legacy JSON snapshot still rewritten by
    // the migration shim). The first existing file wins.
    // Test: `index_disk_and_mtime_handles_missing_dir` (this fn) +
    // `last_indexed_prefers_redb_then_chunks_json` (the pure selector below).
    let last_indexed = first_existing_mtime_rfc3339(&dir, &["index.redb", "chunks.json"]);
    (disk_bytes, last_indexed)
}

/// Return the modification time (as an RFC3339 string) of the first file in
/// `candidates` that exists under `dir`.
///
/// Why (issue #80): after the redb cutover (issue #28) `chunks.json` is no
/// longer rewritten on every commit, so reading its mtime returned `null`
/// for every migrated index. The freshness signal must prefer the current
/// authoritative file (`index.redb`, rewritten by every redb commit / atomic
/// swap) and only fall back to the legacy JSON snapshot for un-migrated
/// indexes. Extracting the selection into a pure function (path in, optional
/// string out) makes the precedence rule unit-testable without mutating the
/// process-wide data-dir env vars that `index_disk_and_mtime` depends on.
/// What: probes each candidate filename in order, returns the RFC3339-encoded
/// mtime of the first one that exists and whose metadata is readable, or
/// `None` when none exist.
/// Test: `last_indexed_prefers_redb_then_chunks_json` and
/// `last_indexed_none_when_no_candidates_exist`.
pub(super) fn first_existing_mtime_rfc3339(
    dir: &std::path::Path,
    candidates: &[&str],
) -> Option<String> {
    candidates
        .iter()
        .find_map(|name| std::fs::metadata(dir.join(name)).ok())
        .and_then(|m| m.modified().ok())
        .map(|t| {
            let dt: chrono::DateTime<chrono::Utc> = t.into();
            dt.to_rfc3339()
        })
}

pub(super) async fn index_status_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let index_id = IndexId::new(id);
    let handle = state.registry.get(&index_id).ok_or(StatusCode::NOT_FOUND)?;
    let indexer = handle.indexer.read().await;
    // Issue #111: surface `path_filter` so callers can see which glob filter
    // (if any) is active for the index. Returns `null` when no filter is set.
    let path_filter = if handle.path_filter.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Array(
            handle
                .path_filter
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        )
    };
    // Issue #112: surface whether a context embedding has been computed
    // for this index, plus the truncated human-readable summary that
    // produced it. Helps operators verify metadata scraping found a
    // recognised file.
    let has_context_embedding = handle.context_embedding.read().await.is_some();
    let context_summary = handle
        .context_summary
        .read()
        .await
        .clone()
        .map(serde_json::Value::String)
        .unwrap_or(serde_json::Value::Null);
    // Issue #38: surface per-index on-disk footprint + last-indexed time for
    // the admin UI's enhanced Indexes table. Both are derived from the
    // per-index data directory; absent / unreadable values degrade to null
    // so a fresh (never-reindexed) index still returns a 200.
    let (disk_bytes, disk_last_indexed) = index_disk_and_mtime(&index_id.0);
    // Issue #878: prefer the in-memory `last_indexed_at` timestamp stamped
    // at reindex-complete time. This is authoritative regardless of storage
    // layout (legacy global dir vs. colocated `.trusty-search/`) and is
    // always non-null after a successful reindex in this daemon session.
    // Fall back to the disk-mtime heuristic for warm-booted indexes whose
    // `last_indexed_at` was not yet populated (i.e. indexed before the fix
    // or not yet reindexed since the last daemon restart).
    let in_memory_last_indexed = handle.last_indexed_at.read().await.clone();
    let last_indexed = in_memory_last_indexed.or(disk_last_indexed);
    // Issue #80: surface a coarse lifecycle status. The legacy top-level
    // `status` field stays for back-compat — it collapses to `indexing` while
    // any reindex task is running and `ready` otherwise (mirrors the v0.8.x
    // contract). Callers wanting per-stage granularity should consult the
    // `stages` block introduced in v0.9.0 (issue #109, Phase 1) — that field
    // tracks lexical → semantic → graph progress and grows
    // `search_capabilities` as each lane comes online.
    let legacy_status = match state
        .reindex_progress
        .get(&index_id)
        .map(|p| p.status.load())
    {
        Some(ReindexStatus::Running) => "indexing",
        _ => "ready",
    };
    // Issue #109 Phase 1: snapshot the staged-pipeline state so the response
    // can surface per-stage status and derive the public `search_capabilities`
    // array. The legacy `status` field stays at the top level, but
    // integrators wanting "is the vector lane ready" should consult
    // `search_capabilities`.
    let stages_snapshot = handle.stages.read().await.clone();
    let search_capabilities = stages_snapshot.search_capabilities();
    // Issue #100: surface budget-truncation so callers can flag indexes that
    // hit the `TRUSTY_MAX_CHUNKS` cap during the last reindex. Defaults to
    // `false` / `0` when no `ReindexProgress` entry exists (i.e. the index
    // was warm-booted from disk and hasn't been reindexed in this daemon
    // session — exactly the back-compat case the task spec calls out).
    let (walk_truncated_by_budget, chunks_dropped_by_cap) = state
        .reindex_progress
        .get(&index_id)
        .map_or((false, 0), |p| {
            let n = p
                .chunks_dropped_by_cap
                .load(std::sync::atomic::Ordering::Acquire);
            (n > 0, n)
        });
    // Issue #280: snapshot the walk diagnostics so operators can diagnose
    // zero-chunk indexes without reading daemon logs.  Use `clone()` so the
    // read lock is released before we build the JSON response.
    let walk_diag = handle.walk_diagnostics.read().await.clone();
    // Issue #681: prefer durable corpus count; in-memory map returns 0 after
    // idle eviction (TRUSTY_CHUNKS_IDLE_EVICT_SECS default 300s). Falls back to
    // in-memory for BM25-only / test indexers that have no corpus wired.
    let chunk_count = indexer
        .corpus_arc()
        .and_then(|c| c.chunk_count().ok())
        .unwrap_or_else(|| indexer.chunk_count());
    Ok(Json(serde_json::json!({
        "index_id": index_id.0,
        "root_path": handle.root_path,
        "chunk_count": chunk_count,
        "status": legacy_status,
        "stages": stages_snapshot,
        "search_capabilities": search_capabilities,
        "lexical_only": handle.lexical_only,
        "skip_kg": handle.skip_kg,
        "path_filter": path_filter,
        "has_context_embedding": has_context_embedding,
        "context_summary": context_summary,
        "disk_bytes": disk_bytes,
        "last_indexed": last_indexed,
        "respect_gitignore": handle.respect_gitignore,
        "walk_truncated_by_budget": walk_truncated_by_budget,
        "chunks_dropped_by_cap": chunks_dropped_by_cap,
        // Issue #280: walk diagnostic fields.
        "last_walk_started_at": walk_diag.last_walk_started_at,
        "last_walk_files_seen": walk_diag.last_walk_files_seen,
        "last_walk_files_skipped": walk_diag.last_walk_files_skipped,
        "last_walk_error": walk_diag.last_walk_error,
    })))
}

/// Optional query parameters for `GET /indexes/{id}/graph` (issue #128).
///
/// Why: a full KG export on a large repo can be tens of thousands of nodes;
/// D3/Cytoscape clients usually want a filtered subgraph. These let the caller
/// narrow the export server-side instead of shipping the whole graph.
/// What: all fields optional; absent params apply no filter.
/// Test: covered by `test_graph_handler_filters` in `tests/integration_tests.rs`.
#[derive(Debug, Default, serde::Deserialize)]
pub(super) struct GraphQueryParams {
    /// Comma-separated node `type` values to keep (e.g. `Symbol,File`).
    pub(super) types: Option<String>,
    /// Comma-separated `EdgeKind` display names to keep (e.g.
    /// `CallsFunction,Implements`).
    pub(super) edge_types: Option<String>,
    /// Minimum edge weight; edges below this are dropped.
    pub(super) min_weight: Option<f32>,
}

/// Parse a comma-separated filter param into a trimmed, lower-cased set.
///
/// Why: both the node-type and edge-type filters accept comma lists and are
/// matched case-insensitively; this keeps the parsing in one place.
/// What: returns `None` when the param is absent or empty (meaning "no
/// filter"), otherwise the set of non-empty lower-cased tokens.
/// Test: exercised via `graph_handler` integration tests.
fn parse_filter_set(raw: Option<&str>) -> Option<std::collections::HashSet<String>> {
    let raw = raw?;
    let set: std::collections::HashSet<String> = raw
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if set.is_empty() {
        None
    } else {
        Some(set)
    }
}

/// Derive the D3/Cytoscape node `type` from a symbol name.
///
/// Why: `SymbolNode` carries no richer type metadata yet (issue #128 note), so
/// the endpoint infers a coarse type from the name shape.
/// What: returns `"File"` when the symbol looks like a file path (contains a
/// `/` and has a file extension), otherwise `"Symbol"`.
/// Test: covered indirectly by `graph_handler` integration tests.
fn node_type_for_symbol(symbol: &str) -> &'static str {
    let looks_like_path = symbol.contains('/')
        && std::path::Path::new(symbol)
            .extension()
            .is_some_and(|e| !e.is_empty());
    if looks_like_path {
        "File"
    } else {
        "Symbol"
    }
}

/// `GET /indexes/{id}/graph` — export the full SymbolGraph as D3/Cytoscape JSON.
///
/// Why: issue #128 — external visualisers (and the admin UI) need the whole
/// knowledge graph, not just the BFS-scoped neighbours the search pipeline
/// uses. This endpoint snapshots the graph and serialises every node and edge.
/// What: snapshots the symbol graph (lock-free after the `Arc` clone), applies
/// the optional `types` / `edge_types` / `min_weight` filters, and returns
/// `{ nodes, edges, stats, generated_at }`. A 1-hour `Cache-Control` header is
/// attached since the graph only changes on reindex.
/// Test: covered by `test_graph_handler_*` in `tests/integration_tests.rs`.
pub(super) async fn graph_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Query(params): Query<GraphQueryParams>,
) -> Result<Response, StatusCode> {
    let index_id = IndexId::new(id);
    let handle = state.registry.get(&index_id).ok_or(StatusCode::NOT_FOUND)?;
    let graph = {
        let indexer = handle.indexer.read().await;
        indexer.snapshot_symbol_graph().await
    };

    let type_filter = parse_filter_set(params.types.as_deref());
    let edge_filter = parse_filter_set(params.edge_types.as_deref());
    let min_weight = params.min_weight.unwrap_or(f32::MIN);

    // Build node list, tracking which symbols survive the type filter so we
    // can drop edges that reference filtered-out endpoints.
    let mut kept_symbols: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut nodes: Vec<serde_json::Value> = Vec::new();
    for (symbol, chunk_id, file) in graph.all_nodes() {
        let node_type = node_type_for_symbol(&symbol);
        if let Some(ref filter) = type_filter {
            if !filter.contains(&node_type.to_ascii_lowercase()) {
                continue;
            }
        }
        kept_symbols.insert(symbol.clone());
        nodes.push(serde_json::json!({
            "id": chunk_id,
            "type": node_type,
            "label": symbol,
            "metadata": { "file": file, "symbol": symbol },
        }));
    }

    let mut edges: Vec<serde_json::Value> = Vec::new();
    for (source, target, kind) in graph.all_edges() {
        // Drop edges whose endpoints were filtered out by the type filter.
        if type_filter.is_some()
            && (!kept_symbols.contains(&source) || !kept_symbols.contains(&target))
        {
            continue;
        }
        let kind_name = format!("{kind:?}");
        if let Some(ref filter) = edge_filter {
            if !filter.contains(&kind_name.to_ascii_lowercase()) {
                continue;
            }
        }
        let weight = kind.score_multiplier();
        if weight < min_weight {
            continue;
        }
        edges.push(serde_json::json!({
            "source": source,
            "target": target,
            "type": kind_name,
            "weight": weight,
        }));
    }

    let body = serde_json::json!({
        "nodes": nodes,
        "edges": edges,
        "stats": {
            "node_count": graph.node_count(),
            "edge_count": graph.edge_count(),
        },
        "generated_at": chrono::Utc::now().to_rfc3339(),
    });

    let mut response = Json(body).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("max-age=3600"),
    );
    Ok(response)
}

/// `GET /indexes/{id}/graph/stats` — symbol-graph summary statistics
/// (issue #41 phase 2).
///
/// `GET /indexes/{id}/graph/stats` — symbol-graph summary statistics
/// (issue #41 phase 2).
///
/// Why: lets agents and dashboards verify KG health (total nodes/edges plus a
/// per-`EdgeKind` breakdown) without parsing the much larger `/graph` export
/// or scraping Prometheus. The Phase B/C edge counts here are the
/// load-bearing signal that the entity-derived edges are actually wired.
/// What: snapshots the symbol graph (lock-free after the `Arc` clone) and
/// returns `{ node_count, edge_count, edge_kinds: { CallsFunction: …, … } }`.
/// Returns 404 when the index id is unknown.
/// Test: covered by `graph_stats_handler_returns_breakdown` in this module.
pub(super) async fn graph_stats_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let index_id = IndexId::new(id);
    let handle = state.registry.get(&index_id).ok_or(StatusCode::NOT_FOUND)?;
    let graph = {
        let indexer = handle.indexer.read().await;
        indexer.snapshot_symbol_graph().await
    };
    let breakdown = graph.edge_kind_breakdown();
    let mut edge_kinds = serde_json::Map::with_capacity(breakdown.len());
    for (tag, count) in breakdown {
        edge_kinds.insert(tag, serde_json::Value::from(count));
    }

    Ok(Json(serde_json::json!({
        "node_count": graph.node_count(),
        "edge_count": graph.edge_count(),
        "edge_kinds": serde_json::Value::Object(edge_kinds),
    })))
}
