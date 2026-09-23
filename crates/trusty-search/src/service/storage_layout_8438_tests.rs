//! Regression tests for #8438: write paths honour the registry's layout.
//!
//! Why: every write path used to probe `<root>/.trusty-search/` instead of
//! reading `PersistedIndex::colocated`. Each test registers an index
//! `colocated=false` whose root ALREADY holds an empty `.trusty-search/` and
//! asserts that directory stays empty while the data dir receives the write —
//! plus the reverse case, the guard, `delete_data`, and a warm-boot round trip.
//! What: the shared [`Fixture`] owns an isolated `TRUSTY_DATA_DIR` and a root;
//! the swap and shutdown paths reuse it from their own test modules.
//! Test: this file, `service::reindex::registry_layout_8438_tests`, and
//! `service::shutdown_flush::shutdown_flush_8438_tests`.

use super::*;
use crate::core::registry::{IndexHandle, IndexId};
use crate::core::store::{UsearchStore, VectorStore as _};
use crate::core::CodeIndexer;
use std::sync::Arc;

/// An isolated data dir plus an index root with an EMPTY in-repo
/// `.trusty-search/`. Callers must be `#[serial_test::serial]`.
pub(crate) struct Fixture {
    pub(crate) data: tempfile::TempDir,
    pub(crate) root: tempfile::TempDir,
}

impl Fixture {
    /// `repo_dir` decides whether `<root>/.trusty-search/` exists up front.
    pub(crate) fn new(repo_dir: bool) -> Self {
        let data = tempfile::tempdir().expect("data tempdir");
        let root = tempfile::tempdir().expect("root tempdir");
        unsafe { std::env::set_var("TRUSTY_DATA_DIR", data.path()) };
        if repo_dir {
            std::fs::create_dir_all(root.path().join(COLOCATED_DIR_NAME)).expect("repo dir");
        }
        Self { data, root }
    }

    pub(crate) fn repo_dir(&self) -> PathBuf {
        self.root.path().join(COLOCATED_DIR_NAME)
    }

    pub(crate) fn data_index_dir(&self, id: &str) -> PathBuf {
        self.data
            .path()
            .join("indexes")
            .join(persistence::sanitize_id_for_path(id))
    }

    /// Panics unless `<root>/.trusty-search/` exists and holds nothing.
    pub(crate) fn assert_repo_dir_empty(&self) {
        let entries: Vec<_> = std::fs::read_dir(self.repo_dir())
            .expect("repo dir must still exist")
            .map(|e| e.expect("entry").path())
            .collect();
        assert!(
            entries.is_empty(),
            "#8438: the in-repo .trusty-search/ of a colocated=false index must stay \
             empty, found {entries:?}"
        );
    }

