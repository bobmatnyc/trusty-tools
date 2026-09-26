//! #8438 regression tests for the reindex write paths: the HNSW swap and the
//! corpus staging, commit and abort.
//!
//! Why: each of these chose `<root>/.trusty-search/` whenever that directory
//! existed, whatever the registry said.
//! What: an index registered `colocated=false` whose root already holds an
//! empty `.trusty-search/`; every path must write the data dir and leave the
//! repo directory empty.
//! Test: this file.

use super::corpus_swap::{
    abort_staged_corpus_swap, begin_staged_corpus_swap, commit_staged_corpus_swap,
};
use super::hnsw_swap::{begin_staged_hnsw_swap, commit_staged_hnsw_swap};
use crate::core::corpus::CorpusStore;
use crate::core::registry::IndexHandle;
use crate::service::storage_layout::storage_layout_8438_tests::Fixture;
use crate::service::storage_layout::{StorageLayout, HNSW_FILE, REDB_FILE, REDB_TMP_FILE};
use std::sync::Arc;

/// A `colocated=false` handle with a live corpus wired in the data dir.
async fn handle_with_corpus(fx: &Fixture, id: &str) -> IndexHandle {
    let handle = fx.handle(id, StorageLayout::DataDir).await;
    let live = crate::service::persistence::corpus_redb_path(id).expect("live path");
    let corpus = CorpusStore::open(&live).expect("open live corpus");
    handle
        .indexer
        .write()
        .await
        .set_corpus_store(Arc::new(corpus));
    handle
}

/// Why (#8438): the HNSW swap staged and published into the repo.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn hnsw_swap_writes_the_data_dir_not_the_repo() {
    let fx = Fixture::new(true);
    let handle = fx.handle("ts-8438-hswap", StorageLayout::DataDir).await;
    let paths = begin_staged_hnsw_swap(&handle, &handle.id)
        .await
        .expect("paths resolve");
    let data = fx.data_index_dir("ts-8438-hswap");
    assert_eq!(paths.live, data.join(HNSW_FILE));
    assert!(
        paths.staging.starts_with(&data),
        "{}",
        paths.staging.display()
    );
    commit_staged_hnsw_swap(&handle, &handle.id, &paths).await;
    assert!(
        paths.live.exists(),
        "the swap must publish into the data dir"
    );
    fx.assert_repo_dir_empty();
}

/// Why (#8438): corpus staging and commit wrote `index.redb.tmp` and promoted
/// it to `index.redb` inside the repo.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn corpus_staging_and_commit_write_the_data_dir_not_the_repo() {
    let fx = Fixture::new(true);
    let handle = handle_with_corpus(&fx, "ts-8438-cswap").await;
    let tmp = begin_staged_corpus_swap(&handle, &handle.id, true, None, None)
        .await
        .expect("staging")
        .expect("staging must engage");
    let data = fx.data_index_dir("ts-8438-cswap");
    assert_eq!(tmp, data.join(REDB_TMP_FILE));
    fx.assert_repo_dir_empty();
    assert!(commit_staged_corpus_swap(&handle, &handle.id, &tmp).await);
    assert!(data.join(REDB_FILE).exists());
    assert!(!tmp.exists(), "the staging file must be promoted");
    fx.assert_repo_dir_empty();
}

/// Why (#8438): the abort re-opened the live corpus from the repo.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn corpus_abort_reopens_the_data_dir_not_the_repo() {
    let fx = Fixture::new(true);
    let handle = handle_with_corpus(&fx, "ts-8438-cabort").await;
    let tmp = begin_staged_corpus_swap(&handle, &handle.id, true, None, None)
        .await
        .expect("staging")
        .expect("staging must engage");
    abort_staged_corpus_swap(&handle, &handle.id, &tmp).await;
    assert!(!tmp.exists(), "the abort must delete the staging file");
    assert!(
        handle.indexer.read().await.has_corpus_store(),
        "the abort must re-attach the data-dir live corpus"
    );
    fx.assert_repo_dir_empty();
}

