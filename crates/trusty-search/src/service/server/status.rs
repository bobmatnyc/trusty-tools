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

/// On-disk footprint and freshness for one index, across BOTH storage layouts.
///
/// Why (#4706): this used to measure only the legacy global registry dir
/// (`<data_dir>/indexes/<id>/`). Since #403 an index's live corpus normally
/// lives colocated at `<root_path>/.trusty-search/`, and the global dir holds
/// metadata only. Because that global dir still EXISTS, the old code took the
/// `Some(dir_size_bytes(..))` branch and reported `0` — not `null` — for a
/// populated index: a healthy 527 MB / 71,433-chunk index read as empty. An
/// operator diagnosed 11 such indexes as broken and considered deleting them.
/// A metric that reads `0` for a half-gigabyte index is dangerous, not merely
/// inaccurate. The documented contract was already "sum of all file sizes under
/// the index data directory" (`router::IndexDetailEntry`), so reading one of
/// the two locations was the bug, not the contract.
/// What: sums whichever of the two directories exist. `None` only when NEITHER
/// does — that is the sole "nothing on disk yet" signal, and it is now reachable
/// only in the state it actually describes. `root_path` is required because the
/// colocated location is derived from it; every caller already holds the handle.
/// Uses a manual path join rather than `colocated_storage::colocated_storage_dir`
/// for the same reason it avoids `persistence::index_data_dir` — both create the
/// directory as a side effect, which would make this read-only metric fabricate
/// the very directory whose absence it reports.
/// Test: `index_disk_and_mtime_handles_missing_dir`,
/// `disk_bytes_sums_colocated_storage_not_just_the_legacy_dir`,
/// `disk_bytes_is_none_only_when_neither_layout_exists`.
pub(super) fn index_disk_and_mtime(
    index_id: &str,
    root_path: &std::path::Path,
) -> (Option<u64>, Option<String>) {
    let dirs = index_storage_dirs(index_id, root_path);
    if dirs.is_empty() {
        return (None, None);
    }
    // #4706: sum, don't pick. A migrated index legitimately holds bytes in both
    // places; reporting either one alone understates it.
    let disk_bytes = Some(
        dirs.iter()
            .map(|d| trusty_common::sys_metrics::dir_size_bytes(d))
            .sum(),
    );
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
    // the migration shim). The first existing file wins WITHIN a directory;
    // #4706 then takes the newest across the two directories, since a
    // colocated index's writes land in `.trusty-search/` and the stale global
    // copy must not win. This is why `IndexHandle::last_indexed_at` (#878)
    // existed as an in-memory workaround — it stays, since it is still the
    // only non-null source for a handle that has not yet written to disk.
    // Test: `index_disk_and_mtime_handles_missing_dir` (this fn) +
    // `last_indexed_prefers_redb_then_chunks_json` (the pure selector below) +
    // `last_indexed_takes_the_newer_of_the_two_layouts`.
    (disk_bytes, newest_mtime(&dirs))
}

