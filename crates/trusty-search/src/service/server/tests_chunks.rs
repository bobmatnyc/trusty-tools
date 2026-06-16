//! Tests for `GET /indexes/:id/chunks` pagination (issues #54, #1325).
//!
//! Why: deep offset pagination 502'd on large indexes (#1325). The handler now
//! supports an additive `after` cursor (indexed seek, O(page)) alongside the
//! legacy `offset`/`limit` (O(offset) scan, retained for back-compat). These
//! tests pin both modes and the `next_cursor` contract.
//! What: register an in-memory index, plant chunks, drive
//! `get_index_chunks_handler` directly, and assert page coverage / ordering /
//! cursor semantics.
//! Test: this module.
use super::*;
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use axum::extract::{Path, Query, State};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Minimal `RawChunk` for the in-memory fallback path (no durable corpus).
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

/// Build a one-index state with the given chunk ids planted in memory.
async fn state_with_chunks(ids: &[&str]) -> (Arc<SearchAppState>, String) {
    let registry = IndexRegistry::new();
    let name = "chunks-test";
    let id = IndexId::new(name);
    let indexer = CodeIndexer::new(name, "/tmp/chunks-test");
    for cid in ids {
        indexer.add_chunk(raw(cid)).await.unwrap();
    }
    registry.register(IndexHandle::bare(
        id,
        Arc::new(RwLock::new(indexer)),
        "/tmp/chunks-test".into(),
    ));
    (Arc::new(SearchAppState::new(registry)), name.to_string())
}

async fn call_chunks(
    state: &Arc<SearchAppState>,
    name: &str,
    params: ChunksParams,
) -> serde_json::Value {
    let resp = get_index_chunks_handler(
        State(Arc::clone(state)),
        Path(name.to_string()),
        Query(params),
    )
    .await
    .expect("handler ok");
    resp.0
}

/// Cursor mode: paging forward by `next_cursor` covers every chunk exactly
/// once, in ascending id order, and terminates with `next_cursor = null`.
#[tokio::test]
async fn chunks_endpoint_cursor_pages_full_coverage() {
    let (state, name) = state_with_chunks(&["a:1:1", "b:1:1", "c:1:1", "d:1:1", "e:1:1"]).await;

    let mut seen: Vec<String> = Vec::new();
    // Start cursor paging with an empty `after` ("from the first chunk").
    let mut after: Option<String> = Some(String::new());
    let mut pages = 0;
    loop {
        let body = call_chunks(
            &state,
            &name,
            ChunksParams {
                offset: 0,
                limit: 2,
                after: after.clone(),
            },
        )
        .await;
        assert_eq!(body["total"], 5);
        for c in body["chunks"].as_array().unwrap() {
            seen.push(c["id"].as_str().unwrap().to_string());
        }
        pages += 1;
        match body["next_cursor"].as_str() {
            Some(c) => after = Some(c.to_string()),
            None => break,
        }
        assert!(pages < 10, "must terminate");
    }
    assert_eq!(seen, vec!["a:1:1", "b:1:1", "c:1:1", "d:1:1", "e:1:1"]);
}

/// Offset mode (back-compat): the legacy `offset`/`limit` slice still works
/// unchanged, total is preserved, and `next_cursor` is always null (offset
/// order differs from cursor order, so it must not seed a cursor walk).
#[tokio::test]
async fn chunks_endpoint_offset_back_compat() {
    let (state, name) = state_with_chunks(&["a:1:1", "b:1:1", "c:1:1"]).await;

    let p1 = call_chunks(
        &state,
        &name,
        ChunksParams {
            offset: 0,
            limit: 2,
            after: None,
        },
    )
    .await;
    assert_eq!(p1["total"], 3);
    assert_eq!(p1["offset"], 0);
    assert_eq!(p1["chunks"].as_array().unwrap().len(), 2);
    assert!(
        p1["next_cursor"].is_null(),
        "offset mode never surfaces a cursor (different ordering)"
    );

    // Second offset page covers the remainder.
    let p2 = call_chunks(
        &state,
        &name,
        ChunksParams {
            offset: 2,
            limit: 2,
            after: None,
        },
    )
    .await;
    assert_eq!(p2["chunks"].as_array().unwrap().len(), 1);
    assert!(p2["next_cursor"].is_null());
}

/// An unknown index id is a 404, in both modes.
#[tokio::test]
async fn chunks_endpoint_unknown_index_is_404() {
    let (state, _name) = state_with_chunks(&["a:1:1"]).await;
    let err = get_index_chunks_handler(
        State(state),
        Path("does-not-exist".to_string()),
        Query(ChunksParams {
            offset: 0,
            limit: 10,
            after: Some("a:1:1".to_string()),
        }),
    )
    .await
    .expect_err("unknown index must 404");
    assert_eq!(err, axum::http::StatusCode::NOT_FOUND);
}
