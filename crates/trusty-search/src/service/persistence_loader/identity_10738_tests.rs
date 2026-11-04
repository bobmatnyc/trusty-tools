//! #10738 (SPEC-OKF-01 §5.6 D1): a corpus created empty in place of the
//! stored one changes the index identity when the loader attaches it.
//!
//! Why: the identity lives outside the corpus, so a corpus that is moved aside
//! and recreated, or deleted out of band, came back empty under the identity a
//! journal had already matched.
//! What: drives the real warm-boot loader, `build_indexer_from_entry`, against
//! a colocated index: a plain reopen keeps the identity; an old-format corpus
//! that the open moves aside, and a deleted storage dir, each change it to a
//! value never reported before.
//! Test: this file IS the suite.

use std::sync::Arc;

use trusty_common::embedder::MockEmbedder;

use crate::core::indexer::CodeIndexer;
use crate::core::Embedder;
use crate::service::index_identity::{self, IndexIdentity};
use crate::service::persistence::{corpus_redb_path_for_entry, PersistedIndex};
use crate::service::persistence_loader::build_indexer_from_entry;
use crate::service::server::tests_components::IsolatedDataDir;

/// Load the index from its registry entry, as warm boot does.
async fn load(entry: &PersistedIndex) -> CodeIndexer {
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    build_indexer_from_entry(entry, &embedder)
        .await
        .expect("build the indexer from its entry")
}

/// Replace the corpus file with a genuine redb 2.x database, which the open
/// moves aside and recreates empty (#702).
fn plant_old_format_corpus(path: &std::path::Path) {
    std::fs::remove_file(path).expect("remove the live corpus");
    let db2 = redb2::Database::create(path).expect("create a redb 2.x database");
    let txn = db2.begin_write().expect("begin a 2.x write txn");
    {
        let def: redb2::TableDefinition<&str, &[u8]> = redb2::TableDefinition::new("chunks");
        let mut table = txn.open_table(def).expect("open a 2.x table");
        table
            .insert("k", b"v".as_slice())
            .expect("insert a 2.x row");
    }
    txn.commit().expect("commit the 2.x txn");
}

/// Why: H1 — every path that yields an empty corpus must change the identity,
/// or a journal keeps reporting files the index no longer holds.
/// What: put a file and read the identity; a reopen keeps it; an old-format
/// corpus recreated empty changes it; deleting the colocated storage dir (what
/// prune and an out-of-band delete do) changes it again. Each new value
/// differs from every earlier one.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn identity_changes_when_the_corpus_is_recreated_empty() {
    let _isolated = IsolatedDataDir::new();
    let root = tempfile::tempdir().expect("root");
    let id = "identity-corpus-recreated-10738";
    let mut entry = PersistedIndex::new(id.to_string(), root.path().to_path_buf());
    entry.colocated = true;
    let redb = corpus_redb_path_for_entry(&entry).expect("redb path");

    let indexer = load(&entry).await;
    indexer
        .index_file("src/lib.rs", "pub fn handler_10738() -> u32 { 7 }\n")
        .await
        .expect("index a file");
    let first = index_identity::current(id).expect("first identity");
    drop(indexer);

    let reopened = load(&entry).await;
    assert!(
        reopened.corpus_store().is_some(),
        "precondition: the reopen attached the corpus"
    );
    assert_eq!(
        index_identity::current(id).expect("identity after reopen"),
        first,
        "a reopen of the stored corpus must keep the identity"
    );
    drop(reopened);

    let mut seen: Vec<IndexIdentity> = vec![first];
    plant_old_format_corpus(&redb);
    let recreated = load(&entry).await;
    assert!(
        redb.with_file_name("index.redb.v2-incompatible").exists(),
        "precondition: the old-format corpus was moved aside"
    );
    let after_recreate = index_identity::current(id).expect("identity after recreate");
    assert!(
        !seen.contains(&after_recreate),
        "a corpus recreated empty must change the identity: {seen:?} -> {after_recreate:?}"
    );
    seen.push(after_recreate);
    drop(recreated);

    std::fs::remove_dir_all(root.path().join(".trusty-search")).expect("delete the storage dir");
    let rebuilt = load(&entry).await;
    let after_delete = index_identity::current(id).expect("identity after delete");
    assert!(
        !seen.contains(&after_delete),
        "a corpus created after its storage was deleted must change the identity: \
         {seen:?} -> {after_delete:?}"
    );
    drop(rebuilt);
}