/// The directories that hold one index's on-disk data, most-authoritative last.
///
/// Why (#4706): the two storage layouts — legacy global
/// `<data_dir>/indexes/<id>/` and colocated `<root_path>/.trusty-search/`
/// (#403) — are resolved identically for the size, freshness, and any future
/// metric, so they are resolved in exactly one place.
/// What: returns only the directories that EXIST; empty means nothing is on
/// disk. Both lookups are read-only by construction — `persistence::index_data_dir`
/// and `colocated_storage::colocated_storage_dir` each call `create_dir_all`, so
/// neither is used here; the legacy path is joined manually and the colocated
/// one goes through `has_colocated_storage`, the crate's read-only existence
/// check.
///
/// The colocated directory additionally must contain `index.redb` to count.
/// Without that gate a home-rooted index would absorb the daemon's OWN runtime
/// directory: `$HOME/.trusty-search/` holds `http_addr` and `mcp_http_addr`, so
/// an index rooted at `$HOME` would report the daemon's runtime files as its
/// corpus. `index.redb` is what makes a `.trusty-search/` a corpus rather than
/// a coincidence of naming. The cost is that a torn colocated directory holding
/// only `hnsw.usearch` reads as absent — an undercount in a state that is
/// already broken, versus a wrong count in a healthy one.
/// Test: `disk_bytes_sums_colocated_storage_not_just_the_legacy_dir`,
/// `colocated_dir_without_a_redb_is_not_counted_as_a_corpus`.
fn index_storage_dirs(index_id: &str, root_path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::with_capacity(2);
    if let Ok(data_dir) = crate::service::persistence::data_dir() {
        let legacy = data_dir
            .join("indexes")
            .join(crate::service::persistence::sanitize_id_for_path(index_id));
        if legacy.is_dir() {
            dirs.push(legacy);
        }
    }
    // #4706: `.trusty-search/` alone is not proof of a corpus — see the gate
    // rationale above.
    if crate::service::colocated_storage::has_colocated_storage(root_path) {
        let colocated = root_path.join(crate::service::colocated_storage::COLOCATED_DIR_NAME);
        if colocated.join("index.redb").is_file() {
            dirs.push(colocated);
        }
    }
    dirs
}

/// Freshness only, skipping the directory-size walk (#4706 review).
///
/// Why: `search_handler` needs `last_indexed` for its response `meta` on EVERY
/// query and discards the byte count. Routing it through `index_disk_and_mtime`
/// made each search pay a blocking, recursive `dir_size_bytes` walk of both
/// storage directories — on a large index that is a walk of the entire redb +
/// HNSW tree, per query, for a number nobody reads. #4706 doubled the cost by
/// adding the second directory.
/// What: the same directory resolution and the same newest-mtime rule as
/// `index_disk_and_mtime`, minus the size walk — two `stat` calls per
/// directory rather than a full traversal.
/// Test: `last_indexed_only_matches_the_mtime_from_the_full_helper`.
pub(super) fn index_last_indexed(index_id: &str, root_path: &std::path::Path) -> Option<String> {
    newest_mtime(&index_storage_dirs(index_id, root_path))
}

/// Newest `index.redb`/`chunks.json` mtime across an index's storage dirs.
///
/// Why (#4706): `first_existing_mtime_rfc3339` fixes precedence WITHIN one
/// directory (issue #80 — redb over the legacy JSON snapshot). Across the two
/// layouts the rule is different: a colocated index's writes land in
/// `.trusty-search/` while a stale global copy may still exist, so the newest
/// write wins rather than the first directory checked.
/// What: applies the per-directory selector to each directory, then takes the
/// chronological max by parsing back to a timestamp rather than comparing the
/// strings — both are produced by the same UTC formatter so lexicographic order
/// happens to agree, but that is a property of the formatter, not a contract.
/// Test: `last_indexed_takes_the_newer_of_the_two_layouts`.
fn newest_mtime(dirs: &[std::path::PathBuf]) -> Option<String> {
    dirs.iter()
        .filter_map(|d| first_existing_mtime_rfc3339(d, &["index.redb", "chunks.json"]))
        .max_by_key(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.timestamp_nanos_opt().unwrap_or(i64::MIN))
                .unwrap_or(i64::MIN)
        })
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

/// Report one index's stages, capabilities, and on-disk footprint.
///
/// Why: a bare hot-registry miss used to be a flat 404, which says the index
/// never existed. A cold-parked or restore-failed index DOES exist — it is just
/// not resident — and `search_handler` has always distinguished the two. #4715
/// makes a 404 here mean what it means there, because the MCP layer now reads
/// this endpoint's 404 as "never indexed" and would otherwise report a built
/// index as one that was never built. #5061 then gave the 503 a body: it used
/// to be a bare `StatusCode`, so an MCP caller saw `returned 503 Service
/// Unavailable: ` with empty text and could neither tell cold-parked from
/// restore-failed nor learn that a plain `search` reloads the former.
/// What: delegates the miss to [`super::degraded::residency_miss_response`], the
/// single builder `chunks` and `grep` share, so all three report a given daemon
/// state identically.
/// Test: `status_404_only_when_absent_from_every_store`,
/// `cold_parked_index_status_is_503_not_404`, and
/// `cold_parked_status_503_body_names_search_as_the_restore_path`.
pub(super) async fn index_status_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    index_status_report(&state, &id)
        .await
        .map(Json)
        .map_err(|(status, body)| (status, Json(body)))
}

