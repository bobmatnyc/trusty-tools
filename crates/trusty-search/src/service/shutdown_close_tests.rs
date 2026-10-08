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

/// #9459: a clone released inside the budget is waited out and closed clean.
///
/// Why: a background task can hold a corpus clone for a moment at shutdown;
/// the close should wait for it rather than give up at once.
/// What: a task drops its clone after 100 ms; the close, with a 3 s budget,
/// must close the corpus and leave it openable read-only.
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
}
