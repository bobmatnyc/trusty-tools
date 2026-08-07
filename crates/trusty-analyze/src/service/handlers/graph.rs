//! Route handlers for knowledge graph, entity, cluster, NER, and SCIP endpoints.
//!
//! Why: Extracted from `handlers/analysis.rs` to keep the graph/embedding
//! domain — KG queries, entity listings, k-means clustering, NER extraction,
//! and SCIP protobuf ingest — in its own file. These handlers are structurally
//! distinct from the simpler complexity/quality handlers.
//!
//! What: Six public handlers (`graph_for_index`, `entities_for_index`,
//! `clusters_for_index`, `ner_for_index`, `ingest_scip`,
//! `scip_overlay_status`) plus their supporting types and helpers.
//!
//! Test: All handler tests are in `service/tests.rs`.

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderName, HeaderValue},
    response::Json,
};
use serde::{Deserialize, Serialize};

use crate::core::{
    bow_embedding, cluster as run_cluster, extract_doc_comments, extract_kg_from_scip,
    ClusterResult, NerExtractor, ScipIngestSummary,
};
use crate::embedder::{Embedder, EmbedderKind};
use crate::service::events::{fetch_chunks, AnalyzerAppState, AnalyzerEvent, ApiError};
use crate::types::{KgGraph, KgNode, RawEntity};

#[derive(Deserialize)]
pub struct GraphQueryParams {
    /// Restrict to a single language (`"rust"`, `"typescript"`, ...).
    pub language: Option<String>,
}

/// Response header naming whether a SCIP overlay contributed to this graph.
///
/// Why (#5049): an empty or thin `/graph` body cannot say whether the index
/// has no SCIP data at all or has an ingested SCIP index that carried no
/// symbols. Answering in a header keeps the JSON body a bare `KgGraph`, so
/// every existing consumer is unaffected.
/// What: `present` when an overlay row exists for this index (even a
/// zero-node one), `absent` when none has ever been ingested.
/// Test: `graph_marks_scip_overlay_present_after_ingest`,
/// `graph_marks_scip_overlay_absent_without_ingest`.
pub const SCIP_OVERLAY_HEADER: &str = "x-scip-overlay";

/// Why: Phase 2 surfaces the language-neutral knowledge graph to consumers
/// (Claude Code, web UIs, etc.) so they can navigate symbols across files.
/// What: Fetch chunks for `index`, run the language registry, merge any stored
/// SCIP overlay, optionally filter to `?language=`, and return the merged
/// `KgGraph` as JSON plus an `x-scip-overlay: present|absent` header.
/// Test: `graph_marks_scip_overlay_present_after_ingest` and
/// `graph_marks_scip_overlay_absent_without_ingest` in `service/tests.rs`.
pub async fn graph_for_index(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<GraphQueryParams>,
) -> Result<([(HeaderName, HeaderValue); 1], Json<KgGraph>), ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let res = state.registry.analyze(&chunks);
    let mut graph = res.graph;
    // Merge any SCIP-derived overlay that the user has uploaded for this
    // index. SCIP supplies fully-resolved cross-file symbols which the
    // tree-sitter adapters cannot derive on their own, so the union is
    // strictly more useful than either alone.
    //
    // #5049: a read failure is surfaced as 500 rather than treated as
    // "no overlay" — silently degrading to the tree-sitter-only graph is the
    // exact indistinguishable-emptiness this endpoint is being fixed for.
    let overlay = state.scip_overlays.get(&id).map_err(|e| {
        tracing::error!("read SCIP overlay for {id} failed: {e:#}");
        ApiError::internal(format!("read SCIP overlay for {id}: {e:#}"))
    })?;
    let overlay_header = HeaderValue::from_static(if overlay.is_some() {
        "present"
    } else {
        "absent"
    });
    if let Some(record) = overlay {
        graph.merge(record.graph);
        graph = crate::core::link(graph);
    }
    if let Some(lang) = params.language.as_deref() {
        let keep_nodes: std::collections::HashSet<String> = graph
            .nodes
            .iter()
            .filter(|n| n.language == lang)
            .map(|n| n.id.clone())
            .collect();
        graph.nodes.retain(|n| keep_nodes.contains(&n.id));
        graph
            .edges
            .retain(|e| keep_nodes.contains(&e.from) && keep_nodes.contains(&e.to));
    }
    Ok((
        [(HeaderName::from_static(SCIP_OVERLAY_HEADER), overlay_header)],
        Json(graph),
    ))
}