/// Why (#8147): the hash-cache root-move decision probed the repo dir, so a
/// data-dir index whose root holds `.trusty-search/` kept its cache on a move.
/// What: both layouts over the same root, which carries the repo dir.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn hash_keys_follow_the_registry_layout_not_the_repo_dir() {
    let fx = Fixture::new(true);
    let data_dir = fx.handle("ts-8147-hkeys-d", StorageLayout::DataDir).await;
    assert!(
        !super::hash_cache::keys_survive_root_move(&data_dir).await,
        "#8147: a colocated=false index clears its hash cache on a root move"
    );
    let colocated = fx.handle("ts-8147-hkeys-c", StorageLayout::Colocated).await;
    assert!(
        super::hash_cache::keys_survive_root_move(&colocated).await,
        "#1073: colocated keys are root-relative and survive a move"
    );
}

/// Why (#8147 round 3, finding 3): the helper test above passes whatever
/// `run_reindex` reads; this one pins the call site, which used to probe
/// `has_colocated_storage(&canonical_root)`.
/// What: a real reindex of a data-dir index after a trusted root move, whose
/// new root holds `.trusty-search/`. A persisted hash row for a file absent
/// from the tree must be gone afterwards: the move cleared the table. Runs in
/// a child process, since the root-move gate reads the process-global
/// `TRUSTY_DATA_DIR`.
/// Test: this test. With the probe restored the row survives.
#[tokio::test]
async fn root_move_clears_the_hash_table_of_a_data_dir_index_over_a_repo_dir() {
    use crate::core::indexer::CodeIndexer;
    use crate::core::registry::IndexId;
    if !crate::service::test_isolation::isolate_in_child(
        "service::reindex::registry_layout_8438_tests::\
         root_move_clears_the_hash_table_of_a_data_dir_index_over_a_repo_dir",
    ) {
        return;
    }
    let corpus_dir = tempfile::tempdir().expect("corpus dir");
    let old_root = tempfile::tempdir().expect("old root");
    let new_root = tempfile::tempdir().expect("new root");
    std::fs::write(new_root.path().join("keep.rs"), "fn keep() {}\n").expect("write file");
    std::fs::create_dir_all(new_root.path().join(".trusty-search")).expect("repo dir");
    let id = IndexId::new("ts-8147-hmove");

    let corpus =
        Arc::new(CorpusStore::open(&corpus_dir.path().join(REDB_FILE)).expect("open corpus"));
    corpus
        .upsert_file_hashes(&[("ghost-8147.rs", "stale")])
        .expect("seed a stale hash row");
    let mut indexer = CodeIndexer::new(id.0.clone(), new_root.path().to_path_buf());
    indexer.set_corpus_store(corpus);
    assert_eq!(indexer.storage_layout(), StorageLayout::DataDir);
    let handle = Arc::new(IndexHandle::bare(
        id.clone(),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        new_root.path().to_path_buf(),
    ));
    handle
        .write_indexed_root(old_root.path())
        .await
        .expect("stamp the prior root");
    crate::service::persistence::upsert_index_registry_entry(
        crate::service::persistence::PersistedIndex {
            id: id.0.clone(),
            root_path: new_root.path().to_path_buf(),
            colocated: false,
            ..Default::default()
        },
    )
    .expect("persist the moved root");

    let progress = Arc::new(super::ReindexProgress::new());
    super::spawn_reindex_awaitable(Arc::clone(&handle), Arc::clone(&progress), false)
        .await
        .expect("reindex task must not panic");
    assert_eq!(progress.status.load(), super::ReindexStatus::Complete);

    let live = handle
        .indexer
        .read()
        .await
        .corpus_store()
        .expect("corpus attached");
    let rows = live.load_file_hashes().expect("read hash table");
    assert!(
        !rows.iter().any(|(path, _)| path == "ghost-8147.rs"),
        "#8147: a data-dir index clears its hash table on a root move. Rows: {rows:?}"
    );
}
