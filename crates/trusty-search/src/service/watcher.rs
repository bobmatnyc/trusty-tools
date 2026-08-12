//! Filesystem watcher that emits debounced [`WatchEvent`]s for an indexed root.
//!
//! Why: The daemon must keep its in-memory HNSW + chunk corpus in sync with
//! disk without re-scanning entire trees. We piggy-back on `notify` (kqueue /
//! fsevent / inotify) and a 500ms debounce window so editor save-storms do not
//! produce duplicate work.
//!
//! What: [`FileWatcher::start`] spawns a `notify-debouncer-mini` watcher on a
//! background thread; events are translated into [`WatchEvent`] and forwarded
//! through an `UnboundedSender` so the consumer can `await` them in a tokio
//! task. The debouncer is held inside the returned struct — dropping it stops
//! the watcher cleanly.
//!
//! Test: `modified_event_emitted_within_one_second` and
//! `removed_event_emitted_on_delete` below. Each re-saves a file in a
//! `tempfile::TempDir` once per debounce window and waits for the matching
//! event, rather than asserting a fixed deadline — see
//! `crate::service::watch_test_support` for why (#4731).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};
use tokio::sync::mpsc::UnboundedSender;

/// Debounce window for filesystem change coalescing. Long enough to absorb
/// editor save-storms, short enough to feel "live" to the indexer.
const DEBOUNCE_MS: u64 = 500;

/// A normalized filesystem event surfaced by [`FileWatcher`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// Path was created or modified — re-index it.
    Modified(PathBuf),
    /// Path was deleted — drop its chunks from the index.
    Removed(PathBuf),
}

/// Recursive filesystem watcher with a 500ms debounce window.
///
/// Owns the underlying `Debouncer<RecommendedWatcher>` so that dropping the
/// `FileWatcher` (or calling [`Self::stop`]) terminates the OS watch.
pub struct FileWatcher {
    _debouncer: Debouncer<RecommendedWatcher>,
}

impl FileWatcher {
    /// Begin watching `root_path` recursively. Each debounced event is mapped
    /// into a [`WatchEvent`] and pushed to `tx`. If the receiver has been
    /// dropped the send is silently ignored (the watcher will simply continue
    /// firing into the void until `self` is dropped).
    pub fn start(root_path: PathBuf, tx: UnboundedSender<WatchEvent>) -> Result<Self> {
        let mut debouncer = new_debouncer(
            Duration::from_millis(DEBOUNCE_MS),
            move |res: DebounceEventResult| match res {
                Ok(events) => {
                    for ev in events {
                        let path = ev.path.clone();
                        // notify-debouncer-mini 0.4 collapses creates/modifies
                        // into `Any`; we treat the path's existence as the
                        // discriminator since deletions yield non-existent paths.
                        let event = if path.exists() {
                            WatchEvent::Modified(path)
                        } else {
                            WatchEvent::Removed(path)
                        };
                        // Receiver dropped → nothing to do, the channel is closed.
                        let _ = tx.send(event);
                    }
                }
                Err(err) => {
                    tracing::warn!(?err, "filesystem watcher error");
                }
            },
        )
        .context("create notify debouncer")?;

        debouncer
            .watcher()
            .watch(&root_path, RecursiveMode::Recursive)
            .with_context(|| format!("watch path {}", root_path.display()))?;

        Ok(Self {
            _debouncer: debouncer,
        })
    }

    /// Stop the watcher and release OS resources by dropping the debouncer.
    pub fn stop(self) {
        // Drop semantics on `_debouncer` perform the cleanup.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::watch_test_support::await_watch_condition;
    use std::fs;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio::sync::Mutex;

    /// Drain everything queued so far and report whether any event matched.
    ///
    /// Stray events (tempdir creation, parent-directory mtime updates) are
    /// tolerated: only a match ends the wait.
    async fn saw_event(
        rx: &Arc<Mutex<mpsc::UnboundedReceiver<WatchEvent>>>,
        want: impl Fn(&WatchEvent) -> bool,
    ) -> bool {
        let mut rx = rx.lock().await;
        while let Ok(event) = rx.try_recv() {
            if want(&event) {
                return true;
            }
        }
        false
    }

    /// `file_name()` rather than `ends_with()`: the watcher delivers the
    /// canonicalized path, so this survives macOS resolving `/tmp` →
    /// `/private/var/folders/…` and any future path-normalization change.
    fn is_named(path: &std::path::Path, name: &str) -> bool {
        path.file_name().and_then(|n| n.to_str()) == Some(name)
    }

    /// Modifying a file inside the watched root produces a `Modified` event.
    ///
    /// #4731: the save is re-applied until the event arrives — a fixed sleep
    /// then a single write cannot survive an FSEvents queue overflow, which
    /// reaches this layer as no event at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn modified_event_emitted_within_one_second() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (tx, rx) = mpsc::unbounded_channel();
        let rx = Arc::new(Mutex::new(rx));

        let _watcher = FileWatcher::start(dir.path().to_path_buf(), tx).expect("watcher starts");

        let file_path = dir.path().join("hello.txt");
        let saw = {
            let rx = Arc::clone(&rx);
            await_watch_condition(
                |generation| {
                    fs::write(&file_path, format!("hello {generation}")).expect("write file");
                },
                move || {
                    let rx = Arc::clone(&rx);
                    async move {
                        saw_event(&rx, |event| {
                            matches!(event, WatchEvent::Modified(p) if is_named(p, "hello.txt"))
                        })
                        .await
                    }
                },
            )
            .await
        };

        assert!(saw, "no Modified event for hello.txt arrived");
    }

    /// Deleting a previously-created file produces a `Removed` event.
    ///
    /// #4731: each generation recreates and re-deletes the file, so a dropped
    /// delete event is retried instead of stranding the test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn removed_event_emitted_on_delete() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("doomed.txt");
        fs::write(&file_path, b"transient").expect("write file");

        let (tx, rx) = mpsc::unbounded_channel();
        let rx = Arc::new(Mutex::new(rx));
        let _watcher = FileWatcher::start(dir.path().to_path_buf(), tx).expect("watcher starts");

        let saw = {
            let rx = Arc::clone(&rx);
            await_watch_condition(
                |generation| {
                    // Recreate then delete: the debouncer coalesces the pair
                    // into one event whose path no longer exists, which is the
                    // `Removed` classification under test.
                    fs::write(&file_path, format!("transient {generation}")).expect("write file");
                    fs::remove_file(&file_path).expect("delete file");
                },
                move || {
                    let rx = Arc::clone(&rx);
                    async move {
                        saw_event(&rx, |event| {
                            matches!(event, WatchEvent::Removed(p) if is_named(p, "doomed.txt"))
                        })
                        .await
                    }
                },
            )
            .await
        };

        assert!(saw, "no Removed event for doomed.txt arrived");
    }
}