    /// A handle on `id` whose indexer carries `layout` and a 20-vector store.
    pub(crate) async fn handle(&self, id: &str, layout: StorageLayout) -> IndexHandle {
        let store = UsearchStore::new(4).expect("store");
        let items: Vec<(String, Vec<f32>)> = (0..20)
            .map(|i| (format!("chunk:{i}"), vec![i as f32 + 1.0, 0.0, 0.0, 0.0]))
            .collect();
        store.upsert_batch(&items).await.expect("upsert");
        let mut indexer = CodeIndexer::new(id, self.root.path()).with_storage_layout(layout);
        indexer.set_store(Arc::new(store));
        IndexHandle::bare(
            IndexId::new(id.to_string()),
            Arc::new(tokio::sync::RwLock::new(indexer)),
            self.root.path().to_path_buf(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("TRUSTY_DATA_DIR") };
    }
}

/// Delete the fixture's index root, leaving its parent in place.
///
/// Why (#8438): a colocated write used to `create_dir_all` the root back into
/// existence, defeating the #484 missing-root guard at warm boot.
pub(crate) fn remove_root(fx: &Fixture) {
    std::fs::remove_dir_all(fx.root.path()).expect("remove root");
    assert!(!fx.root.path().exists(), "precondition: the root is gone");
}

/// Poll up to 10 s until the detached incremental persist task has finished
/// (`PersistState::in_flight` back to `false`). `force_incremental_persist`
/// sets `in_flight` before it spawns, so a `false` read means the task ran.
pub(crate) async fn wait_persist_task_done(indexer: &CodeIndexer) -> bool {
    for _ in 0..1000 {
        if !indexer.persist_flags_for_tests().0 {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    false
}

/// Poll up to 10 s for `path` to exist (the incremental persist is detached).
pub(crate) async fn wait_for(path: &Path) -> bool {
    for _ in 0..200 {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    path.exists()
}

/// Why: the layout is the registry flag, whatever exists on disk.
/// Test: this test.
#[test]
fn layout_follows_the_registry_flag_not_the_directory() {
    let entry = PersistedIndex::default();
    assert_eq!(StorageLayout::for_entry(&entry), StorageLayout::DataDir);
    let entry = PersistedIndex {
        colocated: true,
        ..entry
    };
    assert_eq!(StorageLayout::for_entry(&entry), StorageLayout::Colocated);
    assert_eq!(StorageLayout::default(), StorageLayout::DataDir);
}

/// Why (#8438): the incremental persist — reached by every committed batch,
/// `index_file`, and watcher batches — wrote `hnsw.usearch` into the repo
/// while `chunks.json` went to the data dir.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn incremental_persist_writes_the_data_dir_not_the_repo() {
    let fx = Fixture::new(true);
    let handle = fx.handle("ts-8438-persist", StorageLayout::DataDir).await;
    handle.indexer.read().await.force_incremental_persist();
    let target = fx.data_index_dir("ts-8438-persist").join(HNSW_FILE);
    assert!(wait_for(&target).await, "HNSW must land in the data dir");
    fx.assert_repo_dir_empty();
}

/// Why (#8438 reverse case): a `colocated=true` entry whose directory is
/// missing must create `<root>/.trusty-search/` and write there — the old
/// probe sent it to the data dir instead.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn colocated_layout_creates_a_missing_directory() {
    let fx = Fixture::new(false);
    let handle = fx.handle("ts-8438-coloc", StorageLayout::Colocated).await;
    handle.indexer.read().await.force_incremental_persist();
    let target = fx.repo_dir().join(HNSW_FILE);
    assert!(
        wait_for(&target).await,
        "HNSW must land in <root>/.trusty-search/"
    );
    assert!(
        !fx.data_index_dir("ts-8438-coloc").join(HNSW_FILE).exists(),
        "a colocated index must not write its HNSW snapshot into the data dir"
    );
}

/// Why (#8438): the incremental persist (watcher batches, `index_file`) of a
/// colocated index whose root was deleted recreated `<root>` through
/// `create_dir_all`, so the #484 warm-boot guard passed and `git worktree add`
/// at that path failed. The skip must also leave `dirty` set.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn colocated_persist_never_recreates_a_missing_root() {
    let fx = Fixture::new(false);
    let handle = fx.handle("ts-8438-gone", StorageLayout::Colocated).await;
    remove_root(&fx);
    let indexer = handle.indexer.read().await;
    indexer.force_incremental_persist();
    assert!(
        wait_persist_task_done(&indexer).await,
        "the persist task must finish"
    );
    assert!(
        !fx.root.path().exists(),
        "#8438: a colocated persist must never recreate a deleted root"
    );
    assert!(
        indexer.persist_flags_for_tests().1,
        "a skipped persist must not clear `dirty` as if it had written"
    );
}

/// Why (#8438): a data dir that resolves INTO the repo's `.trusty-search/`
/// must be refused with an error, and the write path must write nothing.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn guard_refuses_a_data_dir_that_resolves_into_the_repo() {
    let fx = Fixture::new(true);
    unsafe { std::env::set_var("TRUSTY_DATA_DIR", fx.repo_dir()) };
    let err = StorageLayout::DataDir
        .storage_dir("ts-8438-guard", fx.root.path())
        .expect_err("a data dir under <root>/.trusty-search must be refused");
    assert!(is_write_refusal(&err), "wrong error: {err:#}");

    let handle = fx.handle("ts-8438-guard", StorageLayout::DataDir).await;
    let indexer = handle.indexer.read().await;
    indexer.force_incremental_persist();
    // #8438: observe the detached persist attempt finish, never a fixed sleep.
    assert!(
        wait_persist_task_done(&indexer).await,
        "the persist task must finish"
    );
    fx.assert_repo_dir_empty();
    assert!(
        indexer.persist_flags_for_tests().1,
        "a refused persist must not clear `dirty` as if it had written"
    );
    drop(indexer);

    // Colocated is the registry naming that directory: allowed.
    assert!(StorageLayout::Colocated
        .storage_dir("ts-8438-guard", fx.root.path())
        .is_ok());
}

