//! RPC handlers for knowledge graph, entity, cluster, NER, and SCIP methods.
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
//! Test: All handler tests are in `service/rpc_tests.rs`.

use serde::{Deserialize, Serialize};

use crate::core::{
    cluster as run_cluster, extract_doc_comments, extract_kg_from_scip, ClusterResult,
    NerExtractor, ScipIngestSummary,
};
use crate::embedder::{Embedder, EmbedderKind};
use crate::service::events::{fetch_chunks, AnalyzerAppState, ApiError};
use crate::types::{KgGraph, KgNode, RawEntity};

#[derive(Debug, Deserialize)]
pub struct GraphRequest {
    pub index_id: String,
    /// Restrict to a single language (`"rust"`, `"typescript"`, ...).
    pub language: Option<String>,
}

/// A knowledge graph plus whether a SCIP overlay contributed to it.
///
/// Why (#5049): an empty or thin graph cannot say whether the index has no SCIP
/// data at all or has an ingested SCIP index that carried no symbols. Until
/// #6287 that answer rode an `x-scip-overlay` HTTP response header, which a
/// JSON-RPC frame has no room for — a frame carries one `result` value and no
/// metadata channel beside it. Moving the flag into the body is what keeps the
/// distinction, and it is now the only place it exists.
/// What: `scip_overlay` is true when an overlay row exists for this index, even
/// a zero-node one. It describes the overlay's EXISTENCE, not the body — a
/// `language` filter that removes every SCIP node still reports `true`.
/// Test: `rpc_graph_marks_scip_overlay_present_after_ingest`,
/// `rpc_graph_marks_scip_overlay_absent_without_ingest`.
#[derive(Debug, Serialize)]
pub struct GraphResponse {
    #[serde(flatten)]
    pub graph: KgGraph,
    pub scip_overlay: bool,
}

/// Read one index's overlay off the blocking pool.
///
/// Why: `ScipOverlayStore::get` opens a synchronous redb read transaction, and
/// redb serialises read-transaction *acquisition* against any in-flight write
/// commit — the same executor stall `handlers/facts.rs` moved off the runtime
/// after issue #67. A `/graph` read landing mid-`put` would block a tokio
/// worker while a whole-repository `KgGraph` is fsynced.
/// What: clones the store (cheap — `Arc<Database>`) into `spawn_blocking` and
/// maps both the join error and the store error to an internal error. A read
/// failure is never degraded to "no overlay": that would reintroduce the
/// indistinguishable emptiness of #5049.
/// Test: `scip_overlay_survives_state_rebuild`,
/// `rpc_graph_marks_scip_overlay_present_after_ingest`.
async fn read_overlay(
    state: &AnalyzerAppState,
    id: &str,
) -> Result<Option<crate::core::ScipOverlayRecord>, ApiError> {
    let store = state.scip_overlays.clone();
    let index_id = id.to_string();
    tokio::task::spawn_blocking(move || store.get(&index_id))
        .await
        .map_err(|e| ApiError::internal(format!("read SCIP overlay task panicked: {e}")))?
        .map_err(|e| {
            tracing::error!("read SCIP overlay for {id} failed: {e:#}");
            ApiError::internal(format!("read SCIP overlay for {id}: {e:#}"))
        })
}

