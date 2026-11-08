//! #10738: a force reindex, which rebuilds from an empty corpus, raises the
//! index generation; an incremental reindex does not.

use std::sync::Arc;

use super::progress::ReindexProgress;
use super::spawn_reindex_awaitable;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId};
use crate::service::index_identity::{self, IndexIdentity};

async fn reindex(handle: &Arc<IndexHandle>, force: bool) {
    spawn_reindex_awaitable(handle.clone(), Arc::new(ReindexProgress::new()), force)
        .await
        .expect("reindex task must not panic");
}

fn identity(id: &str) -> IndexIdentity {
    index_identity::current(id).expect("read the identity")
}

/// Why: a force reindex can drop chunks that were pushed and never written
/// to disk, with no per-path remove; a journal bound to the old generation
/// must see a mismatch (SPEC-OKF-01 §4.9.2, F14).
/// What: two incremental reindexes keep the identity; a force reindex keeps
/// `instance_id` and raises `generation`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
// Same reason as `hash_withhold_tests`: a `#[serial]` writer's memory limit
// would memory-abort this reindex.
#[serial_test::parallel]
async fn generation_changes_when_a_force_reindex_resets_the_index() {
    let id = "identity-force-reindex-10738";
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = tempfile::tempdir().expect("store dir");
    let root = tmp.path().to_path_buf();
    std::fs::write(
        root.join("a.rs"),
        "pub fn alpha_10738() -> u8 {\n    1\n}\n",
    )
    .expect("a.rs");
    let mut indexer = CodeIndexer::new(id, root.clone());
    indexer.set_corpus_store(Arc::new(
        crate::core::corpus::CorpusStore::open(&store.path().join("index.redb"))
            .expect("open corpus"),
    ));
    let handle = Arc::new(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root,
    ));
    let _ = index_identity::retire(id);

    reindex(&handle, false).await;
    let before = identity(id);
    reindex(&handle, false).await;
    assert_eq!(
        identity(id),
        before,
        "an incremental reindex keeps the identity"
    );

    reindex(&handle, true).await;
    let after = identity(id);
    assert_eq!(
        after.instance_id, before.instance_id,
        "{before:?} -> {after:?}"
    );
    assert!(
        after.generation > before.generation,
        "a force reindex must raise the generation: {before:?} -> {after:?}"
    );
}

const PUSHED: &str = "src/pushed_10738.rs";
const PUSHED_BODY: &str = "pub fn pushed_10738(x: u32) -> u32 {\n    x + 1\n}\n";

/// A colocated index whose live corpus holds one file, under a fresh identity,
/// and the put surface mounted on it.
///
/// Why: the corpus swap resolves its staging and live paths from the storage
/// layout, so the fixture builds the real `<root>/.trusty-search/` layout.
/// Test: used by the discarded-write cases below.
async fn staged_index(
    id: &str,
) -> (
    tempfile::TempDir,
    Arc<IndexHandle>,
    Arc<crate::service::server::SearchAppState>,
    std::path::PathBuf,
) {
    use crate::core::embed::{Embedder, MockEmbedder};
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let colocated = root.join(crate::service::colocated_storage::COLOCATED_DIR_NAME);
    std::fs::create_dir_all(&colocated).expect("colocated dir");
    let live = colocated.join(crate::service::storage_layout::REDB_FILE);
    let _ = index_identity::retire(id);
    let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(8));
    let store: Arc<dyn crate::core::store::VectorStore> =
        Arc::new(crate::core::store::UsearchStore::new(8).expect("usearch"));
    let mut indexer = CodeIndexer::new(id, &root)
        .with_components(embedder.clone(), store)
        .with_storage_layout(crate::service::storage_layout::StorageLayout::Colocated);
    indexer.set_corpus_store(Arc::new(
        crate::core::corpus::CorpusStore::open(&live).expect("open live corpus"),
    ));
    indexer
        .index_file("a.rs", "pub fn alpha_10738() -> u8 {\n    1\n}\n")
        .await
        .expect("seed the live corpus");
    let registry = crate::core::registry::IndexRegistry::new();
    let handle = registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root,
    ));
    let state = Arc::new(crate::service::server::SearchAppState::new(registry));
    state.install_embedder(embedder).await;
    (dir, handle, state, live)
}

