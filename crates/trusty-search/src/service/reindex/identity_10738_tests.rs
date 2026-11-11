//! #10738: a force reindex, which rebuilds from an empty corpus, raises the
//! index generation; an incremental reindex does not.

use std::sync::Arc;

use super::corpus_swap::detach_hook::{self, Parked, Window};
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
    Arc::new(walkable_handle(id, root))
}

/// [`walkable_index`] before it is shared, so a test can register it.
fn walkable_handle(id: &str, root: &std::path::Path) -> IndexHandle {
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
    handle
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

/// Settle a staged run on its own task, so a test can act while it is parked.
fn resolve_spawned(
    handle: &Arc<IndexHandle>,
    tmp: std::path::PathBuf,
    commit: bool,
) -> tokio::task::JoinHandle<bool> {
    let handle = Arc::clone(handle);
    tokio::spawn(async move {
        let resolution = if commit {
            super::staging::StagingResolution::Commit
        } else {
            super::staging::StagingResolution::Rollback {
                reason: "test".into(),
            }
        };
        resolve(&handle, &tmp, &resolution, false).await
    })
}

/// Wait until the parked run is inside its detach window.
async fn reach(reached: tokio::sync::oneshot::Receiver<()>) {
    tokio::time::timeout(std::time::Duration::from_secs(60), reached)
        .await
        .expect("the run reaches its detach window")
        .expect("the park stays registered until reached");
}

/// A put through the real put core, the error body on a refusal.
async fn try_put(
    state: &Arc<crate::service::server::SearchAppState>,
    id: &str,
    path: &str,
    body: &str,
) -> Result<serde_json::Value, serde_json::Value> {
    crate::service::server::index_file_report(
        state,
        id,
        crate::service::server::IndexFileRequest {
            path: path.into(),
            content: body.into(),
        },
    )
    .await
    .map_err(|(_, body)| body)
}

/// Whether the attached corpus holds `path`.
async fn corpus_holds(handle: &Arc<IndexHandle>, path: &str) -> bool {
    let corpus = handle
        .indexer
        .read()
        .await
        .corpus_store()
        .expect("attached");
    corpus
        .list_indexed_files()
        .expect("list")
        .iter()
        .any(|f| f == path)
}

/// Why: R1 — the abort takes the staging store, then deletes it and reopens
/// live with no lock held. A put in that span wrote to memory only and was
/// acknowledged, and the live corpus re-attached without it.
/// What: park the abort after the take, put, release, and assert the put was
/// refused or is in the re-attached live corpus; a put after the re-attach
/// lands. Fails before the fix: the put answers `indexed: true` and live
/// lacks it.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_put_during_the_abort_detach_window_is_refused_or_lands_in_live() {
    let id = "identity-abort-window-10738";
    let (_dir, handle, state, _live) = staged_index(id).await;
    let tmp = begin(&handle, false).await;
    let Parked { reached, release } = detach_hook::park(id, Window::Abort);
    let abort = resolve_spawned(&handle, tmp, false);
    reach(reached).await;
    assert!(
        !handle.indexer.read().await.has_corpus_store(),
        "test setup: the abort detached the corpus"
    );

    let reply = try_put(&state, id, PUSHED, PUSHED_BODY).await;
    release.send(()).expect("the abort is still parked");
    assert!(!abort.await.expect("abort task"), "a rollback never defers");
    match reply {
        Err(body) => assert_eq!(body["indexed"], false, "{body}"),
        Ok(body) => assert!(
            corpus_holds(&handle, PUSHED).await,
            "an acknowledged put must be in the re-attached live corpus: {body}"
        ),
    }

    let after = try_put(&state, id, LATE, LATE_BODY)
        .await
        .expect("a put after the re-attach is accepted");
    assert_eq!(after["indexed"], true, "{after}");
    assert!(corpus_holds(&handle, LATE).await, "it lands in live");
}