#[derive(Deserialize)]
pub struct EntitiesQueryParams {
    pub kind: Option<String>,
    pub language: Option<String>,
}

/// Why: Many consumers only want a flat node listing, sorted, for browsing
/// (autocomplete, file outlines).
/// What: Same pipeline as `/graph`, but returns just `Vec<KgNode>` sorted by
/// `(kind, name)`. Optional `?kind=` and `?language=` filters.
/// Test: filtering by `kind=Function` returns only Function nodes.
pub async fn entities_for_index(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<EntitiesQueryParams>,
) -> Result<Json<Vec<KgNode>>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let res = state.registry.analyze(&chunks);
    let mut nodes = res.graph.nodes;
    if let Some(lang) = params.language.as_deref() {
        nodes.retain(|n| n.language == lang);
    }
    if let Some(kind) = params.kind.as_deref() {
        nodes.retain(|n| format!("{:?}", n.kind) == kind);
    }
    nodes.sort_by(|a, b| {
        format!("{:?}", a.kind)
            .cmp(&format!("{:?}", b.kind))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(Json(nodes))
}

#[derive(Deserialize)]
pub struct ClusterQueryParams {
    /// Number of clusters to compute. Defaults to 8, clamped to [1, 50].
    pub k: Option<usize>,
    /// Embedding method: `"bow"` (default, deterministic 256-dim) or
    /// `"neural"` (fastembed all-MiniLM-L6-v2, 384-dim).
    #[serde(default)]
    pub method: Option<EmbedderKind>,
}

#[derive(Serialize)]
pub struct ClusterResponseItem {
    pub id: usize,
    pub label: String,
    pub members: Vec<String>,
    pub cohesion: f32,
    pub size: usize,
}

#[derive(Serialize)]
pub struct ClusterResponse {
    pub k: usize,
    /// Which embedder produced the vectors (`"bow"` or `"neural"`).
    pub method: String,
    /// Dimension of the embedding vectors used.
    pub dim: usize,
    pub iterations: usize,
    pub chunk_count: usize,
    pub clusters: Vec<ClusterResponseItem>,
}

fn cluster_items_from(r: ClusterResult) -> Vec<ClusterResponseItem> {
    r.clusters
        .into_iter()
        .map(|c| ClusterResponseItem {
            id: c.id,
            label: c.label,
            size: c.members.len(),
            members: c.members,
            cohesion: c.cohesion,
        })
        .collect()
}

/// Why: surfaces "what themes does this codebase contain?" without needing a
/// full knowledge graph or neural embedder. Useful for codebase exploration
/// and high-level summaries.
/// What: fetches chunks for `index`, derives a 256-dim bag-of-words vector
/// per chunk, runs seeded k-means, and returns the cluster assignments.
/// Test: covered indirectly by trusty-analyzer-core's `concept_cluster` tests;
/// the route wiring is exercised by `clusters_route_returns_502_when_search_down`.
pub async fn clusters_for_index(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<ClusterQueryParams>,
) -> Result<Json<ClusterResponse>, ApiError> {
    const BOW_DIM: usize = 256;
    let k = params.k.unwrap_or(8).clamp(1, 50);
    let method = params.method.clone().unwrap_or_default();
    let chunks = fetch_chunks(&state, &id).await?;
    if chunks.is_empty() {
        return Ok(Json(ClusterResponse {
            k,
            method: method.as_str().to_string(),
            dim: 0,
            iterations: 0,
            chunk_count: 0,
            clusters: Vec::new(),
        }));
    }

    // Resolve embedder. For neural, defer to the shared state embedder (which
    // may itself be BOW if fastembed failed to load at startup). For BOW,
    // the bow_embedding free function is called directly — no BowEmbedder
    // allocation needed.
    let neural_embedder: Arc<dyn Embedder> = state.embedder.clone();
    let effective_kind_initial: EmbedderKind = match method {
        EmbedderKind::Neural => neural_embedder.kind(),
        EmbedderKind::Bow => EmbedderKind::Bow,
    };

    // Why: `NeuralEmbedder::embed_batch` holds a `std::sync::Mutex` over ONNX
    // inference, which can block for tens-to-hundreds of milliseconds. Running
    // it directly on a tokio executor thread starves other async tasks queued
    // on that thread. `spawn_blocking` moves the call onto a dedicated blocking
    // thread pool so the executor stays responsive.
    // What: converts the chunk contents to owned `String`s (required to cross
    // the `'static` closure boundary), clones the `Arc<dyn Embedder>`, then
    // awaits the blocking join handle. Join-error is mapped to a warn + BOW
    // fallback so the endpoint never 500s on a temporary model hiccup.
    // Test: the existing cluster endpoint tests (e.g. `cluster_endpoint_bow`)
    // exercise this path; the spawn_blocking wrapping does not change observable
    // outputs, only prevents executor starvation.

    // Owned strings are needed both for the Neural spawn_blocking closure
    // (which requires 'static) and for the BOW fallback path.
    let owned_texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();

    let embed_result: anyhow::Result<(Vec<Vec<f32>>, EmbedderKind, usize)> = match method {
        EmbedderKind::Neural => {
            let embedder_arc = Arc::clone(&neural_embedder);
            let dim = embedder_arc.dim();
            let texts_for_task = owned_texts.clone();
            tokio::task::spawn_blocking(move || {
                let refs: Vec<&str> = texts_for_task.iter().map(String::as_str).collect();
                embedder_arc.embed_batch(&refs)
            })
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!("embed_batch task panicked: {e}")))
            .map(|v| (v, EmbedderKind::Neural, dim))
        }
        EmbedderKind::Bow => {
            let vecs: Vec<Vec<f32>> = owned_texts
                .iter()
                .map(|t| bow_embedding(t, BOW_DIM))
                .collect();
            Ok((vecs, EmbedderKind::Bow, BOW_DIM))
        }
    };
    let (vecs, effective_kind, dim) = match embed_result {
        Ok(triple) => triple,
        Err(e) => {
            tracing::warn!(
                "embedder ({:?}) failed ({e:#}); falling back to BOW",
                effective_kind_initial
            );
            let fallback: Vec<Vec<f32>> = owned_texts
                .iter()
                .map(|t| bow_embedding(t, BOW_DIM))
                .collect();
            (fallback, EmbedderKind::Bow, BOW_DIM)
        }
    };
    let embeddings: Vec<(String, Vec<f32>)> = chunks
        .iter()
        .zip(vecs)
        .map(|(c, v)| (c.id.clone(), v))
        .collect();
    let result = run_cluster(&embeddings, k, 100, 42);
    let iterations = result.iterations;
    Ok(Json(ClusterResponse {
        k,
        method: effective_kind.as_str().to_string(),
        dim,
        iterations,
        chunk_count: chunks.len(),
        clusters: cluster_items_from(result),
    }))
}

