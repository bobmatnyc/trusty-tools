//! #9230: a committed delete stamps `_meta["reindexed_unix"]`; a delete whose
//! fail-closed redb step failed does not, reports the failure, and leaves the
//! file's chunks in memory for a retry.
//!
//! Why: `search.project.resolve` ranks two checkouts of one repo by that
//! stamp, so a clone whose latest change was a deletion lost to a stale one.
//! What: the six delete paths — boot reconcile's `apply_delta`, the rescan
//! sweep, the watcher's `handle_removed` and `handle_modified` (an edit to
//! sops-encrypted content), `remove_file_report` (HTTP, socket and MCP `remove-file`)
//! and an excluded pushed write's `purge_pushed` — each delete one file with
//! no fault, then a second under an injected redb delete fault, then retry the
//! second. One test per path, so a red path never hides another.
//! Test: `cargo test -p trusty-search -- delete_stamp_9230`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::StatusCode;
use tokio::sync::RwLock;

use crate::core::corpus::CorpusStore;
use crate::core::indexer::TEST_FAIL_CHUNK_DELETE;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::CodeIndexer;
use crate::service::server::{remove_file_report, RemoveFileRequest, SearchAppState};
use crate::service::watch_loop::{handle_modified, handle_removed};
use crate::service::watch_rescan::reconcile_with_policy;
use crate::service::IndexedFiles;

/// The stamp planted after setup, so "moved" is visible as a change.
const OLD_STAMP: u64 = 100;
/// sops-encrypted content: a watcher edit to it commits no chunks.
const SOPS: &str = "password: ENC[AES256_GCM,data:aGk=,type:str]\nsops:\n    version: 3.8.1\n";
const FILES: [(&str, &str); 2] = [
    ("src/one.rs", "pub fn one_9230() {}\n"),
    ("src/two.rs", "pub fn two_9230() {}\n"),
];

#[derive(Clone, Copy, Debug)]
enum Site {
    Reconcile,
    Rescan,
    Watcher,
    WatcherModified,
    RemoveFile,
    PurgePushed,
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

    /// Remove `rel` at `site` — deleted from disk, or rewritten as sops
    /// content for [`Site::WatcherModified`] — and assert the site's own outcome: a
    /// success, or with `faulted` the failure it reports and a retryable
    /// state (chunks in memory, the file still tracked).
    async fn delete(&self, site: Site, rel: &str, faulted: bool) {
        let path = self.root.join(rel);
        if let Site::WatcherModified = site {
            std::fs::write(&path, SOPS).expect("encrypt the file");
        } else if path.exists() {
            std::fs::remove_file(&path).expect("remove from disk");
        }
        let before = self.ids(rel).await;
        match site {
            Site::Reconcile => {
                let handle = Arc::new(self.handle());
                let delta = vec![rel.to_string()];
                let stamped =
                    crate::service::reconcile::apply_delta(&handle, &self.id.0, &delta, "sha")
                        .await;
                assert_eq!(
                    stamped, !faulted,
                    "{site:?}: a refused delete counts as failed"
                );
            }
            Site::Rescan => {
                let stats = self.rescan().await;
                assert_eq!(stats.files_removed, usize::from(!faulted), "{site:?}");
                assert_eq!(stats.files_failed, usize::from(faulted), "{site:?}");
                assert_eq!(stats.is_complete(), !faulted, "{site:?}: a failure re-arms");
            }
            Site::Watcher => {
                handle_removed(
                    &path,
                    &self.id,
                    &self.root,
                    &self.root,
                    &self.indexer,
                    &self.files,
                )
                .await;
            }
            Site::WatcherModified => {
                handle_modified(
                    &path,
                    &self.id,
                    &self.root,
                    &self.root,
                    &self.indexer,
                    &self.files,
                )
                .await;
            }
            Site::RemoveFile => {
                let registry = IndexRegistry::new();
                registry.register(self.handle());
                let state = Arc::new(SearchAppState::new(registry));
                let req = RemoveFileRequest {
                    path: rel.to_string(),
                };
                match remove_file_report(&state, &self.id.0, req).await {
                    Ok(body) => {
                        assert!(!faulted, "{site:?}: a refused delete answered {body}");
                        assert!(body["removed_chunks"].as_u64() > Some(0), "{body}");
                    }
                    Err((status, body)) => {
                        assert!(faulted, "{site:?}: {status} {body}");
                        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
                        assert_eq!(body["error"], "remove_file_failed", "{body}");
                    }
                }
            }
            Site::PurgePushed => {
                let mut handle = self.handle();
                handle.exclude_globs = vec!["**/src/**".into()];
                let idx = self.indexer.read().await;
                let (status, body) =
                    crate::service::write_admission::gate(&handle, &idx, rel, "pub fn x() {}\n")
                        .await
                        .expect_err("an excluded pushed write is refused");
                if faulted {
                    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
                    assert_eq!(body["error"], "index_file_failed", "{body}");
                } else {
                    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
                    assert!(body["removed_chunks"].as_u64() > Some(0), "{body}");
                }
            }
        }
        let left = self.ids(rel).await;
        if !faulted {
            assert!(left.is_empty(), "{site:?}: {rel} left memory: {left:?}");
            return;
        }
        assert_eq!(left, before, "{site:?}: a refused delete changed memory");
        if matches!(site, Site::Rescan | Site::Watcher | Site::WatcherModified) {
            let mut tracked = self.files.take(Path::new(rel)).await.unwrap_or_default();
            tracked.sort();
            assert_eq!(tracked, before, "{site:?}: entry not kept");
            self.files.record(PathBuf::from(rel), tracked).await;
        }
    }