/// Why: R1 — the promotion releases staging, renames it over live and
/// reopens with no lock held. A put in that span wrote to memory only, and
/// the promoted corpus carried a matching stamp, so nothing bumped.
/// What: park the promotion after the release, put, release, and assert the
/// put was refused or the generation rose past the one its reply carried; a
/// put after the re-attach lands. Fails before the fix: the put answers
/// `indexed: true` and the generation stays.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_put_during_the_promotion_detach_window_is_refused_or_raises_the_generation() {
    let id = "identity-promotion-window-10738";
    let (_dir, handle, state, _live) = staged_index(id).await;
    let tmp = begin(&handle, false).await;
    let Parked { reached, release } = detach_hook::park(id, Window::Promotion);
    let commit = resolve_spawned(&handle, tmp, true);
    reach(reached).await;
    assert!(
        !handle.indexer.read().await.has_corpus_store(),
        "test setup: the promotion released the corpus"
    );

    let reply = try_put(&state, id, PUSHED, PUSHED_BODY).await;
    release.send(()).expect("the promotion is still parked");
    assert!(!commit.await.expect("commit task"), "the promotion lands");
    match reply {
        Err(body) => assert_eq!(body["indexed"], false, "{body}"),
        Ok(body) => {
            let acknowledged = body["identity"]["generation"]
                .as_u64()
                .unwrap_or_else(|| panic!("put reply carries a generation: {body}"));
            let after = identity(id).generation;
            assert!(
                after > acknowledged,
                "a put the promotion dropped must raise the generation: {acknowledged} -> {after}"
            );
        }
    }

    let after = try_put(&state, id, LATE, LATE_BODY)
        .await
        .expect("a put after the promotion is accepted");
    assert_eq!(after["indexed"], true, "{after}");
    assert!(
        corpus_holds(&handle, LATE).await,
        "it lands in the promoted corpus"
    );
}

/// Why: R1 with the abort's `Err` arm — the live reopen fails, so a put in
/// the detach window can land nowhere durable, and the quarantine bump
/// (`quarantine_detached_corpus`) had no test.
/// What: hold the live corpus so the reopen fails, park the abort after the
/// take, put, release, and assert the put was refused, the index is
/// quarantined with no corpus, the generation rose, and a later put is
/// refused. Fails before the fix: the window put answers `indexed: true`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn an_abort_whose_live_reopen_fails_quarantines_and_raises_the_generation() {
    let id = "identity-abort-reopen-fails-10738";
    let (_dir, handle, state, live) = staged_index(id).await;
    let tmp = begin(&handle, false).await;
    let before = identity(id).generation;
    let _holder = crate::core::corpus::CorpusStore::open(&live).expect("hold the live corpus");
    let Parked { reached, release } = detach_hook::park(id, Window::Abort);
    let abort = resolve_spawned(&handle, tmp, false);
    reach(reached).await;

    let reply = try_put(&state, id, PUSHED, PUSHED_BODY).await;
    release.send(()).expect("the abort is still parked");
    assert!(!abort.await.expect("abort task"), "a rollback never defers");
    let body = reply.expect_err("a put while the corpus is detached must be refused");
    assert_eq!(body["indexed"], false, "{body}");

    {
        let indexer = handle.indexer.read().await;
        assert!(
            indexer.is_write_quarantined(),
            "the failed reopen quarantines"
        );
        assert!(!indexer.has_corpus_store(), "no corpus is attached");
    }
    let after = identity(id).generation;
    assert!(
        after > before,
        "a quarantine must raise the generation: {before} -> {after}"
    );
    let later = try_put(&state, id, LATE, LATE_BODY).await;
    assert!(
        later.is_err(),
        "a quarantined index refuses puts: {later:?}"
    );
}

