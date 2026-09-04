//! #6699 tests for vector-lane health on `GET /indexes?details=true`.
//!
//! Why: a zero-vector index reports `stages.semantic: ready`, so it is invisible
//! to `/health`'s `indexes_stage_failed_ids`, and before this change the list
//! row carried neither `stages` nor `semantic_coverage` — the console's Indexes
//! view rendered `tm-trusty-tools-19` and `-21` (58k chunks each, a 112-byte
//! `hnsw.usearch`) as healthy. Every assertion below fails against `origin/main`
//! because the keys do not exist there.
//! What: three cases — a zero-vector index reports `vectors_present: 0` beside a
//! nonzero `chunk_count`; an index whose embed pass is behind the corpus reports
//! the lag; and the list row and `GET /indexes/{id}/status` agree field-for-field
//! on the shared block, which is what pins them to one implementation.
//! Test: this module IS the tests
//! (`cargo test -p trusty-search --no-fail-fast -- vector_health`).

use super::*;
use axum::extract::{Query, State};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry, StageState, StageStatus};
use crate::core::store::{UsearchStore, VectorStore};

/// Minimal `RawChunk` for the in-memory corpus fallback (no durable corpus).
fn raw(id: &str) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: "src/lib.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: "fn x() {}".to_string(),
        function_name: None,
        language: Some("rust".to_string()),
        chunk_type: ChunkType::Code,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    }
}

/// One registered index holding `chunks` chunks and `vectors` vectors, with the
/// lexical and semantic stages both `Ready`.
///
/// Why: this is the #6689 signature exactly — all three stages report healthy
/// and `search_capabilities` advertises `vector`, so nothing except the live
/// store size distinguishes an index that answers vector queries from one that
/// returns nothing for every single one of them.
/// What: chunks go in before the store is wired, so `add_chunk` never sees it;
/// the store is a real `UsearchStore`, not a stub, so `vectors_present` is
/// `Index::size()` on the type that serves production queries.
async fn state_with(id: &str, chunks: usize, vectors: usize) -> Arc<SearchAppState> {
    let mut indexer = CodeIndexer::new(id, format!("/tmp/{id}"));
    for i in 0..chunks {
        indexer.add_chunk(raw(&format!("c{i}"))).await.unwrap();
    }
    let store = UsearchStore::new(4).expect("store init");
    for i in 0..vectors {
        store
            .upsert(&format!("c{i}"), vec![i as f32, 0.0, 0.0, 1.0])
            .await
            .expect("upsert");
    }
    indexer.set_store(Arc::new(store));

    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(indexer)),
        format!("/tmp/{id}").into(),
    ));
    let state = Arc::new(SearchAppState::new(registry));
    {
        let handle = state.registry.get(&IndexId::new(id)).expect("registered");
        let mut stages = handle.stages.write().await;
        stages.lexical = StageState {
            status: StageStatus::Ready,
            chunks: Some(chunks),
            ..Default::default()
        };
        stages.semantic = StageState {
            status: StageStatus::Ready,
            // The misleading per-boot zero of #4787: nothing needed embedding
            // this boot, which says nothing about cumulative coverage.
            embedded: Some(0),
            total: Some(chunks),
            ..Default::default()
        };
    }
    state
}

/// The single entry `GET /indexes?details=true` returns for a one-index state.
async fn detail_entry(state: Arc<SearchAppState>) -> serde_json::Value {
    use super::indexes::ListIndexesParams;
    let resp = list_indexes_handler(
        State(state),
        Query(ListIndexesParams {
            format: None,
            details: true,
            repo_identity: None,
        }),
    )
    .await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["indexes"][0].clone()
}