/// The body `GET /indexes/{id}/status` serves, without the transport
/// (#6285 slice 2).
///
/// Why: `search.index.status` answers the same question over the socket. One
/// body is what stops the two transports reporting a different chunk count,
/// stage snapshot, or residency verdict for the same index.
/// What: [`index_status_handler`]'s whole former body. A refusal keeps its HTTP
/// status beside its body, because that status is what
/// [`crate::service::rpc::error::rpc_error_from_http`] turns into the JSON-RPC
/// code — so one decision classifies the refusal on both transports.
/// Test: `index_status_over_the_socket_matches_the_http_body`,
/// `an_unknown_index_reports_not_found_on_every_index_scoped_read`.
pub(crate) async fn index_status_report(
    state: &Arc<SearchAppState>,
    id: &str,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let index_id = IndexId::new(id);
    let handle = match state.registry.get(&index_id) {
        Some(h) => h,
        // #5061: registered-but-not-resident, permanently failed, and genuinely
        // unknown are three different answers; the builder renders each.
        None => {
            let (status, body) =
                super::degraded::residency_miss_response(&state.cold_store, &index_id);
            return Err((status, body.0));
        }
    };
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
    // #4706: `disk_bytes` here is the same metric `list_indexes` reports as
    // `size_bytes`; both read 0 for a colocated index before this fix.
    let (disk_bytes, disk_last_indexed) = index_disk_and_mtime(&index_id.0, &handle.root_path);
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
    let mut stages_snapshot = handle.stages.read().await.clone();
    // #6524: the pause flag lives on the handle, not inside `stages` — one
    // owner, projected here at read time, so no writer has to keep two copies
    // in step. Only `semantic` is pausable, so the other two stay `false`.
    stages_snapshot.semantic.paused = handle.embedding_pause.is_paused();
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
    // #5336: `status: "indexing"` and `stages.lexical: in_progress` say a walk
    // is underway; neither says whether anything is still driving it. A reindex
    // task that panicked or was cancelled leaves both exactly as a live one
    // does, so this is the field that separates the two.
    let stuck_mid_walk = crate::service::warm_boot::index_is_stuck_mid_walk(
        stages_snapshot.lexical.status,
        walk_diag.last_walk_started_at.is_some(),
        crate::service::reindex::index_task_in_flight(&index_id),
    );
    // Issue #681: prefer durable corpus count; in-memory map returns 0 after
    // idle eviction (TRUSTY_CHUNKS_IDLE_EVICT_SECS default 60s). Falls back to
    // in-memory for BM25-only / test indexers that have no corpus wired.
    // #4333: when the corpus failed to open, the in-memory fallback below is
    // NOT a measurement of the on-disk corpus — the quarantine invariant
    // guarantees `corpus_arc()` is `None`, so this reported 122 for an index
    // holding 201,206 chunks on disk, reading as catastrophic data loss.
    // Report `null` (unknown) rather than a partial-looking number.
    let chunk_count: Option<usize> = if indexer.corpus_open_failed {
        None
    } else {
        Some(
            indexer
                .corpus_arc()
                .and_then(|c| c.chunk_count().ok())
                .unwrap_or_else(|| indexer.chunk_count()),
        )
    };
    // #4333: name the classified failure so a consumer can tell a transient
    // open timeout (retry) from a genuine format mismatch (rebuild).
    let corpus_open_failure = indexer.corpus_open_failure.map(|k| {
        serde_json::json!({
            "kind": k.label(),
            "transient": k.is_transient(),
            "reason": k.stage_reason(),
        })
    });
    // Issue #3408: surface the watcher's live/degraded state per-index. A
    // network-mounted root never gets a live watcher (inotify/FSEvents can't
    // observe another host's writes there); `network_mount_degraded` plus
    // `degraded_reason` name the actionable per-file endpoints so an operator
    // inspecting one index (rather than polling all of `/health`) sees why
    // saves aren't triggering incremental indexing.
    let watcher_active = state.watcher_manager.is_watching(&index_id).await;
    let watcher_degraded_reason = state
        .watcher_manager
        .network_degraded_reason(&index_id)
        .await;
    // #4787: `stages.semantic.embedded` counts embeddings computed during THIS
    // boot's pass, so a fully-working index whose HNSW snapshot was already
    // current at boot reports `0` — indistinguishable from a dead semantic
    // lane, which is the #2178 degradation signature. An estate audit flagged
    // three healthy indexes on it. `vectors_present` is the cumulative ground
    // truth: `Index::size()` on the store that actually answers queries (the
    // same accessor #4707 made the reindex gate check), so it is never a
    // bookkeeping figure that can drift from reality.
    //
    // #4787 review: `null` alone conflated two unrelated states — no store
    // wired versus a store whose count could not be read. Reporting a failed
    // read as "not applicable" is this issue's own defect one level down, so
    // `vectors_unavailable_reason` names which of the two produced the null.
    // It is absent whenever `vectors_present` is a number.
    let vectors_present = indexer.vector_count().await;
    let vectors_unavailable_reason = match (vectors_present, indexer.has_vector_store()) {
        (Some(_), _) => serde_json::Value::Null,
        // A store is attached but `len()` errored — a real fault, not an
        // absence, and distinct from `skip_vector`.
        (None, true) => serde_json::Value::String("count_unreadable".into()),
        // BM25-only, `skip_vector`, or a test indexer: there is nothing to
        // count and that is the correct, healthy answer.
        (None, false) => serde_json::Value::String("no_vector_store".into()),
    };
    let semantic_coverage = serde_json::json!({
        "vectors_present": vectors_present,
        "vectors_unavailable_reason": vectors_unavailable_reason,
        "chunk_count": chunk_count,
        // The same number `stages.semantic.embedded` carries, restated here
        // under a name that says what it measures. The field over in `stages`
        // keeps its name for wire compatibility.
        "embedded_this_boot": stages_snapshot.semantic.embedded,
    });
    Ok(serde_json::json!({
        "index_id": index_id.0,
        "root_path": handle.root_path,
        "chunk_count": chunk_count,
        "corpus_open_failure": corpus_open_failure,
        "status": legacy_status,
        "stages": stages_snapshot,
        // #4787: cumulative semantic coverage, beside the per-boot delta.
        "semantic_coverage": semantic_coverage,
        "search_capabilities": search_capabilities,
        "lexical_only": handle.lexical_only,
        "skip_kg": handle.skip_kg,
        "skip_vector": handle.skip_vector,
        // Issue #2984 Phase 1: per-component visibility — on/off plus the
        // live stage status (InProgress means a catch-up is running).
        "components": {
            "kg": { "enabled": !handle.skip_kg, "status": stages_snapshot.graph.status },
            "vector": { "enabled": !handle.skip_vector, "status": stages_snapshot.semantic.status },
        },
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
        // #5336: true when the walk started and was then abandoned. Surfaced,
        // not recovered — clear it with `POST /indexes/:id/reindex`.
        "stuck_mid_walk": stuck_mid_walk,
        // Issue #3408: per-index watcher liveness + network-mount degradation.
        "watcher": {
            "active": watcher_active,
            "network_mount_degraded": watcher_degraded_reason.is_some(),
            "degraded_reason": watcher_degraded_reason,
        },
    }))
}

