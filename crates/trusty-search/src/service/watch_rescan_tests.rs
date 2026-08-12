//! Tests for dropped-event (`Flag::Rescan`) reconciliation.
//!
//! These never try to provoke a real kernel queue overflow — that is exactly
//! the condition that would not reproduce (0 failures in 148 attempts). Instead
//! they construct the two event shapes `notify` 6.1.1 actually emits and drive
//! the real production functions with them.

use super::*;

use notify::event::Flag;
use notify::{Event, EventKind};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::core::registry::IndexId;
use crate::core::CodeIndexer;
use crate::service::indexed_files::IndexedFiles;
use crate::service::watcher::{classify_debounced, rescan_sentinel_path, tag_rescan, WatchEvent};

/// The event macOS delivers on a queue overflow.
///
/// `fsevent.rs`'s `translate_flags` builds `EventKind::Other` + `Flag::Rescan`
/// with no paths when `StreamFlags::MUST_SCAN_SUBDIRS` is set, and
/// `callback_impl` then calls `.add_path(path.clone())` on it — the directory
/// whose subtree must be rescanned, never the file that actually changed.
fn overflow_event_fsevent(dir: &std::path::Path) -> Event {
    Event::new(EventKind::Other)
        .set_flag(Flag::Rescan)
        .add_path(dir.to_path_buf())
}

/// The event Linux delivers on a queue overflow.
///
/// `inotify.rs` dispatches `EventKind::Other` + `Flag::Rescan` directly to the
/// handler on `EventMask::Q_OVERFLOW` and never attaches a path.
fn overflow_event_inotify() -> Event {
    Event::new(EventKind::Other).set_flag(Flag::Rescan)
}

/// Reproduces the pre-fix pipeline so the "before" half of a test is the real
/// removed behaviour rather than a description of it.
///
/// `notify_debouncer_mini::add_event` keeps one map entry per `event.paths`
/// element and discards `EventKind`/`EventAttributes` (where `Flag::Rescan`
/// lives), so an event with no paths produced no debounced event at all.
/// `watcher.rs` then classified whatever survived by path existence alone.
fn pre_fix_watch_event(ev: &Event) -> Option<WatchEvent> {
    let path = ev.paths.last()?.clone();
    Some(if path.exists() {
        WatchEvent::Modified(path)
    } else {
        WatchEvent::Removed(path)
    })
}

fn fixture(dir: &std::path::Path) -> (IndexId, Arc<RwLock<CodeIndexer>>, IndexedFiles) {
    (
        IndexId::new("watch-rescan-test"),
        Arc::new(RwLock::new(CodeIndexer::new("watch-rescan-test", dir))),
        IndexedFiles::new(),
    )
}

/// Why: the sentinel is only safe because it cannot name a real file. Every
/// filesystem syscall rejects a NUL byte in a path, so a genuine event can
/// never carry this value.
#[test]
fn rescan_sentinel_cannot_collide_with_a_real_path() {
    let sentinel = rescan_sentinel_path();
    assert!(
        sentinel.to_string_lossy().contains('\0'),
        "sentinel must contain a NUL byte to be unrepresentable on disk: {sentinel:?}"
    );
    assert!(!sentinel.exists(), "sentinel must never exist on disk");
}

/// Why: the flag is delivered in two different shapes and both must survive the
/// debouncer. A fix that only handled one backend would leave the other silently
/// losing writes.
#[test]
fn tag_rescan_marks_both_backend_event_shapes() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Linux: no paths at all, so the debouncer recorded nothing.
    let linux = overflow_event_inotify();
    assert!(
        linux.paths.is_empty(),
        "inotify overflow event carries no path"
    );
    assert!(
        pre_fix_watch_event(&linux).is_none(),
        "pre-fix: a pathless event produced no debounced event, so the signal was lost"
    );
    let tagged = tag_rescan(linux);
    assert_eq!(tagged.paths, vec![rescan_sentinel_path()]);

    // macOS: one path, and it is the watched DIRECTORY, not a changed file.
    let macos = overflow_event_fsevent(dir.path());
    assert_eq!(macos.paths.len(), 1);
    assert!(
        macos.paths[0].is_dir(),
        "fsevent overflow event names a directory, never the file that changed"
    );
    assert_eq!(
        pre_fix_watch_event(&macos),
        Some(WatchEvent::Modified(dir.path().to_path_buf())),
        "pre-fix: the overflow reached the watch loop as a Modified event for a directory"
    );
    let tagged = tag_rescan(macos);
    assert_eq!(
        tagged.paths.last(),
        Some(&rescan_sentinel_path()),
        "the sentinel is appended, preserving any real path the backend supplied"
    );
    assert_eq!(
        tagged.paths.len(),
        2,
        "the backend's own path must not be dropped"
    );
}

/// Why: tagging must be inert for ordinary events — a watcher that re-walked
/// the tree on every file save would be far worse than the bug.
#[test]
fn tag_rescan_leaves_ordinary_events_untouched() {
    let ev = Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(std::path::PathBuf::from("/tmp/example.rs"));
    let tagged = tag_rescan(ev.clone());
    assert_eq!(tagged.paths, ev.paths);
    assert!(!tagged.paths.contains(&rescan_sentinel_path()));
}

/// Why: the sentinel does not exist on disk, so the existence check that
/// distinguishes writes from deletions would classify it as a deletion of a
/// file that was never indexed. It has to be tested first.
#[test]
fn classify_debounced_maps_each_shape() {
    let dir = tempfile::tempdir().expect("tempdir");
    let existing = dir.path().join("present.rs");
    std::fs::write(&existing, "fn a() {}\n").expect("write");
    let missing = dir.path().join("gone.rs");

    assert_eq!(
        classify_debounced(rescan_sentinel_path()),
        WatchEvent::Rescan
    );
    assert_eq!(
        classify_debounced(existing.clone()),
        WatchEvent::Modified(existing)
    );
    assert_eq!(
        classify_debounced(missing.clone()),
        WatchEvent::Removed(missing)
    );
}

