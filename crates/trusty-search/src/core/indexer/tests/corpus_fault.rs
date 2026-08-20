//! Durable-corpus read failures must surface, not be answered as empty (#5917).
//!
//! Why: the daemon served an empty in-memory corpus for an index holding
//! 85,269 chunks and answered `HTTP 200` with `results: []` and
//! `bm25_lane_degraded: true` — a flag that means "still warming up", which is
//! the one thing this state is not. The workaround was to delete and recreate
//! the index.
//! What: breaks a real redb corpus after it has been opened and populated (the
//! chunks table is dropped, so every later read of it fails the way redb's
//! "Previous I/O error occurred" state does), then asserts the search path
//! reports the fault instead of publishing the empty lanes it produced.
//! Test: the functions below.

use super::*;
use crate::core::indexer::corpus_fault::CorpusReadFault;
use crate::core::indexer::CorpusReadUnavailable;

/// Same definition redb keyed the corpus rows under. Declared here rather than
/// reaching into `core::corpus::tables` (whose handle is private to that
/// module) — redb identifies a table by name, so this deletes the real one.
const CHUNKS_TABLE: redb::TableDefinition<'static, &str, &[u8]> =
    redb::TableDefinition::new("chunks");

fn search_for(text: &str) -> SearchQuery {
    SearchQuery {
        text: text.to_string(),
        top_k: 10,
        expand_graph: false,
        compact: false,
        branch_files: None,
        branch_boost: SearchQuery::default_branch_boost(),
        branch: None,
        mode: SearchMode::All,
        exclude_archived: false,
        stage: None,
        refine_query: None,
        path_prefix: None,
        repos: Vec::new(),
    }
}

/// Drop the chunks table so every later read of it fails inside redb, the way
/// a corpus that has entered its "Previous I/O error occurred" state does.
fn break_corpus_reads(idx: &CodeIndexer) {
    let corpus = idx
        .corpus
        .clone()
        .expect("test indexer has a durable corpus");
    let txn = corpus.db().begin_write().expect("begin write txn");
    assert!(
        txn.delete_table(CHUNKS_TABLE).expect("delete chunks table"),
        "the chunks table existed before the test broke it"
    );
    txn.commit().expect("commit table drop");
}

/// #5917: a search against an index whose corpus cannot be read must name the
/// fault, not report an empty result set.
///
/// Why: the in-memory chunk map and BM25 corpus are a CACHE of the durable
/// corpus. Idle eviction empties them, the rehydrate that would refill them
/// reads the same broken corpus and reports its failure only to the log, and
/// every lane then answers from nothing. The response carried no field a
/// single-shot caller could branch on — `truncated`/`results` positively
/// asserted completeness.
/// What: indexes a real corpus, proves the query works warm, breaks the redb
/// reads, evicts the caches, and re-runs the identical query. Before the fix
/// this returned `Ok(vec![])`.
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn search_over_an_unreadable_corpus_is_an_error_not_an_empty_result_set() {
    let dir = tempfile::tempdir().unwrap();
    let idx = make_indexer_with_corpus(&dir.path().join("index.redb"));
    idx.index_files_batch(&[("src/a.rs".into(), "fn authenticate_user() {}".into())])
        .await
        .expect("index batch");

    let warm = idx
        .search(&search_for("authenticate_user"))
        .await
        .expect("warm search");
    assert!(!warm.is_empty(), "the query matches while the corpus reads");

    break_corpus_reads(&idx);
    // Keep every bounded rehydrate wait short: the scan fails immediately, so
    // this only bounds the wait if the notification is missed.
    // SAFETY: serialized against every other reader/writer of this var by
    // `#[serial_test::serial]` on this test.
    unsafe { std::env::set_var("TRUSTY_REHYDRATE_WAIT_MS", "25") };
    let evicted = idx.reclaim_memory_now().await;

    let result = idx.search(&search_for("authenticate_user")).await;

    // SAFETY: same serialization as the `set_var` above.
    unsafe { std::env::remove_var("TRUSTY_REHYDRATE_WAIT_MS") };
    assert!(evicted > 0, "eviction dropped the in-memory caches");

    let err = result.expect_err(
        "a search over an unreadable corpus must be an error — answering it with \
         an empty result set is #5917",
    );
    let unavailable = err
        .downcast_ref::<CorpusReadUnavailable>()
        .expect("the search path raises the typed corpus-read error");
    assert_eq!(
        unavailable.index_id, idx.index_id,
        "the error names the index it is about"
    );
    let msg = format!("{err:#}");
    assert!(
        msg.contains(&idx.index_id) && msg.contains("#5917") && !unavailable.detail.is_empty(),
        "the error must name the index and carry the underlying fault, got: {msg}"
    );
}

/// The recorded fault clears on the next successful read.
///
/// Why: the record is what makes `search_with_drops` refuse, so a fault that
/// outlived the condition causing it would wedge an index into refusing every
/// query forever — trading one fail-open for a fail-closed of the same size.
/// What: exercises the record/clear/error contract directly.
/// Test: this function IS the test.
#[test]
fn a_recorded_corpus_fault_clears_on_the_next_successful_read() {
    let fault = CorpusReadFault::default();
    assert!(fault.error("idx").is_none(), "a fresh record is clean");

    fault.record("redb point-read failed: Previous I/O error occurred");
    let err = fault
        .error("idx")
        .expect("a recorded fault produces an error");
    assert_eq!(err.index_id, "idx");
    assert!(err.detail.contains("Previous I/O error"));

    fault.clear();
    assert!(
        fault.error("idx").is_none(),
        "a successful read clears the fault so the index is not wedged"
    );
}
