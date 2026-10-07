//! #9230/#9212 fail-closed deletes under an ADDITIONAL index root (#7434).
//!
//! Why: the watcher's delete, the rescan sweep and their `reindexed_unix`
//! stamp key every file through the root that owns it. Keyed through the
//! primary root, an additional root's delete misses its `@root<n>/…` entry, or
//! the sweep stats it under the wrong tree and drops a live file.
//! What: an index over a primary and one additional root, a redb corpus kept
//! outside both trees, and the `TEST_FAIL_CHUNK_DELETE` fault seam.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::core::corpus::CorpusStore;
use crate::core::indexer::TEST_FAIL_CHUNK_DELETE;
use crate::core::registry::IndexId;
use crate::core::CodeIndexer;
use crate::service::watch_loop::handle_removed_in_root;
use crate::service::watch_rescan::reconcile_with_policy;
use crate::service::watch_roots::WatchedRoot;
use crate::service::IndexedFiles;

const OLD_STAMP: u64 = 100;
const CONTENT: &str = "pub fn extra_7434() {}\n";

struct Fixture {
    _trees: [tempfile::TempDir; 3],
    extra: PathBuf,
    table: Vec<WatchedRoot>,
    id: IndexId,
    indexer: Arc<RwLock<CodeIndexer>>,
    files: IndexedFiles,
}

impl Fixture {
    /// Index `@root1/<rel>` for each of `rels`, all of them on disk.
    async fn new(name: &str, rels: &[&str]) -> Self {
        let (primary_dir, extra_dir) = (
            tempfile::tempdir().expect("p"),
            tempfile::tempdir().expect("e"),
        );
        let store = tempfile::tempdir().expect("store");
        let primary = primary_dir.path().canonicalize().expect("canonical");
        let extra = extra_dir.path().canonicalize().expect("canonical");
        let id = IndexId::new(format!("delete-stamp-7434-{name}"));
        let mut indexer = CodeIndexer::new(id.0.as_str(), primary.as_path());
        indexer.set_additional_roots(vec![extra.clone()]);
        indexer.set_corpus_store(Arc::new(
            CorpusStore::open(&store.path().join("index.redb")).expect("corpus"),
        ));
        let fx = Self {
            _trees: [primary_dir, extra_dir, store],
            table: WatchedRoot::table(&primary, std::slice::from_ref(&extra)),
            extra,
            id,
            indexer: Arc::new(RwLock::new(indexer)),
            files: IndexedFiles::new(),
        };
        let idx = fx.indexer.read().await;
        for rel in rels {
            let path = fx.extra.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(&path, CONTENT).expect("write");
            let key = format!("@root1/{rel}");
            idx.index_file(&key, CONTENT).await.expect("seed");
            let ids = idx.chunk_ids_for_file(&key).await;
            assert!(!ids.is_empty(), "setup: {key} must hold chunks");
            fx.files.record(PathBuf::from(&key), ids).await;
        }
        drop(idx);
        fx.plant().await;
        fx
    }

    async fn plant(&self) {
        let corpus = self.indexer.read().await.corpus_store().expect("corpus");
        corpus.write_reindexed_unix_sync(OLD_STAMP).expect("plant");
    }

    async fn stamp(&self) -> Option<u64> {
        let corpus = self.indexer.read().await.corpus_store().expect("corpus");
        corpus.read_reindexed_unix_sync().expect("read stamp")
    }

    async fn ids(&self, key: &str) -> Vec<String> {
        self.indexer.read().await.chunk_ids_for_file(key).await
    }

    fn fault(&self, on: bool) {
        let mut faults = TEST_FAIL_CHUNK_DELETE.lock().expect("seam");
        faults.retain(|f| f != &self.id.0);
        if on {
            faults.push(self.id.0.clone());
        }
    }
}

/// Why: the watcher's `Removed` arm under an additional root must find the
/// file under its `@root1/…` key, and a delete redb refuses must keep the
/// chunks, re-track the key and withhold the stamp; the retry then deletes and
/// stamps. Keyed through the primary root, the delete finds nothing at all.
/// Test: this test.
#[tokio::test]
async fn a_failed_delete_under_an_additional_root_withholds_its_stamp() {
    let fx = Fixture::new("watcher", &["src/x.rs"]).await;
    let key = "@root1/src/x.rs";
    let path = fx.extra.join("src/x.rs");
    std::fs::remove_file(&path).expect("delete from disk");
    let before = fx.ids(key).await;

    fx.fault(true);
    handle_removed_in_root(&path, &fx.id, &fx.table[1], &fx.indexer, &fx.files).await;
    fx.fault(false);
    assert_eq!(
        fx.stamp().await,
        Some(OLD_STAMP),
        "a refused delete must not stamp"
    );
    assert_eq!(
        fx.ids(key).await,
        before,
        "a refused delete keeps the chunks"
    );
    assert!(
        fx.files.paths().await.contains(&PathBuf::from(key)),
        "the refused delete re-tracks the @root1 key for a retry"
    );

    handle_removed_in_root(&path, &fx.id, &fx.table[1], &fx.indexer, &fx.files).await;
    assert!(fx.ids(key).await.is_empty(), "the retry deletes the chunks");
    assert!(fx.stamp().await.expect("stamp") > OLD_STAMP, "and stamps");
}