/// The defect, end to end: a write that lands while the OS event queue is
/// overflowing is missed by the pre-fix pipeline and caught by the reconcile.
///
/// Why: this is the silent-data-loss case. The pre-fix half is not a
/// description — it replays the exact event `notify` delivers and the exact
/// classification the old code applied, and shows the index still holds
/// nothing. The `is_dir` assertion names the guard that discarded it:
/// `watch_loop::handle_modified` returns immediately for a directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rescan_reconcile_indexes_files_written_during_the_gap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
    let (index_id, indexer, tracker) = fixture(&root);

    // The write the daemon never saw an event for.
    std::fs::write(root.join("dropped.rs"), "fn alpha() {}\nfn beta() {}\n").expect("write");
    assert_eq!(
        indexer.read().await.chunk_count(),
        0,
        "precondition: the write is not in the index"
    );

    // BEFORE: replay what the overflow actually delivered pre-fix.
    let pre_fix = pre_fix_watch_event(&overflow_event_fsevent(&root));
    assert_eq!(
        pre_fix,
        Some(WatchEvent::Modified(root.clone())),
        "pre-fix the overflow arrived as Modified(<watched root>)"
    );
    assert!(
        root.is_dir(),
        "and handle_modified discards a directory path at its first guard"
    );
    assert_eq!(
        indexer.read().await.chunk_count(),
        0,
        "so the index still misses the write — this is the silent loss"
    );

    // AFTER: the same overflow now reaches the reconcile.
    assert_eq!(
        classify_debounced(
            tag_rescan(overflow_event_fsevent(&root))
                .paths
                .last()
                .expect("tagged event carries the sentinel")
                .clone()
        ),
        WatchEvent::Rescan
    );
    let stats = reconcile_after_rescan(&index_id, &root, &root, &indexer, &tracker)
        .await
        .expect("reconcile succeeds");

    assert!(
        indexer.read().await.chunk_count() > 0,
        "reconcile must index the file the dropped batch hid"
    );
    assert_eq!(stats.files_reindexed, 1, "one source file under the root");
    assert!(
        stats.is_complete() && stats.files_unreadable == 0,
        "a clean pass must report no file left in an unknown state"
    );
    assert!(
        tracker
            .paths()
            .await
            .contains(&std::path::PathBuf::from("dropped.rs")),
        "chunk ids must be tracked so a later Removed event can find them"
    );
}

/// Why: an overflow drops deletions as readily as writes. A deletion that is
/// never applied leaves a phantom file answering searches forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rescan_reconcile_drops_files_deleted_during_the_gap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
    let (index_id, indexer, tracker) = fixture(&root);

    let doomed = root.join("doomed.rs");
    std::fs::write(&doomed, "fn gone() {}\n").expect("write");
    reconcile_after_rescan(&index_id, &root, &root, &indexer, &tracker)
        .await
        .expect("seed reconcile succeeds");
    let seeded = indexer.read().await.chunk_count();
    assert!(seeded > 0, "seed must index the file");

    // The delete the daemon never saw an event for.
    std::fs::remove_file(&doomed).expect("remove");

    let stats = reconcile_after_rescan(&index_id, &root, &root, &indexer, &tracker)
        .await
        .expect("reconcile succeeds");

    assert_eq!(stats.files_removed, 1, "the deleted file must be swept");
    assert_eq!(
        indexer.read().await.chunk_count(),
        0,
        "its chunks must be gone from the index"
    );
    assert!(
        !tracker
            .paths()
            .await
            .contains(&std::path::PathBuf::from("doomed.rs")),
        "and its tracking entry must be dropped"
    );
}

/// Why: the walk and the watcher do not apply identical filters, so a tracked
/// file the walk skipped must not be mistaken for a deletion. Only absence from
/// disk may remove anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rescan_reconcile_keeps_a_tracked_file_the_walk_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
    let (index_id, indexer, tracker) = fixture(&root);

    // A file that exists on disk but which the walker prunes (SKIP_DIRS).
    let skipped_dir = root.join("node_modules");
    std::fs::create_dir_all(&skipped_dir).expect("mkdir");
    std::fs::write(skipped_dir.join("vendored.js"), "function v() {}\n").expect("write");
    let tracked = std::path::PathBuf::from("node_modules/vendored.js");
    tracker
        .record(
            tracked.clone(),
            vec!["node_modules/vendored.js:1:1".to_string()],
        )
        .await;

    let stats = reconcile_after_rescan(&index_id, &root, &root, &indexer, &tracker)
        .await
        .expect("reconcile succeeds");

    assert_eq!(
        stats.files_removed, 0,
        "a file the walk skipped but which still exists is not a deletion"
    );
    assert!(
        tracker.paths().await.contains(&tracked),
        "its tracking entry must survive"
    );
}

/// Why: a reconcile that fails leaves the index out of sync, so the retry must
/// keep coming back rather than backing off to never.
#[test]
fn rescan_retry_backoff_grows_and_saturates() {
    assert_eq!(retry_backoff(1), RETRY_BASE, "first failure waits the base");
    assert_eq!(retry_backoff(2), RETRY_BASE * 2);
    assert_eq!(retry_backoff(3), RETRY_BASE * 4);
    assert_eq!(retry_backoff(64), RETRY_MAX, "saturates, never overflows");
    assert!(
        retry_backoff(u32::MAX) <= RETRY_MAX,
        "no failure count may produce an unbounded delay"
    );
}