/// Optional query parameters for `GET /indexes/{id}/graph` (issue #128).
///
/// Why: a full KG export on a large repo can be tens of thousands of nodes;
/// D3/Cytoscape clients usually want a filtered subgraph. These let the caller
/// narrow the export server-side instead of shipping the whole graph.
/// What: all fields optional; absent params apply no filter.
/// Test: covered by `test_graph_handler_filters` in `tests/integration_tests.rs`.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct GraphQueryParams {
    /// Comma-separated node `type` values to keep (e.g. `Symbol,File`).
    pub(crate) types: Option<String>,
    /// Comma-separated `EdgeKind` display names to keep (e.g.
    /// `CallsFunction,Implements`).
    pub(crate) edge_types: Option<String>,
    /// Minimum edge weight; edges below this are dropped.
    pub(crate) min_weight: Option<f32>,
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
    // The HTTP 404 stays a bare status with no body, exactly as before: the
    // body the core builds exists so the socket's error frame can carry a
    // message, and adding one here would be a change to a route this slice is
    // not moving.
    let body = graph_report(&state, &id, &params)
        .await
        .map_err(|(status, _)| status)?;

    let mut response = Json(body).into_response();
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("max-age=3600"),
    );
    Ok(response)
}

/// The body `GET /indexes/{id}/graph` serves, without the transport
/// (#6285 slice 2).
///
/// Why: `search.graph.get` exports the same graph over the socket. Filtering
/// (`types` / `edge_types` / `min_weight`) and the edge-endpoint drop rule are
/// subtle enough that a second implementation would diverge silently.
/// What: [`graph_handler`]'s whole former body up to the response. The
/// `Cache-Control` header stays in the handler — it is an HTTP concern with no
/// counterpart on a socket.
/// Test: `graph_over_the_socket_matches_the_http_body`.
pub(crate) async fn graph_report(
    state: &Arc<SearchAppState>,
    id: &str,
    params: &GraphQueryParams,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let index_id = IndexId::new(id);
    let handle = state
        .registry
        .get(&index_id)
        .ok_or_else(|| unknown_index(&index_id))?;
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

    Ok(serde_json::json!({
        "nodes": nodes,
        "edges": edges,
        "stats": {
            "node_count": graph.node_count(),
            "edge_count": graph.edge_count(),
        },
        // Sampled from the host clock at answer time, so two reads microseconds
        // apart legitimately differ — the parity test excludes it (#6358).
        "generated_at": chrono::Utc::now().to_rfc3339(),
    }))
}

