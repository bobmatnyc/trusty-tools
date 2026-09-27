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
