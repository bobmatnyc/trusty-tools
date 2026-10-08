//! Tests for `service::shutdown_close` (#9459).
//!
//! Why: the close runs on every normal stop, so it must close what it can and
//! must never hold the stop open past its deadline.
//! What: drives `close_corpora_on_shutdown` against real redb corpora with a
//! corpus clone or an indexer lock held elsewhere.
//! Test: this module IS the tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

use super::*;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};

/// Register `id` with a fresh corpus under `dir`; returns the corpus and the
/// indexer lock so a test can hold either one.
fn register(
    registry: &IndexRegistry,
    dir: &Path,
    id: &str,
) -> (PathBuf, Arc<CorpusStore>, Arc<RwLock<CodeIndexer>>) {
    let path = dir.join(id).join("index.redb");
    let corpus = Arc::new(CorpusStore::open(&path).expect("open the corpus"));
    let mut indexer = CodeIndexer::new(id, dir);
    indexer.set_corpus_store(Arc::clone(&corpus));
    let indexer = Arc::new(RwLock::new(indexer));
    registry.register(IndexHandle::bare(
        IndexId::new(id),
        Arc::clone(&indexer),
        dir.to_path_buf(),
    ));
    (path, corpus, indexer)
}

/// True when `path` opens read-only, i.e. it was closed clean.
fn opens_read_only(path: &Path) -> bool {
    redb::ReadOnlyDatabase::open(path).is_ok()
}

/// #9459: a corpus clone or an indexer lock that outlives the deadline is
/// reported and left for repair; the close still returns on time and closes
/// every other corpus.
///
/// Why: the close runs before `process::exit(0)`. A wait with no deadline
/// would turn a stop into a hang (the fix bar's BLOCK).
/// What: three indexes — one free, one whose corpus the test still clones,
/// one whose indexer write lock the test holds — closed with a 200 ms budget
/// inside a 5 s outer timeout. Without the deadline the outer timeout fires.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_held_corpus_or_lock_cannot_stall_the_close() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = IndexRegistry::new();
    let (free, free_corpus, _) = register(&registry, dir.path(), "free");
    let (cloned, held_clone, _) = register(&registry, dir.path(), "cloned");
    let (_, locked_corpus, locked_indexer) = register(&registry, dir.path(), "locked");
    drop(free_corpus);
    drop(locked_corpus);
    let held_lock = Arc::clone(&locked_indexer).write_owned().await;
    let state = SearchAppState::new(registry);

    let budget = Duration::from_millis(200);
    let started = Instant::now();
    let report = tokio::time::timeout(
        Duration::from_secs(5),
        close_corpora_on_shutdown(&state, budget),
    )
    .await
    .expect("the close must return at its deadline, not wait for the holders");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "the close took {elapsed:?} on a {budget:?} budget"
    );
    assert_eq!(report.closed, 1, "the free corpus closes: {report:?}");
    assert_eq!(report.unclean.len(), 2, "{report:?}");
    assert!(
        report
            .unclean
            .iter()
            .any(|l| l.starts_with("cloned:") && l.contains("other reference")),
        "{report:?}"
    );
    assert!(
        report
            .unclean
            .iter()
            .any(|l| l.starts_with("locked:") && l.contains("indexer lock")),
        "{report:?}"
    );
    assert!(opens_read_only(&free), "the free corpus closed clean");
    assert!(
        !opens_read_only(&cloned),
        "the held corpus stays open for the next start to repair"
    );
    drop(held_clone);
    drop(held_lock);
}

/// #9459: an index whose reindex permit is held keeps its corpus attached.
///
/// Why: a reindex that loses its corpus mid-run fails open — its last batches
/// and prunes are dropped while `finish_reindex` still writes the HEAD-SHA
/// marker, so the next boot does no catch-up. The close must leave such an
/// index alone, as the shutdown flush does (#1717).
/// What: holds the index's `index_semaphore` permit as a running reindex
/// does, runs the close, and asserts the corpus is still attached and the
/// index is reported unclean, naming the held permit.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_corpus_whose_reindex_is_running_stays_attached() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = IndexRegistry::new();
    let id = "reindexing-9459";
    let (_, corpus, indexer) = register(&registry, dir.path(), id);
    drop(corpus);
    let reindex_permit = crate::service::reindex::index_semaphore(&IndexId::new(id))
        .try_acquire_owned()
        .expect("no other task holds this test's permit");
    let state = SearchAppState::new(registry);

    let report = close_corpora_on_shutdown(&state, Duration::from_millis(200)).await;

    assert!(
        indexer.read().await.has_corpus_store(),
        "the running reindex keeps its corpus: {report:?}"
    );
    assert_eq!(report.closed, 0, "{report:?}");
    assert!(
        report
            .unclean
            .iter()
            .any(|l| l.starts_with("reindexing-9459:") && l.contains("reindex")),
        "{report:?}"
    );
    drop(reindex_permit);
}

