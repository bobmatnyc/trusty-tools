//! Unit tests for [`super::WatcherManager`] (split out of `watcher_manager.rs`, #7434).

use super::*;
use crate::core::CodeIndexer;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Build a bare registered-style handle pointing at `root`.
fn handle_for(id: &str, root: &std::path::Path) -> Arc<IndexHandle> {
    let indexer = Arc::new(RwLock::new(CodeIndexer::new(id, root)));
    Arc::new(IndexHandle::bare(
        IndexId::new(id),
        indexer,
        root.to_path_buf(),
    ))
}

/// Why: the opt-out gate must match ONLY the exact value "1" so a stray
/// `TRUSTY_DISABLE_WATCHER=true` (or `0`, or any other value) doesn't
/// silently disable incremental indexing. Issue #1641: the previous version
/// of this test only compared raw literals and never touched the real
/// decision function, so it could pass even if the gate were broken.
/// Test: exercises the pure `watcher_disabled_for_value` helper that
/// `watcher_disabled()` delegates to — no process-env mutation needed.
#[test]
fn disable_env_gate_only_matches_one() {
    // Only the exact "1" disables the watcher.
    assert!(watcher_disabled_for_value(Some("1")));

    // Every other value leaves the watcher enabled.
    assert!(!watcher_disabled_for_value(None)); // unset
    assert!(!watcher_disabled_for_value(Some(""))); // empty
    assert!(!watcher_disabled_for_value(Some("0")));
    assert!(!watcher_disabled_for_value(Some("true")));
    assert!(!watcher_disabled_for_value(Some("yes")));
    assert!(!watcher_disabled_for_value(Some("on")));
    assert!(!watcher_disabled_for_value(Some(" 1"))); // not trimmed
    assert!(!watcher_disabled_for_value(Some("1 ")));
    assert!(!watcher_disabled_for_value(Some("11")));
}

/// Why: spawning twice for the same index must keep exactly one watcher so
/// re-registration (e.g. a reindex that re-registers the handle) never
/// leaks watchers.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_is_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mgr = WatcherManager::new();
    let handle = handle_for("idx", dir.path());

    mgr.spawn_for_index(&handle).await;
    mgr.spawn_for_index(&handle).await;

    assert_eq!(
        mgr.watched_count().await,
        1,
        "second spawn for the same index must not add a second watcher"
    );
    mgr.stop_all().await;
}

/// Why: the idle-suspend / wake path keys off `is_watching`, so it must
/// track spawn and stop transitions exactly (false → true → false).
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn is_watching_reflects_spawn_and_stop() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mgr = WatcherManager::new();
    let handle = handle_for("idx", dir.path());
    let id = IndexId::new("idx");

    assert!(!mgr.is_watching(&id).await, "not watching before spawn");
    mgr.spawn_for_index(&handle).await;
    assert!(mgr.is_watching(&id).await, "watching after spawn");
    assert!(mgr.stop_for_index(&id).await);
    assert!(!mgr.is_watching(&id).await, "not watching after stop");
}

/// Why: `stop_for_index` must remove exactly the targeted watcher and
/// report whether one was present.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_for_index_removes_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mgr = WatcherManager::new();
    let handle = handle_for("idx", dir.path());

    mgr.spawn_for_index(&handle).await;
    assert_eq!(mgr.watched_count().await, 1);

    let stopped = mgr.stop_for_index(&handle.id).await;
    assert!(stopped, "stop_for_index must report it stopped a watcher");
    assert_eq!(mgr.watched_count().await, 0);

    // Stopping again is a no-op.
    assert!(!mgr.stop_for_index(&handle.id).await);
}