/// Why (#8438): `delete_data` must remove the directory the registry names —
/// the colocated one included — and never the in-repo directory of a
/// non-colocated index.
/// Test: this test.
#[test]
#[serial_test::serial]
fn delete_data_removes_the_directory_the_registry_names() {
    let fx = Fixture::new(true);
    std::fs::write(fx.repo_dir().join("marker"), b"other instance").unwrap();
    let data = persistence::index_data_dir("ts-8438-del").unwrap();
    StorageLayout::DataDir
        .remove_storage("ts-8438-del", Some(fx.root.path()))
        .unwrap();
    assert!(!data.exists(), "the data dir must be removed");
    assert!(
        fx.repo_dir().join("marker").exists(),
        "a non-colocated delete must not touch <root>/.trusty-search/"
    );

    // A colocated delete of a directory holding only index files removes it.
    std::fs::remove_file(fx.repo_dir().join("marker")).unwrap();
    for name in [REDB_FILE, HNSW_FILE, "hnsw.keys.json", SCHEMA_VERSION_FILE] {
        std::fs::write(fx.repo_dir().join(name), b"index bytes").unwrap();
    }
    StorageLayout::Colocated
        .remove_storage("ts-8438-del", Some(fx.root.path()))
        .unwrap();
    assert!(
        !fx.repo_dir().exists(),
        "a colocated delete must remove <root>/.trusty-search/"
    );
}

/// Why (#8438): `$HOME/.trusty-search/` is both a colocated index dir (for an
/// index rooted at `$HOME`) and the daemon's runtime dir; `delete_data` must
/// remove only the index files and keep the directory while anything else is
/// in it.
/// Test: this test.
#[test]
#[serial_test::serial]
fn delete_data_keeps_foreign_files_in_a_shared_trusty_search_dir() {
    let fx = Fixture::new(true);
    let own = [
        REDB_FILE,
        HNSW_FILE,
        "hnsw.keys.json",
        HNSW_STAGING_FILE,
        REDB_TMP_FILE,
        SCHEMA_VERSION_FILE,
        CHUNKS_JSON_FILE,
    ];
    for name in own {
        std::fs::write(fx.repo_dir().join(name), b"index bytes").unwrap();
    }
    let foreign = ["config.toml", "http_addr", "mcp_http_addr"];
    for name in foreign {
        std::fs::write(fx.repo_dir().join(name), b"daemon runtime").unwrap();
    }

    StorageLayout::Colocated
        .remove_storage("ts-8438-shared", Some(fx.root.path()))
        .unwrap();

    for name in own {
        assert!(
            !fx.repo_dir().join(name).exists(),
            "index file {name} must be removed"
        );
    }
    for name in foreign {
        assert_eq!(
            std::fs::read(fx.repo_dir().join(name)).unwrap(),
            b"daemon runtime",
            "#8438: foreign file {name} next to index.redb must survive delete_data"
        );
    }
}

/// Why (#8438): the split brain — chunks in the data dir, HNSW in the repo —
/// only shows on the next boot. Build, write, persist, drop, rebuild from the
/// same `colocated=false` entry: both halves must come back.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn warm_boot_round_trip_keeps_chunks_and_hnsw_together() {
    let fx = Fixture::new(true);
    let embedder: Arc<dyn crate::core::embed::Embedder> =
        Arc::new(trusty_common::embedder::MockEmbedder::new(8));
    let entry = PersistedIndex {
        id: "ts-8438-boot".into(),
        root_path: fx.root.path().to_path_buf(),
        ..Default::default()
    };
    {
        let indexer =
            crate::service::persistence_loader::build_indexer_from_entry(&entry, &embedder)
                .await
                .expect("build");
        indexer
            .index_file(
                "src/lib.rs",
                "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { 2 }\n",
            )
            .await
            .expect("index_file");
        indexer.force_incremental_persist();
        let hnsw = fx.data_index_dir("ts-8438-boot").join(HNSW_FILE);
        assert!(wait_for(&hnsw).await, "HNSW must land in the data dir");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    fx.assert_repo_dir_empty();

    let reloaded = crate::service::persistence_loader::build_indexer_from_entry(&entry, &embedder)
        .await
        .expect("rebuild");
    assert!(reloaded.chunk_count() > 0, "chunks must survive the reboot");
    assert!(
        reloaded.vector_count().await.unwrap_or(0) > 0,
        "HNSW vectors must survive the reboot from the same directory as the chunks"
    );
    fx.assert_repo_dir_empty();
}