#[derive(Deserialize)]
pub struct NerQueryParams {
    /// Cap on the number of entities returned (after extraction).
    pub top_k: Option<usize>,
}

/// Why: surfaces named-entity candidates pulled from doc comments so callers
/// (Claude Code, UI dashboards) can browse natural-language concepts side by
/// side with structural symbols. The route is always available; the actual
/// ONNX NER model is feature-gated and opportunistically loaded at startup.
/// What: fetches chunks for `id`, runs `extract_doc_comments` on each chunk's
/// content, runs the NER extractor (no-op when the `ner` feature is disabled
/// or the model file is missing), and returns the entities truncated to
/// `top_k` (default 50).
/// Test: with a stub search client returning no chunks the handler returns an
/// empty array and HTTP 200; the NER feature flag is exercised by the core
/// crate's `ner` module tests.
pub async fn ner_for_index(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<NerQueryParams>,
) -> Result<Json<Vec<RawEntity>>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let top_k = params.top_k.unwrap_or(50);
    let extractor = NerExtractor::try_load();

    let mut entities: Vec<RawEntity> = Vec::new();
    for chunk in &chunks {
        let docs = extract_doc_comments(&chunk.content);
        if docs.is_empty() {
            continue;
        }
        entities.extend(extractor.extract(&docs, &chunk.file));
        if entities.len() >= top_k {
            break;
        }
    }
    entities.truncate(top_k);
    Ok(Json(entities))
}

