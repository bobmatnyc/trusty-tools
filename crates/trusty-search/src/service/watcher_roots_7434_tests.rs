//! One watch per index root (#7434).
//!
//! Why: a watcher keyed per index watched the primary root only, so an
//! additional root went stale between reindexes behind `watcher.active: true`.
//! What: drives the manager over a two-root handle: both roots watched and
//! reported, a stuck start on one root (#9339) leaving the other watching, and
//! an additional root's saves landing on its `@root1/…` chunks.
//! Test: this file.

use super::*;
use crate::core::CodeIndexer;
use std::path::{Path, PathBuf};
use tokio::sync::RwLock;

fn two_root_handle(id: &str, primary: &Path, extra: &Path) -> Arc<IndexHandle> {
    let mut indexer = CodeIndexer::new(id, primary);
    indexer.set_additional_roots(vec![extra.to_path_buf()]);
    let mut handle = IndexHandle::bare(
        IndexId::new(id),
        Arc::new(RwLock::new(indexer)),
        primary.to_path_buf(),
    );
    handle.additional_roots = vec![extra.to_path_buf()];
    Arc::new(handle)
}

fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().canonicalize().expect("canonical");
    (dir, path)
}

/// Why: the manager must hold a live watch on EVERY root, report each one,
/// and a save under the additional root must reach that root's chunks.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_index_root_is_watched() {
    let (_p, primary) = canonical_tempdir();
    let (_e, extra) = canonical_tempdir();
    let handle = two_root_handle("every-root-7434", &primary, &extra);
    let mgr = WatcherManager::new();
    mgr.spawn_for_index(&handle).await;

    let states = mgr.root_watch_states(&handle.id).await;
    let labels: Vec<(&str, bool)> = states.iter().map(|r| (r.state, r.primary)).collect();
    assert_eq!(
        labels,
        vec![("watching", true), ("watching", false)],
        "{states:?}"
    );
    assert!(!mgr.needs_root_resync(&handle).await);

    let file = extra.join("saved.rs");
    let probe = Arc::clone(&handle);
    let indexed = crate::service::watch_test_support::await_watch_condition(
        |generation| {
            std::fs::write(&file, format!("fn saved_{generation}() {{}}\n")).expect("write");
        },
        move || {
            let probe = Arc::clone(&probe);
            async move {
                let idx = probe.indexer.read().await;
                !idx.chunk_ids_for_file("@root1/saved.rs").await.is_empty()
            }
        },
    )
    .await;
    assert!(
        indexed,
        "a save under the additional root must land on @root1/saved.rs"
    );

    assert!(mgr.stop_for_index(&handle.id).await);
    assert!(mgr.root_watch_states(&handle.id).await.is_empty());
}

/// Why (#9339 × #7434): one root whose OS-watch start never answers must not
/// hang the index's start, and must not cost the other roots their watch. It
/// is recorded `failed`, and the next resync retries it.
/// What: the additional root's start is held past `WATCHER_START_BOUND`; the
/// spawn still returns within the bound plus slack, the primary watches, the
/// held root reports `failed`. Released, a resync watches it.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stuck_root_start_does_not_block_the_other_roots() {
    let (_p, primary) = canonical_tempdir();
    let (_e, extra) = canonical_tempdir();
    let handle = two_root_handle("stuck-root-7434", &primary, &extra);
    let mgr = WatcherManager::new();

    let release = crate::service::watcher_start::hold_next_start_for(&extra);
    let started = std::time::Instant::now();
    let bound =
        crate::service::watcher_start::WATCHER_START_BOUND + std::time::Duration::from_secs(5);
    let spawned = tokio::time::timeout(bound, mgr.spawn_for_index(&handle)).await;
    let states = mgr.root_watch_states(&handle.id).await;
    drop(release);

    assert!(spawned.is_ok(), "spawn hung for {:?}", started.elapsed());
    let primary_state = states.iter().find(|r| r.primary).map(|r| r.state);
    let extra_state = states.iter().find(|r| !r.primary).map(|r| r.state);
    assert_eq!(primary_state, Some("watching"), "{states:?}");
    assert_eq!(extra_state, Some("failed"), "{states:?}");
    assert!(mgr.is_watching(&handle.id).await);
    assert!(
        mgr.needs_root_resync(&handle).await,
        "a failed root is retried"
    );

    // The detached start thread reopens the root once it ends.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while crate::service::watcher_start::starts_in_flight_for(&extra) > 0
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    mgr.spawn_for_index(&handle).await;
    let states = mgr.root_watch_states(&handle.id).await;
    assert!(states.iter().all(|r| r.state == "watching"), "{states:?}");
    mgr.stop_all().await;
}

/// Why: the corpus key is what decides which chunks a save replaces; an
/// additional root's save keyed bare would replace a same-named primary file.
/// What: the same relative file under both roots, saved through each root's
/// `WatchedRoot`; each keeps its own chunks, and the additional one's delete
/// leaves the primary's alone.
/// Test: this test.
#[tokio::test]
async fn modified_file_under_additional_root_updates_its_root_relative_chunks() {
    let (_p, primary) = canonical_tempdir();
    let (_e, extra) = canonical_tempdir();
    let handle = two_root_handle("modified-root-7434", &primary, &extra);
    let table = WatchedRoot::table(&primary, std::slice::from_ref(&extra));
    let files = IndexedFiles::new();
    for (root, watched) in [(&primary, &table[0]), (&extra, &table[1])] {
        let path = root.join("same.rs");
        std::fs::write(&path, "fn same() {}\n").expect("write");
        crate::service::watch_loop::handle_modified_in_root(
            &path,
            &handle.id,
            watched,
            &handle.indexer,
            &files,
        )
        .await;
    }
    let ids = |key: &'static str| {
        let handle = Arc::clone(&handle);
        async move { handle.indexer.read().await.chunk_ids_for_file(key).await }
    };
    assert!(
        !ids("same.rs").await.is_empty(),
        "the primary file is keyed bare"
    );
    assert!(
        !ids("@root1/same.rs").await.is_empty(),
        "the extra one under @root1"
    );

    let gone = extra.join("same.rs");
    std::fs::remove_file(&gone).expect("delete");
    crate::service::watch_loop::handle_removed_in_root(
        &gone,
        &handle.id,
        &table[1],
        &handle.indexer,
        &files,
    )
    .await;
    assert!(
        ids("@root1/same.rs").await.is_empty(),
        "the extra file's chunks left"
    );
    assert!(
        !ids("same.rs").await.is_empty(),
        "the primary file's chunks stayed"
    );
}
