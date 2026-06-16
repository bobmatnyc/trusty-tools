//! Cursor-pagination tests for [`CodeIndexer::enumerate_chunks_after`]
//! (issue #1325).
//!
//! Why: deep offset pagination over `GET /indexes/{id}/chunks` timed out on
//! large indexes because every page re-sorted the whole corpus. The cursor
//! path does an indexed redb B-tree seek instead. These tests pin both the
//! durable (redb-backed) and in-memory fallback code paths.
//! What: page through a corpus by following `next_cursor`, asserting full
//! coverage, no duplicates, ascending `id` order, and termination.
//! Test: this module (lives in its own file to keep `tests.rs` under the
//! 1500-SLOC test cap).
use super::CodeIndexer;
use crate::core::chunker::{ChunkType, RawChunk};
use crate::core::corpus::CorpusStore;
use std::sync::Arc;

/// Minimal in-memory `RawChunk` builder.
fn raw(id: &str) -> RawChunk {
    RawChunk {
        id: id.to_string(),
        file: "f.rs".to_string(),
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

/// In-memory fallback path: no durable corpus. Paging forward by `next_cursor`
/// must cover every chunk exactly once, in ascending id order, and terminate.
#[tokio::test]
async fn test_enumerate_chunks_after_cursor_in_memory_fallback() {
    let idx = CodeIndexer::new("cursor-mem", "/tmp/cursor-mem");
    for id in ["a:1:1", "b:1:1", "c:1:1", "d:1:1", "e:1:1"] {
        idx.add_chunk(raw(id)).await.unwrap();
    }

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let (total, page, next) = idx.enumerate_chunks_after(cursor.as_deref(), 2).await;
        assert_eq!(total, 5, "total chunk count is stable across pages");
        for c in &page {
            seen.push(c.id.clone());
        }
        pages += 1;
        match next {
            Some(c) => cursor = Some(c),
            None => break,
        }
        assert!(pages < 10, "pagination must terminate");
    }
    assert_eq!(seen, vec!["a:1:1", "b:1:1", "c:1:1", "d:1:1", "e:1:1"]);

    // limit == 0 → empty page, no cursor.
    let (total_z, z, next_z) = idx.enumerate_chunks_after(None, 0).await;
    assert_eq!(total_z, 5);
    assert!(z.is_empty());
    assert!(next_z.is_none());
}

/// Durable path: cursor pagination over a redb corpus does an indexed seek.
/// Verifies full coverage, termination, ascending `chunk_id` key order, and no
/// duplicates.
#[tokio::test]
async fn test_enumerate_chunks_after_cursor_pages_via_redb() {
    let dir = tempfile::tempdir().unwrap();
    let redb_path = dir.path().join("index.redb");
    let mut idx = CodeIndexer::new("cursor-redb", "/tmp/cursor-redb");
    idx.set_corpus_store(Arc::new(
        CorpusStore::open(&redb_path).expect("open corpus"),
    ));

    // index_files_batch persists chunks into redb (unlike add_chunk, which is
    // in-memory only) so the cursor path reads them back via the B-tree.
    idx.index_files_batch(&[
        ("src/a.rs".into(), "fn a_one() {}\nfn a_two() {}".into()),
        ("src/b.rs".into(), "fn b_one() {}".into()),
        ("src/c.rs".into(), "fn c_one() {}".into()),
    ])
    .await
    .expect("index batch");

    let total_chunks = idx.chunk_count();
    assert!(
        total_chunks >= 3,
        "expected >= 3 chunks, got {total_chunks}"
    );

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let (total, page, next) = idx.enumerate_chunks_after(cursor.as_deref(), 2).await;
        assert_eq!(total, total_chunks, "total is the redb chunk_count");
        for c in &page {
            seen.push(c.id.clone());
        }
        pages += 1;
        match next {
            Some(c) => cursor = Some(c),
            None => break,
        }
        assert!(pages < 1000, "pagination must terminate");
    }
    assert_eq!(
        seen.len(),
        total_chunks,
        "every chunk returned exactly once"
    );
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(seen, sorted, "redb cursor pages in ascending id order");
    sorted.dedup();
    assert_eq!(sorted.len(), seen.len(), "no chunk returned twice");
}