/// Why: R1/R2 — a run that panics or is cancelled inside the abort's detach
/// window never re-attaches and never quarantines, so every later put was
/// acknowledged into memory only until a restart.
/// What: put during the run, park the abort after the take, drop the release
/// so the run panics there, and assert a later put is refused; then attach
/// the live corpus as a restart does and assert the generation rose past the
/// first put's reply and a put lands. Fails before the fix: the put after the
/// panic answers `indexed: true`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_panicked_staged_run_is_covered_by_the_next_attach() {
    let id = "identity-panicked-run-10738";
    let (_dir, handle, state, live) = staged_index(id).await;
    let tmp = begin(&handle, false).await;
    let acknowledged = put_generation(&state, id).await;
    let Parked { reached, release } = detach_hook::park(id, Window::Abort);
    let abort = resolve_spawned(&handle, tmp, false);
    reach(reached).await;
    drop(release);
    let died = abort.await.expect_err("the run dies in the detach window");
    assert!(died.is_panic(), "{died}");

    let reply = try_put(&state, id, LATE, LATE_BODY).await;
    let body = reply.expect_err("a put after the run died detached must be refused");
    assert_eq!(body["indexed"], false, "{body}");

    let store = crate::core::corpus::CorpusStore::open(&live).expect("reopen live");
    handle
        .indexer
        .write()
        .await
        .set_corpus_store(Arc::new(store));
    let attached = identity(id).generation;
    assert!(
        attached > acknowledged,
        "the next attach must raise the generation: {acknowledged} -> {attached}"
    );
    let late = try_put(&state, id, LATE, LATE_BODY)
        .await
        .expect("a put after the attach is accepted");
    assert_eq!(late["indexed"], true, "{late}");
}

/// Why: R1, delete arm — a remove in the abort's detach window dropped the
/// file from memory only and was acknowledged, and the live corpus
/// re-attached with the file still in it, so the file came back.
/// What: for `remove-file` and the batch `remove-files`, park the abort after
/// the take, delete the seeded `a.rs`, release, and assert the delete was
/// refused with the in-memory chunks untouched, or the file is gone from the
/// re-attached live corpus; a delete after the re-attach lands. Fails before
/// the fix: the delete is acknowledged and live still holds `a.rs`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_delete_during_the_abort_detach_window_is_refused_or_stays_deleted() {
    async fn delete(
        state: &Arc<crate::service::server::SearchAppState>,
        id: &str,
        batch: bool,
    ) -> Result<serde_json::Value, serde_json::Value> {
        let reply = if batch {
            crate::service::server::remove_files_report(
                state,
                id,
                crate::service::server::RemoveFilesRequest {
                    path_prefix: None,
                    paths: Some(vec!["a.rs".into()]),
                    dry_run: false,
                },
            )
            .await
        } else {
            crate::service::server::remove_file_report(
                state,
                id,
                crate::service::server::RemoveFileRequest {
                    path: "a.rs".into(),
                },
            )
            .await
        };
        reply.map_err(|(_, body)| body)
    }

    for batch in [false, true] {
        let id = format!("identity-abort-delete-{batch}-10738");
        let (_dir, handle, state, _live) = staged_index(&id).await;
        let tmp = begin(&handle, false).await;
        let Parked { reached, release } = detach_hook::park(&id, Window::Abort);
        let abort = resolve_spawned(&handle, tmp, false);
        reach(reached).await;
        let chunks_before = handle.indexer.read().await.chunk_count();
        assert!(chunks_before > 0, "test setup: a.rs is in memory");

        let reply = delete(&state, &id, batch).await;
        let chunks_after = handle.indexer.read().await.chunk_count();
        release.send(()).expect("the abort is still parked");
        assert!(!abort.await.expect("abort task"), "a rollback never defers");
        match reply {
            Err(body) => assert_eq!(
                chunks_after, chunks_before,
                "batch={batch}: a refused delete changes nothing: {body}"
            ),
            Ok(body) => assert!(
                !corpus_holds(&handle, "a.rs").await,
                "batch={batch}: an acknowledged delete must not come back after the \
                 re-attach: {body}"
            ),
        }

        delete(&state, &id, batch)
            .await
            .unwrap_or_else(|body| panic!("batch={batch}: a delete after the re-attach: {body}"));
        assert!(
            !corpus_holds(&handle, "a.rs").await,
            "batch={batch}: it leaves live"
        );
    }
}