/// Why: the rescan sweep stats each tracked key at its own root. A gone
/// additional-root file whose purge redb refuses is counted failed and
/// withholds the stamp; a live one the walker skips (`node_modules`) must not
/// be swept at all — stat'd under the primary root it reads as deleted.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rescan_does_not_sweep_another_root_s_files() {
    let fx = Fixture::new("rescan", &["src/gone.rs", "node_modules/kept.rs"]).await;
    std::fs::remove_file(fx.extra.join("src/gone.rs")).expect("delete from disk");

    fx.fault(true);
    let stats = reconcile_with_policy(&fx.id, &fx.table, &fx.indexer, &fx.files, None)
        .await
        .expect("pass");
    fx.fault(false);
    assert_eq!(stats.files_failed, 1, "{stats:?}");
    assert_eq!(stats.files_removed, 0, "{stats:?}");
    assert_eq!(
        fx.stamp().await,
        Some(OLD_STAMP),
        "a refused purge must not stamp"
    );

    let stats = reconcile_with_policy(&fx.id, &fx.table, &fx.indexer, &fx.files, None)
        .await
        .expect("pass");
    assert_eq!(stats.files_removed, 1, "only the gone file: {stats:?}");
    assert!(fx.stamp().await.expect("stamp") > OLD_STAMP);
    let tracked = fx.files.paths().await;
    assert!(
        tracked.contains(&PathBuf::from("@root1/node_modules/kept.rs")),
        "a live additional-root file must survive the sweep: {tracked:?}"
    );
    assert!(!fx.ids("@root1/node_modules/kept.rs").await.is_empty());
    assert!(!tracked.contains(&PathBuf::from("@root1/src/gone.rs")));
}

/// Why: a dropped-event rescan walks every root and keys each file through
/// the root that owns it, as the watcher and the reindex walk do.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rescan_covers_every_root() {
    let fx = Fixture::new("covers", &[]).await;
    let primary = fx.table[0].canonical().to_path_buf();
    std::fs::write(primary.join("in_primary.rs"), "fn p() {}\n").expect("write");
    std::fs::write(fx.extra.join("in_extra.rs"), "fn e() {}\n").expect("write");

    let stats = reconcile_with_policy(&fx.id, &fx.table, &fx.indexer, &fx.files, None)
        .await
        .expect("pass");
    assert_eq!(stats.files_reindexed, 2, "{stats:?}");
    let tracked = fx.files.paths().await;
    assert!(
        tracked.contains(&PathBuf::from("in_primary.rs")),
        "{tracked:?}"
    );
    assert!(
        tracked.contains(&PathBuf::from("@root1/in_extra.rs")),
        "{tracked:?}"
    );
    assert!(Path::new(&fx.extra).join("in_extra.rs").exists());
}

/// Why (#7434 review, HIGH): a watch keeps the root table it was spawned
/// with. After an add-root, the primary root's watch is still single-root; a
/// dropped-event rescan from it walks the live handle (both roots), found no
/// owner for the new root's files, keyed them ABSOLUTE, and its sweep then
/// stat'd the tracked `@root1/…` key under the primary and dropped it.
/// What: the registry holds the two-root handle; the rescan is handed the
/// stale one-root table, exactly as the primary watch would.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rescan_from_a_watch_with_a_stale_root_table_keeps_the_new_roots_keys() {
    let fx = Fixture::new("stale-table", &["src/x.rs"]).await;
    let primary = fx.table[0].canonical().to_path_buf();
    let mut handle = crate::core::registry::IndexHandle::bare(
        fx.id.clone(),
        Arc::clone(&fx.indexer),
        primary.clone(),
    );
    handle.additional_roots = vec![fx.extra.clone()];
    let registry = crate::core::registry::IndexRegistry::new();
    registry.register(handle);
    let stale = WatchedRoot::table(&primary, &[]);

    crate::service::watch_rescan::reconcile_registered(
        &fx.id,
        &stale,
        &fx.indexer,
        &fx.files,
        Some(&registry),
    )
    .await
    .expect("pass");

    let tracked = fx.files.paths().await;
    assert!(
        tracked.iter().all(|k| !k.is_absolute()),
        "no walked file may be keyed absolute: {tracked:?}"
    );
    assert!(
        tracked.contains(&PathBuf::from("@root1/src/x.rs")),
        "{tracked:?}"
    );
    assert!(
        !fx.ids("@root1/src/x.rs").await.is_empty(),
        "the chunks survive"
    );
}

/// Why (#7434 delta review, HIGH): an unmounted additional root contributes no
/// walked files, so the sweep stat'd each tracked `@root1/…` key, found it
/// absent and dropped it — one rescan erased the root's chunks, and nothing
/// re-walks it when it returns. The reindex prune already fails closed here.
/// What: `@root1/src/kept.rs` under a root that is then removed, plus
/// `gone.rs` tracked under the present primary but absent from disk; the
/// rescan keeps the first and still sweeps the second.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rescan_keeps_the_chunks_of_an_absent_additional_root() {
    let fx = Fixture::new("absent-root", &["src/kept.rs"]).await;
    {
        let idx = fx.indexer.read().await;
        idx.index_file("gone.rs", CONTENT).await.expect("seed");
        let ids = idx.chunk_ids_for_file("gone.rs").await;
        assert!(!ids.is_empty(), "setup: gone.rs must hold chunks");
        fx.files.record(PathBuf::from("gone.rs"), ids).await;
    }
    std::fs::remove_dir_all(&fx.extra).expect("unmount the additional root");

    let stats = reconcile_with_policy(&fx.id, &fx.table, &fx.indexer, &fx.files, None)
        .await
        .expect("pass");

    let tracked = fx.files.paths().await;
    assert!(
        !fx.ids("@root1/src/kept.rs").await.is_empty(),
        "#7434: an absent root's chunks must be kept, not swept: {stats:?}"
    );
    assert!(
        tracked.contains(&PathBuf::from("@root1/src/kept.rs")),
        "{tracked:?}"
    );
    assert!(fx.ids("gone.rs").await.is_empty(), "control: still sweeps");
    assert_eq!(
        stats.files_removed, 1,
        "only the primary's gone file: {stats:?}"
    );
}