/// Begin a staged run the way `run_reindex` does, returning the staging path.
async fn begin(handle: &Arc<IndexHandle>, force: bool) -> std::path::PathBuf {
    super::corpus_swap::begin_staged_corpus_swap(
        handle,
        &handle.id,
        force,
        None,
        None,
        crate::service::storage_layout::REDB_TMP_FILE,
    )
    .await
    .expect("staging")
    .expect("staging must engage")
}

/// Settle the staged run the way `finish_reindex` does.
async fn resolve(
    handle: &Arc<IndexHandle>,
    tmp: &std::path::Path,
    resolution: &super::staging::StagingResolution,
    force: bool,
) -> bool {
    super::finish_teardown::resolve_corpus_swap(
        handle,
        &handle.id,
        handle.root_path.as_path(),
        Some(tmp),
        resolution,
        &super::validate::ReindexOutcome::Ready,
        false,
        force,
    )
    .await
}

/// A put through the real put core; returns the generation its reply carries.
async fn put_generation(state: &Arc<crate::service::server::SearchAppState>, id: &str) -> u64 {
    let reply = crate::service::server::index_file_report(
        state,
        id,
        crate::service::server::IndexFileRequest {
            path: PUSHED.into(),
            content: PUSHED_BODY.into(),
        },
    )
    .await
    .expect("put");
    assert_eq!(reply["indexed"], true, "{reply}");
    reply["identity"]["generation"]
        .as_u64()
        .unwrap_or_else(|| panic!("put reply carries a generation: {reply}"))
}

/// Why: HIGH 1 — a put during a staged run writes into the staging corpus and
/// its reply carries generation G. A rollback deletes staging and re-attaches
/// the live corpus, whose stamp still matches, so nothing bumped and a caller
/// comparing identities could not see the put was gone. A force run is the
/// same: its reset stamps the live corpus with the raised value first.
/// What: for an incremental and a force run, put during the run, roll back,
/// and assert the put is absent from the durable corpus and the stored
/// generation is above the one the put reply carried.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_rollback_that_discards_a_put_raises_the_generation() {
    for force in [false, true] {
        let id = format!("identity-rollback-{force}-10738");
        let (_dir, handle, state, _live) = staged_index(&id).await;
        if force {
            super::hash_cache::clear_persisted(&handle).await;
        }
        let tmp = begin(&handle, force).await;
        let acknowledged = put_generation(&state, &id).await;

        let rollback = super::staging::StagingResolution::Rollback {
            reason: "test".into(),
        };
        assert!(!resolve(&handle, &tmp, &rollback, force).await);
        let corpus = handle.indexer.read().await.corpus_store().expect("live");
        assert!(
            !corpus
                .list_indexed_files()
                .expect("list")
                .iter()
                .any(|f| f == PUSHED),
            "force={force}: the rollback discards the put"
        );
        let after = identity(&id).generation;
        assert!(
            after > acknowledged,
            "force={force}: a discarded put must raise the generation: {acknowledged} -> {after}"
        );
    }
}

/// Why: HIGH 1 — a #7991 deferral keeps serving staging, but the live corpus
/// a restart attaches never held the put, and its stamp still matched.
/// What: put during the run, hold the live corpus so promotion defers, and
/// assert the stored generation is above the one the put reply carried.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_deferred_promotion_raises_the_generation() {
    let id = "identity-deferred-10738";
    let (_dir, handle, state, live) = staged_index(id).await;
    let tmp = begin(&handle, false).await;
    let acknowledged = put_generation(&state, id).await;

    let _holder = crate::core::corpus::CorpusStore::open(&live).expect("hold the live corpus");
    let deferred = resolve(
        &handle,
        &tmp,
        &super::staging::StagingResolution::Commit,
        false,
    )
    .await;
    assert!(deferred, "the held live corpus must defer the promotion");
    let after = identity(id).generation;
    assert!(
        after > acknowledged,
        "a deferral must raise the generation: {acknowledged} -> {after}"
    );
}