/// Why: R1, watcher delete arm — the watcher's removal
/// (`remove_chunk_ids_committed`) in the abort's detach window dropped the
/// file from memory only and reported success, and the live corpus
/// re-attached with the file still in it, so the file came back.
/// What: track `a.rs` as the watcher does, park the abort after the take,
/// deliver the watcher's removal, release, and assert the removal changed
/// nothing in memory and kept the ids tracked for a retry, or the file is
/// gone from the re-attached live corpus; the retried removal after the
/// re-attach lands. Fails before the fix: memory drops `a.rs` and live
/// still holds it.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_watcher_delete_during_the_abort_detach_window_does_not_resurrect() {
    let id = "identity-abort-watcher-delete-10738";
    let (_dir, handle, _state, _live) = staged_index(id).await;
    let root = handle.root_path.clone();
    let ids = handle
        .indexer
        .read()
        .await
        .file_chunk_ids("a.rs")
        .await
        .expect("a.rs chunk ids");
    assert!(!ids.is_empty(), "test setup: a.rs has chunks");
    let tracked = crate::service::indexed_files::IndexedFiles::new();
    tracked.record(std::path::PathBuf::from("a.rs"), ids).await;
    let removed_path = root.join("a.rs");
    let watcher_remove = || {
        crate::service::watch_loop::handle_removed(
            &removed_path,
            &handle.id,
            &root,
            &root,
            &handle.indexer,
            &tracked,
        )
    };

    let tmp = begin(&handle, false).await;
    let Parked { reached, release } = detach_hook::park(id, Window::Abort);
    let abort = resolve_spawned(&handle, tmp, false);
    reach(reached).await;
    let chunks_before = handle.indexer.read().await.chunk_count();
    watcher_remove().await;
    let chunks_after = handle.indexer.read().await.chunk_count();
    release.send(()).expect("the abort is still parked");
    assert!(!abort.await.expect("abort task"), "a rollback never defers");
    if chunks_after == chunks_before {
        assert!(
            tracked.len().await == 1,
            "a refused watcher removal keeps a.rs tracked for a retry"
        );
    } else {
        assert!(
            !corpus_holds(&handle, "a.rs").await,
            "a watcher removal that dropped memory must not come back after the \
             re-attach ({chunks_before} -> {chunks_after} chunks in memory)"
        );
    }

    watcher_remove().await;
    assert!(
        !corpus_holds(&handle, "a.rs").await,
        "the retried watcher removal leaves live"
    );
}