/// #3049: `stop_for_index` must not return while the watcher still holds the
/// index's indexer.
///
/// Why: the watcher's `Arc<RwLock<CodeIndexer>>` owns the index's open redb
/// corpus, and redb is single-open. `DELETE /indexes/:id` calls this while
/// holding the teardown write guard and then releases it; if the watcher
/// task is still alive at that moment, the recreate that follows cannot open
/// the corpus, sets `corpus_open_failed`, and answers `500` —
/// `create_index_cannot_register_while_a_delete_is_tearing_the_id_down`'s
/// intermittent CI failure. `WatcherTask::stop` used to call only `abort()`,
/// which drops the task's future inline when the task is IDLE but not when
/// it is running, so a watcher with events to process outlived the call.
/// What: drives real file events through the loop first, then asserts that
/// nothing owns `indexer` once `stop_for_index` returns — the watcher is the
/// only other owner by then, so `Weak::strong_count` reads exactly "does the
/// watcher still hold it".
///
/// HONEST LIMIT: this is an invariant guard, NOT a proof. It does not fail
/// against the pre-fix code, because `abort()` DOES reap an idle task inline
/// and this test cannot pin the watcher mid-poll without a hook into the
/// loop. What the fix rests on is a direct measurement instead: instrumented
/// `unregister_index` reported the watcher still holding the indexer at the
/// end of all 60 of 60 runs of
/// `create_index_cannot_register_while_a_delete_is_tearing_the_id_down`, and
/// 0 of 1 with `TRUSTY_DISABLE_WATCHER=1`. See the PR for the raw counts.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_for_index_releases_the_indexer_before_it_returns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let id = IndexId::new("release-idx");
    let indexer = Arc::new(RwLock::new(CodeIndexer::new("release-idx", dir.path())));
    let weak = Arc::downgrade(&indexer);
    let handle = Arc::new(IndexHandle::bare(
        id.clone(),
        Arc::clone(&indexer),
        dir.path().to_path_buf(),
    ));
    drop(indexer);

    let mgr = WatcherManager::new();
    mgr.spawn_for_index(&handle).await;
    assert!(mgr.is_watching(&id).await, "watcher must be running");

    // Drive the loop until it has provably picked work up, so the abort
    // below lands on a task that is doing something rather than one parked
    // on an empty channel.
    let file = dir.path().join("busy.rs");
    {
        let probe = Arc::clone(&handle);
        let reacted = crate::service::watch_test_support::await_watch_condition(
            |generation| {
                for n in 0..16 {
                    std::fs::write(
                        dir.path().join(format!("busy{n}.rs")),
                        format!("fn f{generation}_{n}() {{}}\n"),
                    )
                    .expect("write file");
                }
                std::fs::write(&file, format!("fn busy{generation}() {{}}\n")).expect("write file");
            },
            move || {
                let probe = Arc::clone(&probe);
                async move { probe.indexer.read().await.chunk_count() > 0 }
            },
        )
        .await;
        assert!(reacted, "watcher never indexed the stimulus");
    }

    // The watcher's clone is now the only other owner; drop ours so the
    // count below is unambiguous.
    drop(handle);

    assert!(mgr.stop_for_index(&id).await, "a watcher was stopped");

    assert_eq!(
        weak.strong_count(),
        0,
        "stop_for_index returned while the watcher task still held the \
         indexer — its redb corpus is still open, so a recreate under this \
         id would fail to open it and answer 500 (issue #3049)"
    );
}

/// Why: graceful shutdown must clear every watcher and report the count.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_all_clears_all() {
    let dir_a = tempfile::tempdir().expect("tempdir a");
    let dir_b = tempfile::tempdir().expect("tempdir b");
    let mgr = WatcherManager::new();

    mgr.spawn_for_index(&handle_for("a", dir_a.path())).await;
    mgr.spawn_for_index(&handle_for("b", dir_b.path())).await;
    assert_eq!(mgr.watched_count().await, 2);

    let stopped = mgr.stop_all().await;
    assert_eq!(stopped, 2, "stop_all must report every watcher it stopped");
    assert_eq!(mgr.watched_count().await, 0);
}

