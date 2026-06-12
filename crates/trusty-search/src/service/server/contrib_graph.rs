//! Contributed-graph ingest + traversal endpoints (ADR-0009, issue #819).
//!
//! Why: external extractors contribute cross-tier relationship graphs
//! (proc/table data flow, host-language → database bridges) that the
//! chunk-derived pipeline cannot see. They need a durable ingest surface and
//! a direction-aware traversal primitive to be useful.
//! What: two handlers —
//! - `POST /indexes/{id}/graph`: replace-per-producer ingest of a contributed
//!   graph document into the `kg_contrib` redb table, followed by a serving-
//!   graph rebuild so the contribution is immediately queryable.
//! - `GET /indexes/{id}/graph/neighbors`: BFS over the merged graph with
//!   direction control, edge-kind filtering, and a bounded hop count.
//!
//! Test: `tests_contrib_graph.rs` (sibling module).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use petgraph::Direction;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::core::corpus::contrib::{ContribEdge, ContribGraph, ContribNode};
use crate::core::entity::EdgeKind;
use crate::core::registry::IndexId;
use crate::core::symbol_graph::parse_kind_token;

use super::state::SearchAppState;

/// Wire shape of `POST /indexes/{id}/graph` — matches the reference
/// extractor's `--emit kggraph` document (`navigatsql/kggraph@1`), with the
/// producer identity and staleness metadata from the #819 contract.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IngestGraphRequest {
    /// Optional wire-schema id (e.g. `navigatsql/kggraph@1`). Logged, not
    /// enforced — the field set below is the actual contract.
    #[serde(default)]
    pub schema: Option<String>,
    pub producer: String,
    #[serde(default)]
    pub producer_version: Option<String>,
    #[serde(default)]
    pub git_sha: Option<String>,
    #[serde(default)]
    pub nodes: Vec<ContribNode>,
    #[serde(default)]
    pub edges: Vec<ContribEdge>,
}

#[derive(Debug, Serialize)]
pub(super) struct IngestGraphResponse {
    pub producer: String,
    /// Whether this ingest replaced a prior contribution from the producer.
    pub replaced: bool,
    pub nodes_received: usize,
    pub edges_received: usize,
    /// Post-merge serving-graph totals (derived + all contributions).
    pub graph_nodes: usize,
    pub graph_edges: usize,
    /// Edges dropped across the merge for unresolvable kinds (#816 counter).
    pub unknown_edge_tags_dropped: usize,
}

/// `POST /indexes/{id}/graph` — ingest one producer's contributed graph.
///
/// Why: ADR-0009 Option A — batch ingest into trusty-search's persisted KG as
/// a durable contributed overlay. Replace-per-producer semantics (the #819
/// refinement): the extractor emits its complete graph per run, so each
/// ingest atomically replaces that producer's prior contribution — deletions
/// in the scanned codebase never leave stale edges.
/// What: validates the request, stores the [`ContribGraph`] blob (one redb
/// row per producer), then rebuilds the serving graph (derived-from-chunks +
/// every stored contribution) so the data is immediately traversable. Returns
/// post-merge graph totals.
/// Test: `ingest_then_neighbors_round_trip`, `ingest_unknown_index_404`,
/// `ingest_empty_producer_400` in `tests_contrib_graph`.
pub(super) async fn ingest_graph_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Json(req): Json<IngestGraphRequest>,
) -> Result<Json<IngestGraphResponse>, (StatusCode, Json<serde_json::Value>)> {
    let err = |code: StatusCode, msg: String| (code, Json(serde_json::json!({ "error": msg })));

    if req.producer.trim().is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "producer must be a non-empty identifier".into(),
        ));
    }
    let index_id = IndexId::new(id);
    let handle = state
        .registry
        .get(&index_id)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("unknown index '{index_id}'")))?;

    let contrib = ContribGraph {
        producer: req.producer.clone(),
        producer_version: req.producer_version,
        git_sha: req.git_sha,
        nodes: req.nodes,
        edges: req.edges,
    };
    let (nodes_received, edges_received) = (contrib.nodes.len(), contrib.edges.len());
    tracing::info!(
        index_id = %index_id,
        producer = %contrib.producer,
        schema = req.schema.as_deref().unwrap_or("-"),
        nodes = nodes_received,
        edges = edges_received,
        "contrib ingest received"
    );

    // Persist (replace-per-producer) on a blocking worker — redb is sync.
    let replaced = {
        let indexer = handle.indexer.read().await;
        let Some(corpus) = indexer.corpus_store() else {
            return Err(err(
                StatusCode::SERVICE_UNAVAILABLE,
                "index has no durable corpus store — contributed graphs require one".into(),
            ));
        };
        // `save_contrib_graph` reports the replacement from the insert
        // itself, so the flag is exact under concurrent same-producer
        // ingest (PR #1129 review, finding 4).
        let join = tokio::task::spawn_blocking(move || corpus.save_contrib_graph(&contrib)).await;
        match join {
            Ok(Ok(replaced)) => replaced,
            Ok(Err(e)) => {
                return Err(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("contrib persist failed: {e}"),
                ))
            }
            Err(e) => {
                return Err(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("contrib persist task panicked: {e}"),
                ))
            }
        }
    };

    // Fold into the serving graph: rebuild derived-from-chunks (rehydrates an
    // idle-evicted chunk map internally) + merge every stored contribution.
    let graph = {
        let indexer = handle.indexer.read().await;
        indexer.rebuild_symbol_graph_now().await;
        indexer.snapshot_symbol_graph().await
    };

    Ok(Json(IngestGraphResponse {
        producer: req.producer,
        replaced,
        nodes_received,
        edges_received,
        graph_nodes: graph.node_count(),
        graph_edges: graph.edge_count(),
        unknown_edge_tags_dropped: graph.unknown_edge_tags_dropped(),
    }))
}

