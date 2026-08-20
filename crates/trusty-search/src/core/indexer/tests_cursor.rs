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
        let (total, page, next) = idx
            .enumerate_chunks_after(cursor.as_deref(), 2)
            .await
            .unwrap();
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
    let (total_z, z, next_z) = idx.enumerate_chunks_after(None, 0).await.unwrap();
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
        let (total, page, next) = idx
            .enumerate_chunks_after(cursor.as_deref(), 2)
            .await
            .unwrap();
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

/// #6043: an evicted, not-yet-repopulated chunk map must not be enumerated as
/// though it were the corpus.
///
/// Why: `enumerate_chunks` reported `chunks.len()` as `total`, and
/// `ensure_chunks_loaded` returns on a bounded budget whether or not the
/// detached rehydrate has committed. An index holding 50,929 chunks therefore
/// answered `GET /indexes/trusty-tools/chunks` with `total: 0` and an empty
/// page at HTTP 200; trusty-analyze read that as a complete empty corpus and
/// published `complexity_distribution total: 0`. Issue #5917 recorded the same
/// mechanism as an intermittent flap — 0 chunks, then 85,269 chunks 67 seconds
/// later from the identical command.
/// What: indexes a real corpus, evicts the in-memory map, and holds the
/// rehydrate past the caller's wait budget with the test delay hook, so the
/// call lands in exactly the state that produced the zero. Asserts `Err`.
/// Before the fix this returned `Ok((0, vec![]))`.
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn enumerate_chunks_errors_when_rehydrate_did_not_commit() {
    use std::sync::atomic::Ordering;

    let dir = tempfile::tempdir().unwrap();
    let mut idx = CodeIndexer::new("evicted-mid-rehydrate", "/tmp/evicted-mid-rehydrate");
    idx.set_corpus_store(Arc::new(
        CorpusStore::open(&dir.path().join("index.redb")).expect("open corpus"),
    ));
    idx.index_files_batch(&[("src/a.rs".into(), "fn a_one() {}\nfn a_two() {}".into())])
        .await
        .expect("index batch");
    assert!(
        idx.enumerate_chunks(0, 100).await.expect("warm read").0 > 0,
        "the corpus is non-empty before eviction"
    );

    // Hold the rehydrate scan well past the wait budget so the call observes
    // the in-flight state rather than the committed one.
    // SAFETY: serialized against every other reader/writer of this var by
    // `#[serial_test::serial]` on this test.
    unsafe { std::env::set_var("TRUSTY_REHYDRATE_WAIT_MS", "25") };
    super::idle_evict::TEST_REHYDRATE_DELAY_MS.store(5_000, Ordering::Relaxed);

    let evicted = idx.clear_in_memory_chunks().await;
    assert!(evicted > 0, "eviction dropped the in-memory chunks");

    let result = idx.enumerate_chunks(0, 100).await;

    super::idle_evict::TEST_REHYDRATE_DELAY_MS.store(0, Ordering::Relaxed);
    // SAFETY: same serialization as the `set_var` above.
    unsafe { std::env::remove_var("TRUSTY_REHYDRATE_WAIT_MS") };

    let err = result.expect_err(
        "an evicted, unrepopulated map must be an error — reporting it as a \
         zero-chunk corpus is #6043",
    );
    let msg = format!("{err:#}");
    assert!(
        msg.contains("evicted") && msg.contains("#6043"),
        "the error must name the state and the ticket, got: {msg}"
    );
}
