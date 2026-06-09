//! Route handlers for knowledge graph, entity, cluster, NER, and SCIP endpoints.
//!
//! Why: Extracted from `handlers/analysis.rs` to keep the graph/embedding
//! domain — KG queries, entity listings, k-means clustering, NER extraction,
//! and SCIP protobuf ingest — in its own file. These handlers are structurally
//! distinct from the simpler complexity/quality handlers.
//!
//! What: Six public handlers (`graph_for_index`, `entities_for_index`,
//! `clusters_for_index`, `ner_for_index`, `ingest_scip`) plus their supporting
//! types and helpers.
//!
//! Test: All handler tests are in `service/tests.rs`.

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    response::Json,
};
use serde::{Deserialize, Serialize};

use crate::core::{
    bow_embedding, cluster as run_cluster, extract_doc_comments, extract_kg_from_scip,
    ClusterResult, NerExtractor, ScipIngestSummary,
};
use crate::embedder::{BowEmbedder, Embedder, EmbedderKind};
use crate::service::events::{fetch_chunks, AnalyzerAppState, AnalyzerEvent, ApiError};
use crate::types::{KgGraph, KgNode, RawEntity};

#[derive(Deserialize)]
pub struct GraphQueryParams {
    /// Restrict to a single language (`"rust"`, `"typescript"`, ...).
    pub language: Option<String>,
}

/// Why: Phase 2 surfaces the language-neutral knowledge graph to consumers
/// (Claude Code, web UIs, etc.) so they can navigate symbols across files.
/// What: Fetch chunks for `index`, run the language registry, optionally
/// filter to `?language=`, and return the merged `KgGraph` as JSON.
/// Test: with a mock index containing a Rust chunk, GET returns at least
/// one Function node tagged `language=rust`.
pub async fn graph_for_index(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<GraphQueryParams>,
) -> Result<Json<KgGraph>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let res = state.registry.analyze(&chunks);
    let mut graph = res.graph;
    // Merge any SCIP-derived overlay that the user has uploaded for this
    // index. SCIP supplies fully-resolved cross-file symbols which the
    // tree-sitter adapters cannot derive on their own, so the union is
    // strictly more useful than either alone.
    if let Some(overlay) = state.scip_overlays.read().await.get(&id).cloned() {
        graph.merge(overlay);
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
    Ok(Json(graph))
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
    // construct a fresh stateless BowEmbedder so we never go through fastembed
    // when the user explicitly asked for BOW.
    let neural_embedder: Arc<dyn Embedder> = state.embedder.clone();
    let bow_embedder = BowEmbedder::with_dim(BOW_DIM);
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
    // Suppress unused-variable warning if bow_embedder was not directly used
    let _ = &bow_embedder;

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
/// `KgGraph`, stores it as a per-index overlay, and returns ingest stats.
/// The overlay is merged into `/indexes/{id}/graph` responses.
/// Test: `scip_ingest_round_trip` POSTs a hand-built SCIP index and verifies
/// the resulting graph appears in the `/graph` response.
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
    state.scip_overlays.write().await.insert(id.clone(), graph);
    state.emit(AnalyzerEvent::ScipIngested {
        index_id: id.clone(),
        symbols_ingested,
    });
    Ok(Json(ScipIngestResponse {
        index_id: id,
        summary,
    }))
}