    /// `rel`'s chunk ids in memory, sorted so two reads compare equal.
    async fn ids(&self, rel: &str) -> Vec<String> {
        let mut ids = self.indexer.read().await.chunk_ids_for_file(rel).await;
        ids.sort();
        ids
    }

    fn handle(&self) -> IndexHandle {
        IndexHandle::bare(self.id.clone(), self.indexer.clone(), self.root.clone())
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
/// not, and must stay retryable rather than drop the file from memory.
/// What: with an old stamp planted each time, deletes one file at `site`,
/// then a second under the redb fault, then retries the second.
async fn assert_only_a_committed_delete_stamps(site: Site) {
    let fx = Fixture::new(site).await;
    let corpus = fx.indexer.read().await.corpus_store().expect("corpus");

    corpus.write_reindexed_unix_sync(OLD_STAMP).expect("plant");
    fx.delete(site, FILES[0].0, false).await;
    let got = fx.stamp().await.expect("stamp");
    assert!(got > OLD_STAMP, "{site:?}: a committed delete must stamp");

    corpus.write_reindexed_unix_sync(OLD_STAMP).expect("plant");
    set_fault(&fx.id, true);
    fx.delete(site, FILES[1].0, true).await;
    set_fault(&fx.id, false);
    assert_eq!(
        fx.stamp().await,
        Some(OLD_STAMP),
        "{site:?}: a delete redb refused must not stamp"
    );

    fx.delete(site, FILES[1].0, false).await;
    let got = fx.stamp().await.expect("stamp");
    assert!(got > OLD_STAMP, "{site:?}: the retried delete must stamp");
}

/// Boot reconcile's `apply_delta`. Fails against 8fe9e93737: no delete
/// stamped; against cb543af1de: the refused delete dropped memory.
#[tokio::test]
async fn reconcile_delete_stamps_only_when_committed() {
    assert_only_a_committed_delete_stamps(Site::Reconcile).await;
}

/// The rescan sweep's `drop_file`. Fails against 8fe9e93737 and cb543af1de.
#[tokio::test]
async fn rescan_delete_stamps_only_when_committed() {
    assert_only_a_committed_delete_stamps(Site::Rescan).await;
}

/// The watcher's `handle_removed`. Fails against 8fe9e93737 and cb543af1de.
#[tokio::test]
async fn watcher_delete_stamps_only_when_committed() {
    assert_only_a_committed_delete_stamps(Site::Watcher).await;
}

/// The watcher's `handle_modified` stale-chunk delete, for an edit whose new
/// content commits no chunks (sops). Fails against cb543af1de: no stamp.
#[tokio::test]
async fn watcher_edit_delete_stamps_only_when_committed() {
    assert_only_a_committed_delete_stamps(Site::WatcherModified).await;
}

/// `POST /indexes/{id}/remove-file`, the socket RPC and the MCP tool all
/// serve `remove_file_report`. Fails against 7a7e7479d4: it deleted
/// warn-only and never stamped; against cb543af1de: a refused delete
/// answered 200.
#[tokio::test]
async fn remove_file_report_stamps_only_a_committed_delete() {
    assert_only_a_committed_delete_stamps(Site::RemoveFile).await;
}

/// An excluded pushed `index-file` write purges through `purge_pushed`.
/// Fails against 7a7e7479d4: it purged warn-only and never stamped; against
/// cb543af1de: a refused purge answered 403.
#[tokio::test]
async fn excluded_pushed_write_purge_stamps_only_when_committed() {
    assert_only_a_committed_delete_stamps(Site::PurgePushed).await;
}