/// The refusal every graph read shares for an id the hot registry does not hold.
///
/// Why one builder: `graph`, `graph/stats` and `graph/neighbors` all answered a
/// bare 404 or their own hand-rolled body, and the socket needs a message for
/// the error frame. One spelling is what keeps the three saying the same thing.
fn unknown_index(index_id: &IndexId) -> (StatusCode, serde_json::Value) {
    (
        StatusCode::NOT_FOUND,
        serde_json::json!({
            "error": format!("unknown index: {}", index_id.0),
            "index_id": index_id.0,
        }),
    )
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
    // As in `graph_handler`: the HTTP 404 keeps its bare-status shape.
    graph_stats_report(&state, &id)
        .await
        .map(Json)
        .map_err(|(status, _)| status)
}

/// The body `GET /indexes/{id}/graph/stats` serves, without the transport
/// (#6285 slice 2).
///
/// Test: `graph_stats_over_the_socket_matches_the_http_body`.
pub(crate) async fn graph_stats_report(
    state: &Arc<SearchAppState>,
    id: &str,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let index_id = IndexId::new(id);
    let handle = state
        .registry
        .get(&index_id)
        .ok_or_else(|| unknown_index(&index_id))?;
    let graph = {
        let indexer = handle.indexer.read().await;
        indexer.snapshot_symbol_graph().await
    };
    let breakdown = graph.edge_kind_breakdown();
    let mut edge_kinds = serde_json::Map::with_capacity(breakdown.len());
    for (tag, count) in breakdown {
        edge_kinds.insert(tag, serde_json::Value::from(count));
    }

    // Issue #816: surface dropped-edge count so operators can detect
    // daemon/corpus version skew without log scraping.
    Ok(serde_json::json!({
        "node_count": graph.node_count(),
        "edge_count": graph.edge_count(),
        "edge_kinds": serde_json::Value::Object(edge_kinds),
        "unknown_edge_tags_dropped": graph.unknown_edge_tags_dropped(),
    }))
}