/// Why: R1, watcher rescan arm — the overflow rescan commits through
/// `index_files_batch_no_rebuild`. In the abort's detach window that batch
/// landed in memory only and returned `Ok`, the rescan recorded the new
/// fingerprint, and the re-attached live corpus kept the old content, so the
/// next rescan skipped the file as unchanged and the index stayed stale.
/// What: change `a.rs` on disk, park the abort after the take, run the
/// rescan reconcile, release, and assert the pass failed without recording
/// the new fingerprint, or the new content is in the re-attached live
/// corpus. Fails before the fix: the pass succeeds and live holds the old
/// content.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_rescan_batch_during_the_abort_detach_window_is_refused_or_lands_in_live() {
    let id = "identity-abort-rescan-10738";
    let (_dir, handle, _state, _live) = staged_index(id).await;
    let root = handle.root_path.clone();
    let new_body = "pub fn alpha_rescanned_10738() -> u8 {\n    2\n}\n";
    std::fs::write(root.join("a.rs"), new_body).expect("a.rs on disk");
    let fingerprint = super::hash::hash_content(new_body);
    let tracked = crate::service::indexed_files::IndexedFiles::new();

    let tmp = begin(&handle, false).await;
    let Parked { reached, release } = detach_hook::park(id, Window::Abort);
    let abort = resolve_spawned(&handle, tmp, false);
    reach(reached).await;
    let outcome = crate::service::watch_rescan::reconcile_after_rescan(
        &handle.id,
        &root,
        &root,
        &handle.indexer,
        &tracked,
    )
    .await;
    let recorded = super::hash::hashes_for(&handle.id)
        .get(std::path::Path::new("a.rs"))
        .map(|h| h.value().clone());
    release.send(()).expect("the abort is still parked");
    assert!(!abort.await.expect("abort task"), "a rollback never defers");

    match outcome {
        Err(e) => assert_ne!(
            recorded.as_deref(),
            Some(fingerprint.as_str()),
            "a refused rescan batch must not record the new fingerprint: {e}"
        ),
        Ok(stats) => {
            let corpus = handle
                .indexer
                .read()
                .await
                .corpus_store()
                .expect("attached");
            let live_a: Vec<String> = corpus
                .load_all_chunks()
                .expect("load")
                .into_iter()
                .filter(|c| c.file == "a.rs")
                .map(|c| c.content)
                .collect();
            assert!(
                live_a.iter().any(|c| c.contains("alpha_rescanned_10738")),
                "a rescan that reported success must leave the new content in the \
                 re-attached live corpus: {stats:?}, live a.rs = {live_a:?}"
            );
        }
    }
}

/// A registered, walkable index whose staged run was dropped mid-batch.
struct DroppedRun {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    handle: Arc<IndexHandle>,
    state: Arc<crate::service::server::SearchAppState>,
    live: std::path::PathBuf,
    staging_probe: crate::core::store::ReindexProbe,
    /// The generation the put taken into staging was acknowledged under.
    acknowledged: u64,
}

