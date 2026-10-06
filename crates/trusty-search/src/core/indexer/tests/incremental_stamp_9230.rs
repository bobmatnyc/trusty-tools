//! #9230: an incremental commit stamps `_meta["reindexed_unix"]`; a refused,
//! failed, no-op or read-only operation does not.
//!
//! Why: `search.project.resolve` ranks two checkouts of one repo by that
//! stamp, and only a full reindex wrote it, so a clone kept current by the
//! watcher, `index-file` or the boot-reconcile delta lost to a stale clone.
//! What: each committed write path (`index_file`, its tombstone arm, and the
//! watcher rescan's `index_files_batch*`) against a real redb corpus, plus the
//! negative arms: a no-op write, a cap refusal, a redb write that fails and is
//! logged, a failed stamp write, and a rehydrate.
//! Test: `cargo test -p trusty-search -- incremental_stamp_9230`

use super::*;
use crate::core::indexer::ingest::commit_stamp::TEST_FAIL_STAMP;

const FILE_A: &str = "pub fn alpha_9230() {}\npub fn alpha_two_9230() {}\n";
const FILE_B: &str = "pub fn bravo_9230() {}\npub fn bravo_two_9230() {}\n";
const TOMBSTONE: &str = "---\nsource_id: stamp-9230\nsource_status: deleted\n---\n";
/// An old stamp planted before a write, so "advanced" is visible as a change.
const OLD_STAMP: u64 = 100;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after epoch")
        .as_secs()
}

/// A BM25-only indexer over a fresh redb corpus in its own tempdir.
fn indexer(tag: &str) -> (tempfile::TempDir, CodeIndexer) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut idx = CodeIndexer::new(tag, "/tmp/stamp-9230");
    let store = CorpusStore::open(&dir.path().join("index.redb")).expect("open corpus");
    idx.set_corpus_store(Arc::new(store));
    (dir, idx)
}

fn stamp(idx: &CodeIndexer) -> Option<u64> {
    let corpus = idx.corpus.clone().expect("durable corpus");
    corpus.read_reindexed_unix_sync().expect("read stamp")
}

fn plant(idx: &CodeIndexer, unix: u64) {
    let corpus = idx.corpus.clone().expect("durable corpus");
    corpus.write_reindexed_unix_sync(unix).expect("plant stamp");
}

/// Replace the chunks table with one of the same name and another value type,
/// so every later chunk upsert fails inside redb with a real error.
pub(crate) fn break_chunk_writes(idx: &CodeIndexer) {
    const CHUNKS: redb::TableDefinition<'static, &str, &[u8]> =
        redb::TableDefinition::new("chunks");
    const DECOY: redb::TableDefinition<'static, &str, u64> = redb::TableDefinition::new("chunks");
    let corpus = idx.corpus.clone().expect("durable corpus");
    let txn = corpus.db().begin_write().expect("begin write txn");
    txn.delete_table(CHUNKS).expect("drop chunks table");
    txn.open_table(DECOY).expect("create decoy table");
    txn.commit().expect("commit decoy");
}

/// Why: #9230 — `index-file`, the watcher's per-file write and the boot
/// reconcile delta all commit through `index_file`; each must stamp.
/// Fails on origin/main: the stamp stays `None`.
/// Test: this test.
#[tokio::test]
async fn index_file_stamps_the_corpus_it_commits() {
    let (_dir, idx) = indexer("stamp-9230-file");
    assert_eq!(stamp(&idx), None, "a fresh corpus is unstamped");
    let started = now_unix();
    idx.index_file("src/a.rs", FILE_A).await.expect("write");
    let got = stamp(&idx).expect("an incremental commit stamps the corpus");
    assert!((started..=now_unix()).contains(&got), "{got}");
}

/// Why: #9230 — the watcher rescan commits through `index_files_batch_no_rebuild`.
/// Fails on origin/main: the planted stamp does not move.
/// Test: this test.
#[tokio::test]
async fn index_files_batch_stamps_the_corpus_it_commits() {
    let (_dir, idx) = indexer("stamp-9230-batch");
    plant(&idx, OLD_STAMP);
    let added = idx
        .index_files_batch_no_rebuild(&[("src/a.rs".into(), FILE_A.into())])
        .await
        .expect("batch");
    assert!(added > 0, "the fixture must land chunks");
    assert!(
        stamp(&idx).expect("stamp") > OLD_STAMP,
        "the batch must stamp"
    );
}

/// Why: #9230 — a tombstone that removes rows is a committed write; a blank
/// file or a tombstone for a path that holds nothing changes nothing.
/// Fails on origin/main at the tombstone assertion.
/// Test: this test.
#[tokio::test]
async fn a_tombstone_that_removes_rows_stamps_and_a_noop_write_does_not() {
    let (_dir, idx) = indexer("stamp-9230-noop");
    // A blank `.json` lands no chunk (`IndexFileOutcome::Empty`).
    let outcome = idx.index_file_outcome("config/empty.json", "").await;
    assert_eq!(outcome.expect("blank"), IndexFileOutcome::Empty);
    idx.index_file("src/never.rs", TOMBSTONE)
        .await
        .expect("tombstone of nothing");
    assert_eq!(stamp(&idx), None, "a no-op write must not stamp");

    idx.index_file("src/a.rs", FILE_A).await.expect("write");
    plant(&idx, OLD_STAMP);
    idx.index_file("src/a.rs", TOMBSTONE)
        .await
        .expect("tombstone");
    assert!(idx.chunk_ids_for_file("src/a.rs").await.is_empty());
    assert!(
        stamp(&idx).expect("stamp") > OLD_STAMP,
        "the removal must stamp"
    );
}