#[derive(Serialize)]
pub struct ScipIngestResponse {
    pub index_id: String,
    #[serde(flatten)]
    pub summary: ScipIngestSummary,
}

/// Why: SCIP indexes carry fully-resolved cross-file symbols that the
/// tree-sitter adapters can't derive (call resolution, trait implementations
/// across files, generics). Ingesting them is how the analyzer goes from
/// "approximate" to "precise" for languages with a real SCIP indexer.
/// What: accepts a SCIP `Index` protobuf as raw bytes, converts it to a
/// `KgGraph`, writes it to the durable per-index overlay store, and returns
/// ingest stats. The overlay is merged into `/indexes/{id}/graph` responses.
/// A store write failure is a 500 — #5049: this endpoint must not answer 200
/// for an ingest that was not durably recorded.
/// Test: `scip_ingest_accepts_valid_index_and_stores_overlay` POSTs a
/// hand-built SCIP index; `scip_overlay_survives_state_rebuild` proves the
/// write outlives the process state that accepted it.
pub async fn ingest_scip(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<ScipIngestResponse>, ApiError> {
    let (graph, summary) = extract_kg_from_scip(&body).map_err(|e| {
        tracing::warn!("SCIP ingest for {id} failed: {e:#}");
        ApiError::bad_request(format!("invalid SCIP protobuf: {e:#}"))
    })?;
    let symbols_ingested = summary.kg_nodes;
    state.scip_overlays.put(&id, graph).map_err(|e| {
        tracing::error!("persist SCIP overlay for {id} failed: {e:#}");
        ApiError::internal(format!("persist SCIP overlay for {id}: {e:#}"))
    })?;
    state.emit(AnalyzerEvent::ScipIngested {
        index_id: id.clone(),
        symbols_ingested,
    });
    Ok(Json(ScipIngestResponse {
        index_id: id,
        summary,
    }))
}

/// Overlay presence report for one index.
#[derive(Serialize)]
pub struct ScipOverlayStatus {
    pub index_id: String,
    pub nodes: usize,
    pub edges: usize,
    /// Unix seconds at which the overlay was ingested.
    pub ingested_at: u64,
}

/// Why (#5049): the defect this endpoint closes is that an empty `/graph`
/// response cannot say whether the index has no SCIP data at all or has an
/// ingested SCIP index that carried no symbols. A 404 means nobody ingested;
/// a 200 with `nodes: 0` means somebody ingested an empty index. Persisting
/// the overlay stops the data loss, but only this distinction stops the
/// silence.
/// What: reads the durable overlay store for `id` and returns its node/edge
/// counts and ingest timestamp, or 404 when no overlay row exists.
/// Test: `scip_overlay_status_404_when_never_ingested` and
/// `scip_overlay_survives_state_rebuild` in `service/tests.rs`.
pub async fn scip_overlay_status(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
) -> Result<Json<ScipOverlayStatus>, ApiError> {
    let record = state.scip_overlays.get(&id).map_err(|e| {
        tracing::error!("read SCIP overlay for {id} failed: {e:#}");
        ApiError::internal(format!("read SCIP overlay for {id}: {e:#}"))
    })?;
    let record = record.ok_or_else(|| {
        ApiError::not_found(format!("no SCIP overlay has been ingested for index {id}"))
    })?;
    Ok(Json(ScipOverlayStatus {
        index_id: record.index_id,
        nodes: record.graph.node_count(),
        edges: record.graph.edge_count(),
        ingested_at: record.ingested_at,
    }))
}