/// Index `a.rs`, start a staged run that parks its `b.rs` batch on the
/// embedding pause, put, run `before_drop`, cancel the run, and resume.
///
/// Why: the dropped-run cases share this setup and differ only in what they
/// arrange just before the drop.
/// Test: used by the dropped-run cases below.
async fn drop_a_staged_run<T>(id: &str, before_drop: impl FnOnce() -> T) -> (DroppedRun, T) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    std::fs::write(
        root.join("a.rs"),
        "pub fn alpha_10738() -> u8 {\n    1\n}\n",
    )
    .expect("a.rs");
    let _ = index_identity::retire(id);
    let registry = crate::core::registry::IndexRegistry::new();
    let handle = registry.register(walkable_handle(id, &root));
    let state = Arc::new(crate::service::server::SearchAppState::new(registry));
    state
        .install_embedder(Arc::new(crate::core::embed::MockEmbedder::new(8)))
        .await;
    reindex(&handle, false).await;
    let live = root
        .join(crate::service::colocated_storage::COLOCATED_DIR_NAME)
        .join(crate::service::storage_layout::REDB_FILE);

    // The next run stages, then parks its batch for `b.rs` on the pause.
    std::fs::write(root.join("b.rs"), "pub fn beta_10738() -> u8 {\n    2\n}\n").expect("b.rs");
    let staging_probe = handle.indexer.read().await.reindex_staging_probe();
    handle.embedding_pause.pause();
    let run = spawn_reindex_awaitable(handle.clone(), Arc::new(ReindexProgress::new()), false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !staging_probe() {
        assert!(
            std::time::Instant::now() < deadline,
            "the run never reached its batches"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let staged = handle
        .indexer
        .read()
        .await
        .corpus_store()
        .expect("staging attached");
    assert_ne!(staged.path(), live, "test setup: staging is attached");
    drop(staged);
    let acknowledged = put_generation(&state, id).await;

    let arranged = before_drop();
    run.abort();
    let died = run.await.expect_err("the run is dropped mid-batch");
    assert!(died.is_cancelled(), "{died}");
    handle.embedding_pause.resume();
    let run = DroppedRun {
        _dir: dir,
        root,
        handle,
        state,
        live,
        staging_probe,
        acknowledged,
    };
    (run, arranged)
}

/// Wait until the dropped run's rollback has returned and released the
/// index permit it held.
async fn rollback_settled(permits: &Arc<tokio::sync::Semaphore>) {
    let _permit = tokio::time::timeout(std::time::Duration::from_secs(60), permits.acquire())
        .await
        .expect("the rollback returns")
        .expect("the per-index semaphore is never closed");
}

/// Why: R2 — a staged run that panics or is cancelled mid-batch, outside the
/// detach windows, left staging attached. The termination guard neither
/// re-attached live nor raised the generation, so later puts were
/// acknowledged into a staging corpus the next attach discards, under the
/// generation the earlier replies carried.
/// What: park the run's batch on the embedding pause with staging attached,
/// put, cancel the task, then put again and assert that put was refused or
/// its reply carries a generation above the first put's. Then wait for a put
/// to be accepted and assert it is in the attached live corpus, the first put
/// (staging only) is not, the generation rose, and HNSW staging ended. Fails
/// before the fix: the put after the cancel answers `indexed: true` at the
/// first put's generation.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_staged_run_dropped_mid_batch_refuses_or_rolls_back() {
    let id = "identity-midbatch-drop-10738";
    let (run, ()) = drop_a_staged_run(id, || ()).await;
    let DroppedRun {
        handle,
        state,
        live,
        staging_probe,
        acknowledged,
        ..
    } = &run;
    let (handle, state, acknowledged) = (handle, state, *acknowledged);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);

    match try_put(state, id, LATE, LATE_BODY).await {
        Err(body) => assert_eq!(body["indexed"], false, "{body}"),
        Ok(body) => {
            let generation = body["identity"]["generation"]
                .as_u64()
                .unwrap_or_else(|| panic!("put reply carries a generation: {body}"));
            assert!(
                generation > acknowledged,
                "a put acknowledged after the run died must carry a raised generation, \
                 or be refused: {acknowledged} -> {generation}, {body}"
            );
        }
    }

    // The rollback settles: a put is accepted again, into live.
    let accepted = loop {
        if let Ok(body) = try_put(state, id, LATE, LATE_BODY).await {
            break body;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the index never settled after the run died"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!(accepted["indexed"], true, "{accepted}");
    let generation = accepted["identity"]["generation"]
        .as_u64()
        .unwrap_or_else(|| panic!("put reply carries a generation: {accepted}"));
    assert!(
        generation > acknowledged,
        "the rollback raises the generation: {acknowledged} -> {generation}"
    );
    let attached = handle
        .indexer
        .read()
        .await
        .corpus_store()
        .expect("attached");
    assert_eq!(
        std::fs::canonicalize(attached.path()).ok(),
        std::fs::canonicalize(live).ok(),
        "live is re-attached"
    );
    drop(attached);
    assert!(corpus_holds(handle, LATE).await, "the later put is in live");
    assert!(
        !corpus_holds(handle, PUSHED).await,
        "the put taken into staging is discarded"
    );
    assert!(!staging_probe(), "HNSW staging ended with the rollback");
}

/// The identity file a bump or a mint writes for `id`.
fn identity_file(id: &str) -> std::path::PathBuf {
    let name = crate::service::persistence::sanitize_id_for_path(id);
    crate::service::persistence::data_dir()
        .expect("data dir")
        .join("index-identity")
        .join(format!("{name}.json"))
}

/// Why: R2 review, HIGH — a delete takes only the teardown lock's exclusive
/// side. The dropped run released its shared side as it died, so a delete
/// could run while the guard's rollback was in flight; the rollback then
/// reopened live, which recreated the deleted data directory, and bumped the
/// retired identity back into existence.
/// What: drop a staged run, park its rollback inside the abort, start a
/// delete, give it a window, release, and assert the delete did not finish
/// inside the rollback and that, once it lands, neither the data directory
/// nor an identity file is left. Fails before the fix: the delete finishes
/// at once and the rollback recreates both.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::parallel]
async fn a_delete_during_a_dropped_runs_rollback_leaves_no_data_or_identity() {
    use crate::service::server::{delete_index_report, DeleteIndexParams};
    let id = "identity-midbatch-delete-10738";
    let permits = super::semaphore::index_semaphore(&IndexId::new(id));
    let (run, Parked { reached, release }) =
        drop_a_staged_run(id, || detach_hook::park(id, Window::Abort)).await;
    reach(reached).await;

    let delete = |state: Arc<crate::service::server::SearchAppState>| {
        tokio::spawn(async move {
            let params = DeleteIndexParams {
                delete_data: true,
                expected_root_path: None,
            };
            delete_index_report(&state, id, params)
                .await
                .unwrap_or_else(|(_, body)| body)
        })
    };
    let mut first = delete(Arc::clone(&run.state));
    let early = tokio::time::timeout(std::time::Duration::from_millis(500), &mut first).await;
    let ran_early = early.is_ok();
    release.send(()).expect("the rollback is still parked");
    rollback_settled(&permits).await;
    let mut outcome = match early {
        Ok(joined) => joined,
        Err(_) => first.await,
    }
    .expect("delete task");
    // A delete whose quiesce wait timed out changed nothing; re-issue it.
    if outcome["removed"] != true {
        outcome = delete(Arc::clone(&run.state)).await.expect("delete task");
    }
    assert_eq!(outcome["removed"], true, "{outcome}");

    let data = run
        .root
        .join(crate::service::colocated_storage::COLOCATED_DIR_NAME);
    assert!(
        !data.exists(),
        "a delete must not leave the data directory behind: {}",
        data.display()
    );
    assert!(
        !identity_file(id).exists(),
        "a deleted index must not keep an identity file"
    );
    assert!(
        !ran_early,
        "the delete ran while the dropped run's rollback was in flight"
    );
}

/// Makes a directory read-only; restores it on drop.
struct ReadOnly(std::path::PathBuf);
impl Drop for ReadOnly {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// Why: R2 review, MEDIUM (Fail-Open) — the guard's rollback cleared the
/// refusal flag even when its generation bump failed, so puts resumed with
/// no raised generation for the puts the rollback discarded.
/// What: make the identity directory read-only just before the drop, wait
/// for the rollback to return, and assert the bump failed and a put is still
/// refused. Fails before the fix: the put answers `indexed: true`.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
async fn a_dropped_runs_rollback_whose_bump_fails_keeps_refusing_puts() {
    use std::os::unix::fs::PermissionsExt;
    // Root ignores directory permissions, so the bump would not fail.
    if nix::unistd::geteuid().is_root() {
        return;
    }
    let _isolated = crate::service::server::tests_components::IsolatedDataDir::new();
    let id = "identity-midbatch-bump-fails-10738";
    let permits = super::semaphore::index_semaphore(&IndexId::new(id));
    let (run, _read_only) = drop_a_staged_run(id, || {
        let dir = identity_file(id)
            .parent()
            .expect("identity dir")
            .to_path_buf();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555))
            .expect("make the identity dir read-only");
        ReadOnly(dir)
    })
    .await;
    rollback_settled(&permits).await;
    assert!(
        index_identity::current(id).is_err(),
        "test setup: the rollback's bump failed"
    );

    let reply = try_put(&run.state, id, LATE, LATE_BODY).await;
    assert!(
        reply.is_err(),
        "a rollback whose bump failed must keep refusing puts: {reply:?}"
    );
}