/// #9459: a clone released inside the budget is waited out and closed clean.
///
/// Why: a background task can hold a corpus clone for a moment at shutdown;
/// the close should wait for it rather than give up at once.
/// What: a task drops its clone after 100 ms; the close, with a 3 s budget,
/// must close the corpus and leave it openable read-only. The report holds
/// the index permit until it is dropped.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_clone_released_inside_the_budget_is_closed_cleanly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = IndexRegistry::new();
    let (path, clone, _) = register(&registry, dir.path(), "transient");
    let state = SearchAppState::new(registry);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(clone);
    });

    let report = close_corpora_on_shutdown(&state, SHUTDOWN_CORPUS_CLOSE_BUDGET).await;

    assert_eq!(report.closed, 1, "{report:?}");
    assert!(report.unclean.is_empty(), "{report:?}");
    assert!(opens_read_only(&path), "the corpus closed clean");
    // #9459: no reindex, relocate or embed pass starts while the report lives.
    let permit = crate::service::reindex::index_semaphore(&IndexId::new("transient"));
    assert!(
        permit.try_acquire().is_err(),
        "the live report holds the permit"
    );
    drop(report);
    assert!(
        permit.try_acquire().is_ok(),
        "dropping the report releases it"
    );
}

/// #9459: a reconcile delta that runs after the close fails and leaves the
/// HEAD SHA unstamped.
///
/// Why: the boot and cold-load reconcile delta runs detached and takes no
/// index permit, so it can write after the close took the corpus. Those
/// writes landed in memory only and answered `Ok`, so `apply_delta` stamped
/// the new SHA and the next boot never retried the delta.
/// What: indexes two files into a real corpus, runs the close, then edits one
/// file, deletes the other and runs `apply_delta` over both. The delta must
/// answer `false` and leave `indexed_head_sha` unchanged; `stamp_handle`
/// writes that field in the same call that persists the SHA to
/// `indexes.toml`. Both write entry points then refuse directly.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn a_reconcile_delta_after_the_close_fails_and_does_not_stamp() {
    let tree = tempfile::tempdir().expect("tree");
    let root = tree.path().canonicalize().expect("canonical root");
    std::fs::create_dir_all(root.join("src")).expect("src");
    std::fs::write(root.join("src/kept.rs"), "pub fn kept_9459() {}\n").expect("kept");
    std::fs::write(root.join("src/gone.rs"), "pub fn gone_9459() {}\n").expect("gone");
    let store = tempfile::tempdir().expect("store");
    let id = IndexId::new("reconcile-after-close-9459");
    let mut indexer = CodeIndexer::new(id.0.as_str(), root.as_path());
    let corpus = CorpusStore::open(&store.path().join("index.redb")).expect("open the corpus");
    indexer.set_corpus_store(Arc::new(corpus));
    let indexer = Arc::new(RwLock::new(indexer));
    for rel in ["src/kept.rs", "src/gone.rs"] {
        let content = std::fs::read_to_string(root.join(rel)).expect("read");
        let idx = indexer.read().await;
        idx.index_file(rel, &content).await.expect("seed write");
    }
    let registry = IndexRegistry::new();
    registry.register(IndexHandle::bare(
        id.clone(),
        Arc::clone(&indexer),
        root.clone(),
    ));
    let handle = registry.get(&id).expect("registered");
    *handle.indexed_head_sha.write().await = Some("old-sha".into());
    let state = SearchAppState::new(registry);

    let report = close_corpora_on_shutdown(&state, SHUTDOWN_CORPUS_CLOSE_BUDGET).await;
    assert_eq!(report.closed, 1, "{report:?}");

    std::fs::write(root.join("src/kept.rs"), "pub fn kept_9459_v2() {}\n").expect("edit");
    std::fs::remove_file(root.join("src/gone.rs")).expect("delete");
    let delta = vec!["src/kept.rs".to_string(), "src/gone.rs".to_string()];
    let stamped = crate::service::reconcile::apply_delta(&handle, &id.0, &delta, "new-sha").await;

    assert!(
        !stamped,
        "a delta against a closed corpus must report failure"
    );
    assert_eq!(
        handle.indexed_head_sha.read().await.as_deref(),
        Some("old-sha"),
        "a delta whose writes persisted nothing must not stamp the SHA"
    );
    let idx = indexer.read().await;
    let write = idx.index_file("src/kept.rs", "pub fn x() {}\n").await;
    assert!(
        write.is_err_and(|e| e.to_string().contains("closed for shutdown")),
        "index_file must refuse"
    );
    let purge = idx.purge_file_committed(&id, "src/gone.rs").await;
    assert!(
        purge.is_err_and(|e| e.to_string().contains("closed for shutdown")),
        "purge_file_committed must refuse"
    );
    drop(idx);
    drop(report);
}