/// Why: HIGH 1 — an incremental run copies the live corpus into staging, then
/// swaps staging in (#839). A put that commits to the live corpus between the
/// copy and the swap is missing from staging, and the promotion drops it.
/// What: hold a read guard on the indexer so the swap queues behind it after
/// the copy, write through the shared write core while it waits (the core the
/// watcher, reconcile and put all use), release, promote, and assert the put
/// is absent from the promoted corpus and the generation rose. Deterministic:
/// the write starts only once the swap is observed queued (`try_read` fails).
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_write_in_the_carryover_window_raises_the_generation() {
    let id = "identity-carryover-window-10738";
    let (_dir, handle, _state, _live) = staged_index(id).await;
    let before = identity(id);

    let guard = handle.indexer.read().await;
    let staging = {
        let handle = Arc::clone(&handle);
        tokio::spawn(async move { begin(&handle, false).await })
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while handle.indexer.try_read().is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "the swap never queued for the write lock"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    guard
        .index_file_outcome(PUSHED, PUSHED_BODY)
        .await
        .expect("a write while the swap waits");
    drop(guard);
    let tmp = staging.await.expect("begin task");

    assert!(
        !resolve(
            &handle,
            &tmp,
            &super::staging::StagingResolution::Commit,
            false
        )
        .await,
        "the promotion lands"
    );
    let corpus = handle
        .indexer
        .read()
        .await
        .corpus_store()
        .expect("promoted");
    assert!(
        !corpus
            .list_indexed_files()
            .expect("list")
            .iter()
            .any(|f| f == PUSHED),
        "the write in the window is missing from the promoted corpus"
    );
    let after = identity(id);
    assert!(
        after.generation > before.generation,
        "a write the promotion dropped must raise the generation: {before:?} -> {after:?}"
    );
}

const LATE: &str = "src/late_10738.rs";
const LATE_BODY: &str = "pub fn late_10738(x: u32) -> u32 {\n    x + 2\n}\n";

/// A colocated, walkable index under `root` with `a.rs` on disk.
///
/// Why: the restart and prune cases run real reindexes, which walk the root;
/// the corpus directory must not be walked as source.
/// Test: used by the restart and prune cases below.
fn walkable_index(id: &str, root: &std::path::Path) -> Arc<IndexHandle> {
    let embedder: Arc<dyn crate::core::embed::Embedder> =
        Arc::new(crate::core::embed::MockEmbedder::new(8));
    let store: Arc<dyn crate::core::store::VectorStore> =
        Arc::new(crate::core::store::UsearchStore::new(8).expect("usearch"));
    let colocated = root.join(crate::service::colocated_storage::COLOCATED_DIR_NAME);
    std::fs::create_dir_all(&colocated).expect("colocated dir");
    let mut indexer = CodeIndexer::new(id, root.to_path_buf())
        .with_storage_layout(crate::service::storage_layout::StorageLayout::Colocated)
        .with_components(embedder, store);
    indexer.set_corpus_store(Arc::new(
        crate::core::corpus::CorpusStore::open(
            &colocated.join(crate::service::storage_layout::REDB_FILE),
        )
        .expect("open live corpus"),
    ));
    let mut handle = IndexHandle::bare(
        IndexId::new(id),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.to_path_buf(),
    );
    handle.defer_embed = false;
    handle
        .extra_skip_dirs
        .push(crate::service::colocated_storage::COLOCATED_DIR_NAME.into());
    Arc::new(handle)
}

/// Write `path` through the shared write core, as a put does.
async fn push(handle: &Arc<IndexHandle>, path: &str, body: &str) {
    let outcome = handle
        .indexer
        .read()
        .await
        .index_file_outcome(path, body)
        .await
        .expect("push");
    assert!(
        matches!(outcome, crate::core::indexer::IndexFileOutcome::Indexed { chunks } if chunks > 0),
        "{outcome:?}"
    );
}

/// Why: HIGH 1, crash arm — puts made during a staged run live only in
/// staging. After a restart the live corpus attached with a matching stamp
/// and no marker, so nothing bumped; the next run then discarded staging, or
/// adopted it without the puts made after the restart.
/// What: for a discard and a resume, begin a staged run, push a path, drop the
/// handle and the process state as a crash does, re-attach the live corpus
/// and assert the generation rose; then push again, run the next reindex
/// (which adopts staging when `resume`), and assert it rose again.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_restart_mid_staged_reindex_raises_the_generation() {
    for resume in [false, true] {
        let id = format!("identity-restart-mid-run-{resume}-10738");
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        std::fs::write(
            root.join("a.rs"),
            "pub fn alpha_10738() -> u8 {\n    1\n}\n",
        )
        .expect("a.rs");
        let _ = index_identity::retire(&id);
        let handle = walkable_index(&id, &root);
        reindex(&handle, false).await;
        let checkpoint = resume.then(|| {
            super::checkpoint::ReindexCheckpoint::for_run(
                &handle,
                &handle.id,
                &super::validate::canonical_walk_root(&root),
            )
        });
        super::corpus_swap::begin_staged_corpus_swap(
            &handle,
            &handle.id,
            false,
            None,
            checkpoint.as_ref(),
            crate::service::storage_layout::REDB_TMP_FILE,
        )
        .await
        .expect("staging")
        .expect("staging must engage");
        push(&handle, PUSHED, PUSHED_BODY).await;
        let acknowledged = identity(&id).generation;

        // A crash: every handle and the process-local state go.
        drop(handle);
        index_identity::forget_process_state(&id);
        super::hash::file_hashes().remove(&IndexId::new(&id));
        let handle = walkable_index(&id, &root);
        let restarted = identity(&id).generation;
        assert!(
            restarted > acknowledged,
            "resume={resume}: a restart mid-run must raise the generation: \
             {acknowledged} -> {restarted}"
        );

        push(&handle, LATE, LATE_BODY).await;
        let late = identity(&id).generation;
        let progress = Arc::new(ReindexProgress::new());
        spawn_reindex_awaitable(handle.clone(), progress.clone(), false)
            .await
            .expect("reindex task must not panic");
        let events = progress.events.lock().await.clone();
        let start = events
            .iter()
            .map(|e| serde_json::from_str::<serde_json::Value>(e).expect("event json"))
            .find(|e| e["event"] == "start")
            .expect("a start event");
        assert_eq!(start["resumed"], resume, "{start}");
        let after = identity(&id).generation;
        assert!(
            after > late,
            "resume={resume}: the next run drops the push made after the restart: {late} -> {after}"
        );
    }
}