/// The list flags a zero-vector index, and keeps every pre-existing field (#6699).
///
/// Why: this is closure condition 1 of the issue. The row must carry the three
/// inputs the expanded panel's `empty_vector_store` verdict reads — a nonzero
/// `chunk_count`, `vector` in `search_capabilities`, and `vectors_present: 0` —
/// or the roster cannot tell this index from a healthy one. The tail of the test
/// pins backward compatibility: the #312 / #661 / #2611 fields keep their names
/// and values beside the additions.
/// Test: this function.
#[tokio::test]
async fn list_indexes_details_reports_zero_vectors() {
    let entry = detail_entry(state_with("empty-store", 17, 0).await).await;

    assert_eq!(
        entry["semantic_coverage"]["vectors_present"], 0,
        "an empty live store is 0 vectors, not an absent measurement: {entry}"
    );
    assert_eq!(
        entry["semantic_coverage"]["chunk_count"], 17,
        "the denominator the count is judged against must travel with it"
    );
    assert!(
        entry["semantic_coverage"]["vectors_unavailable_reason"].is_null(),
        "a readable store is not an unavailable one"
    );
    assert_eq!(
        entry["stages"]["semantic"]["status"], "ready",
        "the stage says ready — which is exactly why the count is needed"
    );
    assert!(
        entry["search_capabilities"]
            .as_array()
            .expect("capabilities array")
            .iter()
            .any(|c| c == "vector"),
        "the index advertises vector search while holding none: {entry}"
    );
    assert_eq!(entry["skip_vector"], false);
    assert_eq!(entry["lexical_only"], false);
    assert_eq!(entry["chunk_count"], 17);

    // Back-compat: the fields this row carried before #6699 are untouched.
    assert_eq!(entry["id"], "empty-store");
    assert_eq!(entry["root_path"], "/tmp/empty-store");
    assert!(
        entry.get("size_bytes").is_some(),
        "size_bytes must still be reported (null or a number): {entry}"
    );
}

/// The list reports an embed pass that is behind the corpus (#6699).
///
/// Why: partial coverage is the graded version of the same fault — 8 vectors for
/// 20 chunks means 60% of the corpus is unreachable by vector search, and
/// `stages.semantic.embedded` reads `0` here, so the per-boot counter would call
/// this index dead and a `ready` status would call it fine. Only the pair
/// `vectors_present` / `chunk_count` states the lag.
/// What: asserts the two numbers and that `embedded_this_boot` still carries the
/// per-boot value unchanged, so the wire meaning of neither field shifts.
/// Test: this function.
#[tokio::test]
async fn list_indexes_details_reports_embed_lag() {
    let entry = detail_entry(state_with("lagging", 20, 8).await).await;

    let coverage = &entry["semantic_coverage"];
    assert_eq!(coverage["vectors_present"], 8);
    assert_eq!(coverage["chunk_count"], 20);
    assert_eq!(
        coverage["embedded_this_boot"], 0,
        "the per-boot delta keeps its meaning — it is not the coverage figure"
    );
    assert_eq!(
        entry["stages"]["semantic"]["embedded"], 0,
        "the stage field keeps its name and value for wire compatibility"
    );
}

/// The list row and the per-index status body report identical lane health (#6699).
///
/// Why: two computations of the same verdict is how the roster and the expanded
/// panel come to disagree about one index — the defect this issue exists to
/// avoid, not merely a tidiness concern. Asserting equality of the shared block
/// is what pins both endpoints to `vector_health`; a reimplementation in either
/// arm fails here even when both arms are individually "correct".
/// What: drives both handlers over one fixture and compares the six shared keys.
/// Test: this function.
#[tokio::test]
async fn list_and_status_agree_on_vector_lane_health() {
    let state = state_with("agreement", 12, 5).await;
    let entry = detail_entry(Arc::clone(&state)).await;

    let axum::Json(status) = super::status::index_status_handler(
        State(Arc::clone(&state)),
        axum::extract::Path("agreement".to_string()),
    )
    .await
    .expect("a resident index reports status");

    for key in [
        "semantic_coverage",
        "stages",
        "search_capabilities",
        "chunk_count",
        "lexical_only",
        "skip_vector",
    ] {
        assert_eq!(
            entry[key], status[key],
            "the list and the per-index endpoint must not disagree on `{key}`"
        );
    }
    assert_eq!(
        status["semantic_coverage"]["vectors_present"], 5,
        "the fixture must actually exercise a populated store"
    );
}
