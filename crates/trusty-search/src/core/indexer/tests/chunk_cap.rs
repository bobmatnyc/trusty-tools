//! Regression tests for the per-index `TRUSTY_MAX_CHUNKS` cap (#100).
//!
//! Why: the cap dropped chunks with a `tracing::warn!` and still returned
//! `Ok`, so `CodeIndexer::index_file` reported success for a write that never
//! landed and `POST /indexes/:id/index-file` answered `"indexed": true`. An
//! index that had reached its cap therefore answered that way for every
//! subsequent write, forever, while accepting nothing — and a search over the
//! discarded content read exactly like a correct empty result. Each test below
//! fails against the pre-fix commit.
//! What: drives a real `CodeIndexer` to its cap, then asserts the refusal, its
//! persistence across later writes, the update-at-cap exemption, and the
//! `CommitTimings` accounting the refusal reads.
//! Test: `cargo test -p trusty-search -- chunk_cap`

use super::super::CodeIndexer;
use crate::core::indexer::ParsedBatch;

/// Restore `TRUSTY_MAX_CHUNKS` to its prior value when a test ends, however it
/// ends. Paired with `#[serial_test::serial]` because `std::env` is
/// process-global and `max_chunks_per_index()` reads it on every call.
struct MaxChunksEnvGuard(Option<String>);

impl MaxChunksEnvGuard {
    fn set(value: usize) -> Self {
        let prior = std::env::var("TRUSTY_MAX_CHUNKS").ok();
        // SAFETY: serialized against every other reader/writer of this var via
        // `#[serial_test::serial]` on each test that constructs the guard.
        unsafe { std::env::set_var("TRUSTY_MAX_CHUNKS", value.to_string()) };
        Self(prior)
    }

    fn set_to(&self, value: usize) {
        // SAFETY: same serialization as `set`.
        unsafe { std::env::set_var("TRUSTY_MAX_CHUNKS", value.to_string()) };
    }
}

impl Drop for MaxChunksEnvGuard {
    fn drop(&mut self) {
        // SAFETY: same serialization as `set`.
        unsafe {
            match self.0.take() {
                Some(v) => std::env::set_var("TRUSTY_MAX_CHUNKS", v),
                None => std::env::remove_var("TRUSTY_MAX_CHUNKS"),
            }
        }
    }
}

const FILE_A: &str = "pub fn alpha_one() {}\npub fn alpha_two() {}\n";
const FILE_B: &str = "pub fn bravo_one() {}\npub fn bravo_two() {}\n";

/// BM25-only indexer (no embedder, no durable corpus) — the cap lives in the
/// in-memory corpus map, so neither lane is needed to exercise it.
fn make_cap_indexer() -> CodeIndexer {
    CodeIndexer::new("cap-test", "/tmp/cap-test")
}

/// Fill an indexer to exactly its cap by indexing `FILE_A` with the cap wide
/// open, then pinning the cap at the resulting chunk count. Returns the guard
/// holding that pinned value.
async fn fill_to_cap(idx: &CodeIndexer) -> MaxChunksEnvGuard {
    let guard = MaxChunksEnvGuard::set(1_000_000);
    idx.index_file("src/a.rs", FILE_A)
        .await
        .expect("baseline write must succeed with the cap wide open");
    let at_cap = idx.in_memory_chunk_count().await;
    assert!(at_cap > 0, "fixture must produce at least one chunk");
    guard.set_to(at_cap);
    guard
}

/// The defect itself: at cap, a write of a brand-new file must NOT report
/// success. Pre-fix this returned `Ok(())` and the HTTP handler answered
/// `"indexed": true` while the corpus was unchanged.
#[tokio::test]
#[serial_test::serial]
async fn at_cap_index_file_is_an_error_not_a_silent_success() {
    let idx = make_cap_indexer();
    let _guard = fill_to_cap(&idx).await;
    let at_cap = idx.in_memory_chunk_count().await;

    let err = idx
        .index_file("src/b.rs", FILE_B)
        .await
        .expect_err("a write the chunk cap discarded must not be reported as success");

    let msg = err.to_string();
    assert!(
        msg.contains("chunk cap") && msg.contains("src/b.rs"),
        "the error must name the cap and the file the caller sent, got: {msg}"
    );
    assert_eq!(
        idx.in_memory_chunk_count().await,
        at_cap,
        "nothing from the refused file may have landed"
    );
    assert!(
        !idx.raw_chunks_snapshot()
            .await
            .expect("corpus reads")
            .iter()
            .any(|c| c.id.starts_with("src/b.rs")),
        "the refused file's chunks must be absent from the corpus"
    );
}

/// The consequence that matters most: an index at its cap is permanently
/// frozen. Pre-fix EVERY later write answered success, so a caller driving the
/// index (a CI hook on a network mount, the file watcher) had no signal at all
/// that it had stopped accepting content.
#[tokio::test]
#[serial_test::serial]
async fn at_cap_index_stays_an_error_for_every_later_write() {
    let idx = make_cap_indexer();
    let _guard = fill_to_cap(&idx).await;
    let at_cap = idx.in_memory_chunk_count().await;

    for i in 0..3 {
        let path = format!("src/later_{i}.rs");
        let content = format!("pub fn later_{i}() {{}}\n");
        idx.index_file(&path, &content).await.unwrap_err();
        assert_eq!(
            idx.in_memory_chunk_count().await,
            at_cap,
            "write {i} must leave the frozen index exactly as it was"
        );
    }
}

/// The refusal must be conditional on chunks actually being dropped, not on
/// the index merely sitting at its cap: re-indexing a file already in the
/// corpus only touches existing chunk ids, which the cap never rejects. Over-
/// refusing here would break the file watcher on every save once an index
/// filled up.
#[tokio::test]
#[serial_test::serial]
async fn at_cap_update_to_an_already_indexed_file_still_succeeds() {
    let idx = make_cap_indexer();
    let _guard = fill_to_cap(&idx).await;
    let at_cap = idx.in_memory_chunk_count().await;

    idx.index_file("src/a.rs", FILE_A)
        .await
        .expect("re-indexing an already-present file updates existing ids, which the cap allows");
    assert_eq!(idx.in_memory_chunk_count().await, at_cap);
}

/// `commit_parsed_batch` is the single place that knows a chunk was dropped,
/// and its `CommitTimings` is the only channel carrying that out. `chunks`
/// must count what landed, not what was attempted.
#[tokio::test]
#[serial_test::serial]
async fn commit_parsed_batch_reports_cap_drops_in_timings() {
    let idx = make_cap_indexer();
    let _guard = fill_to_cap(&idx).await;

    let (chunks, _entities) = crate::core::chunker::chunk_ast("src/b.rs", FILE_B);
    let n = chunks.len();
    assert!(n > 0, "fixture must produce at least one chunk");
    let parsed = ParsedBatch {
        embeddings: vec![None; n],
        chunks,
        entities_by_file: Vec::new(),
        parse_ms: 0,
        embed_ms: 0,
        vector_count: 0,
    };

    let timings = idx
        .commit_parsed_batch(parsed, true)
        .await
        .expect("commit itself does not fail; it reports the drop");
    assert_eq!(
        timings.chunks_dropped_by_cap, n,
        "every chunk of the batch was dropped by the cap and must be counted"
    );
    assert_eq!(
        timings.chunks, 0,
        "`chunks` must count what reached the corpus, not what was attempted"
    );
}
