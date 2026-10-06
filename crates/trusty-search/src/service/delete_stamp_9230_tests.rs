//! #9230: a committed delete stamps `_meta["reindexed_unix"]`; a delete whose
//! fail-closed redb step failed does not, and keeps the caller's tolerance.
//!
//! Why: `search.project.resolve` ranks two checkouts of one repo by that
//! stamp, so a clone whose latest change was a deletion lost to a stale one.
//! What: the three delete paths — boot reconcile's `apply_delta`, the rescan
//! sweep and the watcher's `handle_removed` — each delete one file under an
//! injected redb delete fault, then a second file with no fault.
//! Test: `cargo test -p trusty-search -- delete_stamp_9230`.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::core::corpus::CorpusStore;
use crate::core::indexer::TEST_FAIL_CHUNK_DELETE;
use crate::core::registry::{IndexHandle, IndexId};
use crate::core::CodeIndexer;
use crate::service::watch_loop::handle_removed;
use crate::service::watch_rescan::reconcile_with_policy;
use crate::service::IndexedFiles;

/// The stamp planted after setup, so "moved" is visible as a change.
const OLD_STAMP: u64 = 100;
const FILES: [(&str, &str); 2] = [
    ("src/one.rs", "pub fn one_9230() {}\n"),
    ("src/two.rs", "pub fn two_9230() {}\n"),
];

#[derive(Clone, Copy, Debug)]
enum Site {
    Reconcile,
    Rescan,
    Watcher,
}

/// One delete path's fixture: a tree holding [`FILES`] and an indexer over a
/// redb corpus kept outside the tree, so the rescan walk never sees it.
struct Fixture {
    _tree: tempfile::TempDir,
    _store: tempfile::TempDir,
    root: PathBuf,
    id: IndexId,
    indexer: Arc<RwLock<CodeIndexer>>,
    files: IndexedFiles,
}

impl Fixture {
    async fn new(site: Site) -> Self {
        let tree = tempfile::tempdir().expect("tree");
        let root = tree.path().canonicalize().expect("canonical root");
        for (rel, content) in FILES {
            std::fs::create_dir_all(root.join("src")).expect("src");
            std::fs::write(root.join(rel), content).expect("write");
        }
        let store = tempfile::tempdir().expect("store");
        let id = IndexId::new(format!("delete-stamp-9230-{site:?}").to_lowercase());
        let mut indexer = CodeIndexer::new(id.0.as_str(), root.as_path());
        indexer.set_corpus_store(Arc::new(
            CorpusStore::open(&store.path().join("index.redb")).expect("open corpus"),
        ));
        let fixture = Self {
            _tree: tree,
            _store: store,
            root,
            id,
            indexer: Arc::new(RwLock::new(indexer)),
            files: IndexedFiles::new(),
        };
        fixture.index_all(site).await;
        fixture
    }

    /// Index both files the way `site` would have, so it knows them.
    async fn index_all(&self, site: Site) {
        if let Site::Rescan = site {
            self.rescan().await;
            return;
        }
        let idx = self.indexer.read().await;
        for (rel, content) in FILES {
            idx.index_file(rel, content).await.expect("setup write");
            let ids = idx.chunk_ids_for_file(rel).await;
            assert!(!ids.is_empty(), "setup: {rel} must hold chunks");
            self.files.record(PathBuf::from(rel), ids).await;
        }
    }

    async fn rescan(&self) -> crate::service::watch_rescan::RescanStats {
        reconcile_with_policy(
            &self.id,
            &self.root,
            &self.root,
            &self.indexer,
            &self.files,
            None,
        )
        .await
        .expect("rescan pass")
    }

    /// Delete `rel` from disk, then let `site` remove it from the index.
    async fn delete(&self, site: Site, rel: &str) {
        std::fs::remove_file(self.root.join(rel)).expect("remove from disk");
        match site {
            Site::Reconcile => {
                let handle = Arc::new(IndexHandle::bare(
                    self.id.clone(),
                    self.indexer.clone(),
                    self.root.clone(),
                ));
                let delta = vec![rel.to_string()];
                let stamped =
                    crate::service::reconcile::apply_delta(&handle, &self.id.0, &delta, "sha")
                        .await;
                assert!(stamped, "{site:?}: the delta counts the removal");
            }
            Site::Rescan => {
                assert_eq!(self.rescan().await.files_removed, 1, "{site:?}");
            }
            Site::Watcher => {
                handle_removed(
                    &self.root.join(rel),
                    &self.id,
                    &self.root,
                    &self.root,
                    &self.indexer,
                    &self.files,
                )
                .await;
            }
        }
        let left = self.indexer.read().await.chunk_ids_for_file(rel).await;
        assert!(left.is_empty(), "{site:?}: {rel} left memory: {left:?}");
    }

    async fn stamp(&self) -> Option<u64> {
        let corpus = self.indexer.read().await.corpus_store().expect("corpus");
        corpus.read_reindexed_unix_sync().expect("read stamp")
    }
}

fn set_fault(id: &IndexId, on: bool) {
    let mut faults = TEST_FAIL_CHUNK_DELETE.lock().expect("seam");
    faults.retain(|f| f != &id.0);
    if on {
        faults.push(id.0.clone());
    }
}

/// Why: #9230 review — a delete that removed rows from redb is a committed
/// write and stamps the corpus; one whose fail-closed redb delete failed is
/// not, and still drops the file from memory as the caller always did.
/// Fails against 8fe9e93737 at the second stamp assertion: no delete path
/// stamped.
/// Test: this test.
#[tokio::test]
async fn a_failed_delete_does_not_stamp_and_the_next_committed_delete_does() {
    for site in [Site::Reconcile, Site::Rescan, Site::Watcher] {
        let fx = Fixture::new(site).await;
        let corpus = fx.indexer.read().await.corpus_store().expect("corpus");
        corpus.write_reindexed_unix_sync(OLD_STAMP).expect("plant");

        set_fault(&fx.id, true);
        fx.delete(site, FILES[0].0).await;
        set_fault(&fx.id, false);
        assert_eq!(
            fx.stamp().await,
            Some(OLD_STAMP),
            "{site:?}: a delete redb refused must not stamp"
        );

        fx.delete(site, FILES[1].0).await;
        let got = fx.stamp().await.expect("stamp");
        assert!(got > OLD_STAMP, "{site:?}: a committed delete must stamp");
    }
}