/// Why: HIGH 1, prune arm — an incremental reindex removes every path that
/// is in the corpus but not on disk, pushed paths included, and promoted with
/// a matching stamp, so the push was gone under the same identity.
/// What: reindex, push a path that is not on disk, reindex again, and assert
/// the path is gone and the generation rose.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::parallel]
async fn a_prune_that_removes_a_pushed_path_raises_the_generation() {
    let id = "identity-prune-pushed-10738";
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    std::fs::write(
        root.join("a.rs"),
        "pub fn alpha_10738() -> u8 {\n    1\n}\n",
    )
    .expect("a.rs");
    let _ = index_identity::retire(id);
    let handle = walkable_index(id, &root);
    reindex(&handle, false).await;
    push(&handle, PUSHED, PUSHED_BODY).await;
    let acknowledged = identity(id).generation;

    reindex(&handle, false).await;
    let corpus = handle.indexer.read().await.corpus_store().expect("corpus");
    assert!(
        !corpus
            .list_indexed_files()
            .expect("list")
            .iter()
            .any(|f| f == PUSHED),
        "the prune removes the pushed path"
    );
    let after = identity(id).generation;
    assert!(
        after > acknowledged,
        "a prune that removes a pushed path must raise the generation: {acknowledged} -> {after}"
    );
}