/// Why: #9230 — a write the chunk cap refused is reported as an error, so it
/// must not refresh the index's recency. The cap leaves room for one of B's
/// chunks: B lands in part, rows reach redb, and only the cap refusal keeps
/// the stamp still. Fails with the stamp moved above the `dropped_by_cap` bail.
/// Test: this test.
#[tokio::test]
async fn a_write_the_chunk_cap_refuses_does_not_stamp() {
    let probe = CodeIndexer::new("stamp-9230-cap-count", "/tmp/stamp-9230");
    probe.index_file("src/a.rs", FILE_A).await.expect("probe");
    let a_chunks = probe.chunk_count();
    probe.index_file("src/b.rs", FILE_B).await.expect("probe");
    assert!(
        probe.chunk_count() - a_chunks >= 2,
        "B must hold at least two chunks for a partial landing"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let mut idx =
        CodeIndexer::new("stamp-9230-cap", "/tmp/stamp-9230").with_chunk_cap(a_chunks + 1);
    idx.set_corpus_store(Arc::new(
        CorpusStore::open(&dir.path().join("index.redb")).expect("open"),
    ));
    idx.index_file("src/a.rs", FILE_A).await.expect("fits");
    plant(&idx, OLD_STAMP);
    let refused = idx.index_file("src/b.rs", FILE_B).await;
    assert!(refused.is_err(), "the cap must refuse the second file");
    assert_eq!(
        idx.chunk_ids_for_file("src/b.rs").await.len(),
        1,
        "one of B's chunks must land, or the cap and the stamp never compete"
    );
    assert_eq!(stamp(&idx), Some(OLD_STAMP), "a refused write stamped");
}

/// Why: #9230 Fail-Open Check — `commit_corpus_to_redb` logs a failed redb
/// write at warn and the write still answers `Ok`; the stamp must not move,
/// or the resolver would rank a corpus missing the write as the freshest.
/// Test: this test.
#[tokio::test]
async fn a_redb_write_logged_as_a_warning_does_not_stamp() {
    let (_dir, idx) = indexer("stamp-9230-persist");
    plant(&idx, OLD_STAMP);
    break_chunk_writes(&idx);
    idx.index_file("src/a.rs", FILE_A)
        .await
        .expect("the in-memory commit still answers Ok");
    idx.index_files_batch_no_rebuild(&[("src/b.rs".into(), FILE_B.into())])
        .await
        .expect("the batch still answers Ok");
    assert_eq!(stamp(&idx), Some(OLD_STAMP), "a failed persist stamped");
}

/// Why: #9230 — the write landed, so a stamp failure is logged rather than
/// failing it, and the stamp keeps its previous value: the resolver reads the
/// index as older, never as fresher than it is.
/// Test: this test.
#[tokio::test]
async fn a_failed_stamp_write_leaves_the_previous_stamp() {
    let tag = "stamp-9230-stamp-fault";
    let (_dir, idx) = indexer(tag);
    plant(&idx, OLD_STAMP);
    TEST_FAIL_STAMP.lock().expect("seam").push(tag.to_string());
    let direct = idx.stamp_incremental_commit().await;
    let write = idx.index_file("src/a.rs", FILE_A).await;
    TEST_FAIL_STAMP.lock().expect("seam").retain(|t| t != tag);
    assert!(direct.is_err(), "the stamp failure must be returned");
    write.expect("the write itself landed and still answers Ok");
    assert!(!idx.chunk_ids_for_file("src/a.rs").await.is_empty());
    assert_eq!(stamp(&idx), Some(OLD_STAMP));
}

/// Why: #9230 closure — only a commit stamps; reclaiming the warm caches and
/// rehydrating them from redb must not.
/// Test: this test.
// #9230: reclaims and rehydrates; serial with the tests that set
// `TRUSTY_REHYDRATE_WAIT_MS` and `TEST_REHYDRATE_DELAY_MS`.
#[tokio::test]
#[serial_test::serial]
async fn a_reclaim_and_rehydrate_does_not_stamp() {
    let (_dir, idx) = indexer("stamp-9230-rehydrate");
    idx.index_file("src/a.rs", FILE_A).await.expect("write");
    plant(&idx, OLD_STAMP);
    assert!(
        idx.reclaim_memory_now().await > 0,
        "the caches were resident"
    );
    assert!(!idx.chunk_ids_for_file("src/a.rs").await.is_empty());
    assert_eq!(stamp(&idx), Some(OLD_STAMP));
}