#[derive(Debug, Deserialize)]
pub(super) struct NeighborsParams {
    /// Start node id (symbol or contributed canonical id).
    pub node: String,
    /// `in` | `out` | `both` (default `both`).
    #[serde(default)]
    pub direction: Option<String>,
    /// Comma-separated edge-kind filter. Accepts the contributed coarse
    /// vocabulary (`reads`, `writes`, …), PascalCase static tags (`Reads`),
    /// and `custom:<label>` tags. Omitted = all kinds.
    #[serde(default)]
    pub edge_kinds: Option<String>,
    /// BFS depth (default 2, clamped to `1..=4` like `get_call_chain`).
    #[serde(default)]
    pub max_hops: Option<usize>,
}

#[derive(Debug, Serialize)]
pub(super) struct NeighborEntry {
    pub symbol: String,
    /// Node kind for contributed nodes (`table`, `proc`, …); `null` for
    /// derived code symbols.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_kind: Option<String>,
    /// Defining chunk id for derived symbols; empty for contributed nodes.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub chunk_id: String,
    /// Tag of the edge that discovered this neighbour.
    pub edge: String,
}

/// `GET /indexes/{id}/graph/neighbors` — bounded BFS over the merged graph.
///
/// Why: the single traversal primitive of ADR-0009 — answers "what writes
/// table X" (`direction=in&edge_kinds=writes`), "what does this method
/// transitively touch" (`direction=out&max_hops=3`), and "callers of a
/// deprecated proc" (`direction=in`) without exporting the whole graph.
/// What: resolves the index, snapshots the graph (lock-free reads after the
/// Arc clone), and runs the direction-aware, kind-filtered BFS.
/// Test: `ingest_then_neighbors_round_trip`, `neighbors_unknown_node_is_empty`.
pub(super) async fn graph_neighbors_handler(
    State(state): State<Arc<SearchAppState>>,
    Path(id): Path<String>,
    Query(params): Query<NeighborsParams>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let err = |code: StatusCode, msg: String| (code, Json(serde_json::json!({ "error": msg })));

    let index_id = IndexId::new(id);
    let handle = state
        .registry
        .get(&index_id)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("unknown index '{index_id}'")))?;

    let dirs: &[Direction] = match params.direction.as_deref().unwrap_or("both") {
        "in" | "inbound" => &[Direction::Incoming],
        "out" | "outbound" => &[Direction::Outgoing],
        "both" => &[Direction::Outgoing, Direction::Incoming],
        other => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!("direction must be in|out|both, got '{other}'"),
            ))
        }
    };
    let kinds: Option<Vec<EdgeKind>> = match params.edge_kinds.as_deref() {
        None | Some("") => None,
        Some(csv) => {
            let mut out = Vec::new();
            for token in csv.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                let Some(kind) = parse_kind_token(token) else {
                    return Err(err(
                        StatusCode::BAD_REQUEST,
                        format!("unknown edge kind '{token}'"),
                    ));
                };
                out.push(kind);
            }
            Some(out)
        }
    };
    let max_hops = params.max_hops.unwrap_or(2).clamp(1, 4);

    let graph = {
        let indexer = handle.indexer.read().await;
        indexer.snapshot_symbol_graph().await
    };
    let neighbors: Vec<NeighborEntry> = graph
        .graph_neighbors(&params.node, dirs, kinds.as_deref(), max_hops)
        .into_iter()
        .map(|(symbol, chunk_id, node_kind, edge)| NeighborEntry {
            symbol,
            node_kind,
            chunk_id,
            edge,
        })
        .collect();

    Ok(Json(serde_json::json!({
        "index_id": index_id.to_string(),
        "node": params.node,
        "node_kind": graph.node_kind(&params.node),
        "direction": params.direction.as_deref().unwrap_or("both"),
        "max_hops": max_hops,
        "count": neighbors.len(),
        "neighbors": neighbors,
    })))
}
