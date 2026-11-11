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

/// Makes a directory read-only, so a write and a remove in it both fail;
/// restores it on drop.
struct ReadOnly(std::path::PathBuf);
impl ReadOnly {
    fn new(dir: std::path::PathBuf) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555))
            .expect("make the identity dir read-only");
        Self(dir)
    }
}
impl Drop for ReadOnly {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// The `identity` and `identity_error` fields a reply would carry now.
fn reply_fields(id: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut body = serde_json::Map::new();
    index_identity::insert_into(id, &mut body);
    body
}

/// A daemon restart: the process-local poison is gone; reload from the entry.
async fn restart(entry: &PersistedIndex) -> CodeIndexer {
    index_identity::forget_process_state(&entry.id);
    load(entry).await
}

/// Load an index, put one file, and return the identity it then reports.
async fn seeded(id: &str, root: &std::path::Path) -> (PersistedIndex, IndexIdentity) {
    let mut entry = PersistedIndex::new(id.to_string(), root.to_path_buf());
    entry.colocated = true;
    let indexer = load(&entry).await;
    indexer
        .index_file("src/lib.rs", "pub fn handler_10738() -> u32 { 7 }\n")
        .await
        .expect("index a file");
    let first = index_identity::current(id).expect("first identity");
    (entry, first)
}

/// Why: H2 — a corpus-loss bump that fails leaves the pre-event identity
/// file, and the recreated corpus exists at the next restart, so nothing
/// re-detected the loss and the stale identity was served beside it.
/// What: an old-format corpus is recreated empty while the identity dir is
/// read-only; after a restart with the dir still read-only the reply carries
/// `identity: null`; after a restart with it writable the identity is new,
/// and a further plain restart keeps that new value.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn a_corpus_loss_bump_that_failed_is_retried_after_a_restart() {
    // Root ignores directory permissions, so nothing would fail.
    if nix::unistd::geteuid().is_root() {
        return;
    }
    let isolated = IsolatedDataDir::new();
    let root = tempfile::tempdir().expect("root");
    let id = "identity-corpus-loss-restart-10738";
    let (entry, first) = seeded(id, root.path()).await;
    let redb = corpus_redb_path_for_entry(&entry).expect("redb path");

    plant_old_format_corpus(&redb);
    let guard = ReadOnly::new(isolated.path().join("index-identity"));
    let recreated = load(&entry).await;
    assert!(
        redb.with_file_name("index.redb.v2-incompatible").exists(),
        "precondition: the old-format corpus was moved aside"
    );
    index_identity::current(id).expect_err("precondition: the bump failed");
    drop(recreated);

    let reloaded = restart(&entry).await;
    let body = reply_fields(id);
    assert!(
        body["identity"].is_null(),
        "a restart must not serve the pre-event identity: {body:?} (was {first:?})"
    );
    assert!(body["identity_error"].is_string(), "{body:?}");
    drop(reloaded);

    drop(guard);
    let healed = restart(&entry).await;
    let after = index_identity::current(id).expect("a writable dir bumps");
    assert_ne!(after, first, "the healed identity must be new");
    drop(healed);
    let reopened = restart(&entry).await;
    assert_eq!(
        index_identity::current(id).expect("identity after a plain restart"),
        after,
        "a plain restart keeps the healed identity"
    );
    drop(reopened);
}

/// Why: H2 — an event that keeps the corpus (here a migration fault) leaves
/// the corpus stamp equal to the pre-event identity file when its bump fails,
/// so only the `identity_bump_pending` marker can tell a restart.
/// What: a migration fault is recorded while the identity dir is read-only;
/// a restart with the dir still read-only answers `identity: null`; a
/// restart with it writable raises the generation, and a plain restart
/// keeps that value.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn a_pending_bump_that_failed_is_retried_after_a_restart() {
    if nix::unistd::geteuid().is_root() {
        return;
    }
    let isolated = IsolatedDataDir::new();
    let root = tempfile::tempdir().expect("root");
    let id = "identity-pending-restart-10738";
    let (entry, first) = seeded(id, root.path()).await;

    let indexer = restart(&entry).await;
    assert_eq!(
        index_identity::current(id).expect("identity after a plain restart"),
        first,
        "precondition: a plain restart keeps the identity"
    );
    let guard = ReadOnly::new(isolated.path().join("index-identity"));
    indexer.record_migration_failure("test_stage_10738", "injected fault");
    index_identity::current(id).expect_err("precondition: the bump failed");
    drop(indexer);

    let reloaded = restart(&entry).await;
    let body = reply_fields(id);
    assert!(
        body["identity"].is_null(),
        "a restart must not serve the pre-event identity: {body:?} (was {first:?})"
    );
    assert!(body["identity_error"].is_string(), "{body:?}");
    drop(reloaded);

    drop(guard);
    let healed = restart(&entry).await;
    let after = index_identity::current(id).expect("a writable dir bumps");
    assert_eq!(after.instance_id, first.instance_id);
    assert!(
        after.generation > first.generation,
        "{first:?} -> {after:?}"
    );
    drop(healed);
    let reopened = restart(&entry).await;
    assert_eq!(
        index_identity::current(id).expect("identity after a plain restart"),
        after,
        "a plain restart keeps the raised generation"
    );
    drop(reopened);
}