/// Why: Phase 2 surfaces the language-neutral knowledge graph to consumers
/// (Claude Code, web UIs, etc.) so they can navigate symbols across files.
/// What: Fetch chunks for `index_id`, run the language registry, merge any
/// stored SCIP overlay, optionally filter to `language`, and return the merged
/// graph with the overlay-presence flag beside it ([`GraphResponse`]).
/// Test: `rpc_graph_marks_scip_overlay_present_after_ingest` and
/// `rpc_graph_marks_scip_overlay_absent_without_ingest` in
/// `service/rpc_tests.rs`.
pub async fn graph_for_index(
    state: &AnalyzerAppState,
    req: GraphRequest,
) -> Result<GraphResponse, ApiError> {
    let id = req.index_id;
    let chunks = fetch_chunks(state, &id).await?;
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
    let overlay = read_overlay(state, &id).await?;
    let scip_overlay = overlay.is_some();
    if let Some(record) = overlay {
        graph.merge(record.graph);
        graph = crate::core::link(graph);
    }
    if let Some(lang) = req.language.as_deref() {
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
    Ok(GraphResponse {
        graph,
        scip_overlay,
    })
}

#[derive(Debug, Deserialize)]
pub struct EntitiesRequest {
    pub index_id: String,
    pub kind: Option<String>,
    pub language: Option<String>,
}

/// Why: Many consumers only want a flat node listing, sorted, for browsing
/// (autocomplete, file outlines).
/// What: Same pipeline as `analyze.graph`, but returns just `Vec<KgNode>`
/// sorted by `(kind, name)`. Optional `kind` and `language` filters.
/// Test: filtering by `kind=Function` returns only Function nodes.
pub async fn entities_for_index(
    state: &AnalyzerAppState,
    req: EntitiesRequest,
) -> Result<Vec<KgNode>, ApiError> {
    let chunks = fetch_chunks(state, &req.index_id).await?;
    let res = state.registry.analyze(&chunks);
    let mut nodes = res.graph.nodes;
    if let Some(lang) = req.language.as_deref() {
        nodes.retain(|n| n.language == lang);
    }
    if let Some(kind) = req.kind.as_deref() {
        nodes.retain(|n| format!("{:?}", n.kind) == kind);
    }
    nodes.sort_by(|a, b| {
        format!("{:?}", a.kind)
            .cmp(&format!("{:?}", b.kind))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(nodes)
}

#[derive(Debug, Deserialize)]
pub struct ClustersRequest {
    pub index_id: String,
    /// Number of clusters to compute. Defaults to 8, clamped to [1, 50].
    pub k: Option<usize>,
    /// Embedding method. `"bow"` (deterministic 256-dim) is the only accepted
    /// value and the default.
    ///
    /// Why (#5067): `"neural"` used to be accepted here. It is now rejected
    /// with a 400 rather than quietly served by BOW, because the pre-fix
    /// daemon already did the quiet thing — when the model failed to load it
    /// logged a warning, installed BOW, and kept answering `method=neural`
    /// requests with hashed vectors. Removing the backend without removing the
    /// parameter value would make that silence permanent.
    ///
    /// Kept as a `String` rather than an `EmbedderKind` so the rejection is
    /// this service's own error message naming the bad value, not serde's
    /// "unknown variant" for a field the caller cannot see the shape of.
    #[serde(default)]
    pub method: Option<String>,
}

/// Resolve the `method` query parameter to an embedder.
///
/// Why (#5067): the only accepted value is `bow`, and anything else — in
/// practice the removed `neural` — has to be refused rather than quietly
/// treated as `bow`.
/// What: `None` yields the default; `"bow"` yields `Bow`; anything else is an
/// `invalid_params` error naming the offending value.
/// Test: `clusters_reject_removed_neural_method`,
/// `clusters_return_bow_vectors_for_a_live_corpus`.
fn resolve_method(method: Option<&str>) -> Result<EmbedderKind, ApiError> {
    match method {
        None => Ok(EmbedderKind::default()),
        Some(m) if m.eq_ignore_ascii_case(EmbedderKind::Bow.as_str()) => Ok(EmbedderKind::Bow),
        Some(other) => Err(ApiError::bad_request(format!(
            "unknown embedding method '{other}'; only '{}' is supported \
             (the neural backend was removed in #5067)",
            EmbedderKind::Bow.as_str()
        ))),
    }
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
    /// Which embedder produced the vectors. Always `"bow"` since #5067.
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
/// full knowledge graph. Useful for codebase exploration and high-level
/// summaries.
/// What: fetches chunks for `index`, embeds each one through the state's
/// embedder, runs seeded k-means, and returns the cluster assignments.
///
/// Why (#5067): this used to branch on `method`, with the neural arm deferring
/// to a model the daemon had loaded at boot and wrapping it in `spawn_blocking`
/// because ONNX inference could block for hundreds of milliseconds. Both are
/// gone. The remaining embedder is pure hashing — infallible, no I/O, nothing
/// to fall back from — so the error-swallowing fallback that used to sit here
/// is gone too: there is no failure left for it to hide.
///
/// Test: `clusters_return_bow_vectors_for_a_live_corpus`,
/// `clusters_reject_removed_neural_method`, and the wiring in
/// `rpc_clusters_report_an_unreachable_search_daemon`.
pub async fn clusters_for_index(
    state: &AnalyzerAppState,
    req: ClustersRequest,
) -> Result<ClusterResponse, ApiError> {
    let k = req.k.unwrap_or(8).clamp(1, 50);
    // #5067: refuse an unknown method before doing any work, so a caller asking
    // for the removed neural backend is told rather than silently given BOW.
    let method = resolve_method(req.method.as_deref())?;
    let chunks = fetch_chunks(state, &req.index_id).await?;
    if chunks.is_empty() {
        return Ok(ClusterResponse {
            k,
            method: method.as_str().to_string(),
            dim: 0,
            iterations: 0,
            chunk_count: 0,
            clusters: Vec::new(),
        });
    }

    let embedder: &dyn Embedder = state.embedder.as_ref();
    let dim = embedder.dim();
    let texts: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
    let vecs = embedder
        .embed_batch(&texts)
        .map_err(|e| ApiError::internal(format!("embed cluster corpus: {e:#}")))?;

    let embeddings: Vec<(String, Vec<f32>)> = chunks
        .iter()
        .zip(vecs)
        .map(|(c, v)| (c.id.clone(), v))
        .collect();
    let result = run_cluster(&embeddings, k, 100, 42);
    let iterations = result.iterations;
    Ok(ClusterResponse {
        k,
        method: embedder.kind().as_str().to_string(),
        dim,
        iterations,
        chunk_count: chunks.len(),
        clusters: cluster_items_from(result),
    })
}

#[derive(Debug, Deserialize)]
pub struct NerRequest {
    pub index_id: String,
    /// Cap on the number of entities returned (after extraction).
    pub top_k: Option<usize>,
}

/// Why: surfaces named-entity candidates pulled from doc comments so callers
/// (Claude Code, UI dashboards) can browse natural-language concepts side by
/// side with structural symbols. The method is always available; the actual
/// ONNX NER model is feature-gated and opportunistically loaded at startup.
/// What: fetches chunks for `index_id`, runs `extract_doc_comments` on each
/// chunk's content, runs the NER extractor (no-op when the `ner` feature is
/// disabled or the model file is missing), and returns the entities truncated
/// to `top_k` (default 50).
/// Test: with a stub search client returning no chunks the handler returns an
/// empty array; the NER feature flag is exercised by the core crate's `ner`
/// module tests.
pub async fn ner_for_index(
    state: &AnalyzerAppState,
    req: NerRequest,
) -> Result<Vec<RawEntity>, ApiError> {
    let chunks = fetch_chunks(state, &req.index_id).await?;
    let top_k = req.top_k.unwrap_or(50);
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
    Ok(entities)
}

/// Params for `analyze.scip_ingest`.
///
/// Why (#6287): the SCIP protobuf used to arrive as a raw `POST` body, and the
/// MCP tool that fed it base64-decoded client-side. A JSON-RPC frame carries no
/// binary channel, so the base64 the tool already had is what crosses the wire
/// and the decode moves here — one decoder instead of two, and the `invalid
/// base64` message now comes from the same place that rejects an invalid index.
/// Test: `rpc_scip_ingest_rejects_invalid_base64`,
/// `rpc_scip_ingest_accepts_a_valid_index_and_stores_the_overlay`.
#[derive(Debug, Deserialize)]
pub struct ScipIngestRequest {
    pub index_id: String,
    /// The SCIP `Index` protobuf, standard-alphabet base64 with padding.
    pub scip_base64: String,
}

#[derive(Debug, Serialize)]
pub struct ScipIngestResponse {
    pub index_id: String,
    #[serde(flatten)]
    pub summary: ScipIngestSummary,
}

/// Why: SCIP indexes carry fully-resolved cross-file symbols that the
/// tree-sitter adapters can't derive (call resolution, trait implementations
/// across files, generics). Ingesting them is how the analyzer goes from
/// "approximate" to "precise" for languages with a real SCIP indexer.
/// What: base64-decodes the SCIP `Index` protobuf, converts it to a `KgGraph`,
/// writes it to the durable per-index overlay store, and returns ingest stats.
/// The overlay is merged into `analyze.graph` responses. A store write failure
/// is an internal error — #5049: this method must not report success for an
/// ingest that was not durably recorded.
/// Test: `rpc_scip_ingest_accepts_a_valid_index_and_stores_the_overlay` sends a
/// hand-built SCIP index; `scip_overlay_survives_state_rebuild` proves the
/// write outlives the process state that accepted it.
pub async fn ingest_scip(
    state: &AnalyzerAppState,
    req: ScipIngestRequest,
) -> Result<ScipIngestResponse, ApiError> {
    use base64::Engine as _;
    let id = req.index_id;
    let body = base64::engine::general_purpose::STANDARD
        .decode(&req.scip_base64)
        .map_err(|e| ApiError::bad_request(format!("scip_base64 is not valid base64: {e}")))?;
    let (graph, summary) = extract_kg_from_scip(&body).map_err(|e| {
        tracing::warn!("SCIP ingest for {id} failed: {e:#}");
        ApiError::bad_request(format!("invalid SCIP protobuf: {e:#}"))
    })?;
    // Why: the write serialises a whole-repository `KgGraph` to JSON and
    // fsyncs it — the most expensive blocking call in this module. Same
    // `spawn_blocking` convention as `handlers/facts.rs` (issue #67).
    let store = state.scip_overlays.clone();
    let index_id = id.clone();
    tokio::task::spawn_blocking(move || store.put(&index_id, graph))
        .await
        .map_err(|e| ApiError::internal(format!("persist SCIP overlay task panicked: {e}")))?
        .map_err(|e| {
            tracing::error!("persist SCIP overlay for {id} failed: {e:#}");
            ApiError::internal(format!("persist SCIP overlay for {id}: {e:#}"))
        })?;
    Ok(ScipIngestResponse {
        index_id: id,
        summary,
    })
}

/// Overlay presence report for one index.
#[derive(Debug, Serialize)]
pub struct ScipOverlayStatus {
    pub index_id: String,
    pub nodes: usize,
    pub edges: usize,
    /// Unix seconds at which the overlay was ingested.
    pub ingested_at: u64,
}

/// Why (#5049): the defect this method closes is that an empty graph response
/// cannot say whether the index has no SCIP data at all or has an ingested SCIP
/// index that carried no symbols. An error frame means nobody ingested; a
/// result frame with `nodes: 0` means somebody ingested an empty index.
/// Persisting the overlay stops the data loss, but only this distinction stops
/// the silence. #6287 carried the distinction across the transport change: the
/// HTTP 404 became [`crate::service::events::CODE_NOT_FOUND`], which is why
/// that code exists rather than folding into `internal_error`.
/// What: reads the durable overlay store for `index_id` and returns its
/// node/edge counts and ingest timestamp, or `not_found` when no overlay row
/// exists.
/// Test: `rpc_scip_status_reports_not_found_when_never_ingested` and
/// `scip_overlay_survives_state_rebuild`.
pub async fn scip_overlay_status(
    state: &AnalyzerAppState,
    req: super::analysis::IndexRequest,
) -> Result<ScipOverlayStatus, ApiError> {
    let id = req.index_id;
    let record = read_overlay(state, &id).await?;
    let record = record.ok_or_else(|| {
        ApiError::not_found(format!("no SCIP overlay has been ingested for index {id}"))
    })?;
    Ok(ScipOverlayStatus {
        index_id: record.index_id,
        nodes: record.graph.node_count(),
        edges: record.graph.edge_count(),
        ingested_at: record.ingested_at,
    })
}