/// Why: end-to-end — after the manager spawns a watcher for an index, a file
/// save must be incrementally indexed (chunk count grows) within the
/// debounce window. This is the core acceptance criterion of issue #1621.
/// Test: this test.
///
/// #4731: the save is re-applied until the index reacts, so a dropped
/// FSEvents batch no longer strands a fixed 3 s deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn save_triggers_incremental_index_via_manager() {
    let dir = tempfile::tempdir().expect("tempdir");
    let handle = handle_for("live", dir.path());
    let mgr = WatcherManager::new();
    mgr.spawn_for_index(&handle).await;

    let file = dir.path().join("lib.rs");
    let indexed = {
        let handle = Arc::clone(&handle);
        crate::service::watch_test_support::await_watch_condition(
            |generation| {
                std::fs::write(
                    &file,
                    format!("fn alpha() {{}}\nfn beta{generation}() {{}}\n"),
                )
                .expect("write file");
            },
            move || {
                let handle = Arc::clone(&handle);
                async move { handle.indexer.read().await.chunk_count() > 0 }
            },
        )
        .await
    };

    assert!(
        indexed,
        "chunk_count never grew — watcher did not index the save"
    );
    mgr.stop_all().await;
}

// ── Issue #3408: network-mount detection wired into the spawn path ──────

/// Why: this is the core acceptance criterion of issue #3408 — a root
/// positively identified as network-mounted must NOT get a live watcher
/// (it would silently never fire for cross-host writes), and the
/// refusal must be reported through `network_degraded_count` /
/// `network_degraded_reason` with an actionable message naming the
/// `index-file` / `remove-file` endpoints, rather than just logged and
/// forgotten. Uses `spawn_for_index_with_mount_kind` to inject
/// `MountKind::Network` directly instead of requiring a real NFS/CIFS/SMB
/// mount in CI.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn network_mount_root_is_refused_and_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mgr = WatcherManager::new();
    let handle = handle_for("net-idx", dir.path());
    let id = IndexId::new("net-idx");

    mgr.spawn_for_index_with_mount_kind(&handle, MountKind::Network)
        .await;

    assert!(
        !mgr.is_watching(&id).await,
        "a network-mounted root must never get a live watcher"
    );
    assert_eq!(
        mgr.watched_count().await,
        0,
        "no watcher should have been inserted"
    );
    assert_eq!(
        mgr.network_degraded_count().await,
        1,
        "the refusal must be recorded so /health can surface it"
    );
    let reason = mgr
        .network_degraded_reason(&id)
        .await
        .expect("reason must be present for a network-degraded index");
    assert!(
        reason.contains("index-file") && reason.contains("remove-file"),
        "actionable message must name the supported per-file endpoints, got: {reason}"
    );
}

/// Why: the network-mount check must be a pure gate in front of the
/// existing spawn path — passing `MountKind::Local` (the classification a
/// real local disk always resolves to, per `network_fs` tests) must leave
/// watcher startup completely unaffected, and must NOT record any
/// degraded entry. This is the regression guard against the network
/// check accidentally short-circuiting or corrupting the normal path.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_root_is_unaffected_by_network_check() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mgr = WatcherManager::new();
    let handle = handle_for("local-idx", dir.path());
    let id = IndexId::new("local-idx");

    mgr.spawn_for_index_with_mount_kind(&handle, MountKind::Local)
        .await;

    assert!(
        mgr.is_watching(&id).await,
        "a local root must still get a live watcher"
    );
    assert_eq!(mgr.watched_count().await, 1);
    assert_eq!(
        mgr.network_degraded_count().await,
        0,
        "a local root must never be recorded as network-degraded"
    );
    assert!(mgr.network_degraded_reason(&id).await.is_none());

    mgr.stop_all().await;
}

/// Why: `stop_for_index` (e.g. `DELETE /indexes/:id`) must clear a stale
/// network-degraded entry too, not just live watchers — otherwise
/// `/health` would keep reporting a degraded index that no longer exists.
/// Test: this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_for_index_clears_network_degraded_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mgr = WatcherManager::new();
    let handle = handle_for("net-idx-2", dir.path());
    let id = IndexId::new("net-idx-2");

    mgr.spawn_for_index_with_mount_kind(&handle, MountKind::Network)
        .await;
    assert_eq!(mgr.network_degraded_count().await, 1);

    // stop_for_index normally reports `false` when there was no live
    // watcher (there wasn't one here — the whole point of the refusal),
    // but it must still clear the degraded bookkeeping.
    mgr.stop_for_index(&id).await;
    assert_eq!(
        mgr.network_degraded_count().await,
        0,
        "stop_for_index must clear the network-degraded entry"
    );
}
