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
//! `crate::service::watch_test_support` for why (#4731). Dropped-event
//! (`Flag::Rescan`) handling is covered by
//! `crate::service::watch_rescan_tests`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::{Event, EventHandler, RecommendedWatcher, RecursiveMode, Watcher, WatcherKind};
use notify_debouncer_mini::{
    new_debouncer_opt, Config as DebouncerConfig, DebounceEventResult, Debouncer,
};
use tokio::sync::mpsc::UnboundedSender;

/// Debounce window for filesystem change coalescing. Long enough to absorb
/// editor save-storms, short enough to feel "live" to the indexer.
const DEBOUNCE_MS: u64 = 500;

/// Sentinel path attached to a `Flag::Rescan` event so the debouncer cannot
/// discard the signal.
///
/// Why: `notify_debouncer_mini::DebouncedEvent` carries only `{path, kind}`.
/// Its `add_event` reads `event.paths` and throws away `EventKind` and
/// `EventAttributes` — and `Flag::Rescan` lives in `EventAttributes`. So the
/// flag is destroyed before any consumer of the debounced stream can see it,
/// on every platform. Tagging the raw event with a path routes the signal
/// through the debouncer's existing delivery path rather than a second
/// channel, and inherits the 500ms coalescing: a queue-overflow storm that
/// emits many rescan events produces one reconcile, not one per event.
///
/// What: a name containing a NUL byte. Every filesystem syscall rejects NUL in
/// a path, so no real file can produce this path and the sentinel can never be
/// confused with a genuine event.
///
/// Test: `rescan_sentinel_cannot_collide_with_a_real_path` and
/// `tag_rescan_marks_both_backend_event_shapes` in `watch_rescan_tests`.
const RESCAN_SENTINEL: &str = "\0trusty-search::watcher::rescan";

/// A normalized filesystem event surfaced by [`FileWatcher`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// Path was created or modified — re-index it.
    Modified(PathBuf),
    /// Path was deleted — drop its chunks from the index.
    Removed(PathBuf),
    /// The OS dropped an unknown batch of events — the watched tree must be
    /// reconciled because the specific changed paths are unrecoverable.
    ///
    /// Why: `notify` raises `Flag::Rescan` when the kernel or user event queue
    /// overflows (macOS `StreamFlags::MUST_SCAN_SUBDIRS` in `fsevent.rs`, Linux
    /// `EventMask::Q_OVERFLOW` in `inotify.rs`). Nothing redelivers the lost
    /// events, so a consumer that ignores this reports success while the index
    /// silently misses every change in the dropped batch.
    Rescan,
}

/// The sentinel path that [`WatchEvent::Rescan`] travels under inside the
/// debouncer. See [`RESCAN_SENTINEL`].
pub(crate) fn rescan_sentinel_path() -> PathBuf {
    PathBuf::from(RESCAN_SENTINEL)
}

/// Attach [`RESCAN_SENTINEL`] to a raw event that carries `Flag::Rescan`.
///
/// Why: this runs on the RAW `notify::Event`, before the debouncer, because
/// that is the last point at which the flag still exists. The two backends
/// deliver the flag in different shapes and neither survives the debouncer
/// unaided (verified against notify 6.1.1 in this workspace's lock file):
/// macOS `fsevent.rs` builds the event with no paths at `translate_flags`, then
/// `callback_impl` attaches one path — the DIRECTORY whose subtree must be
/// rescanned, which `watch_loop::handle_modified` discards at its `is_dir()`
/// guard. Linux `inotify.rs` dispatches the event with no paths at all, which
/// the debouncer's `for path in event.paths` loop records as nothing.
///
/// What: appends the sentinel rather than replacing `event.paths`, so any real
/// path the backend supplied is still delivered on its own merits.
///
/// Test: `tag_rescan_marks_both_backend_event_shapes` and
/// `tag_rescan_leaves_ordinary_events_untouched` in `watch_rescan_tests`.
pub(crate) fn tag_rescan(event: Event) -> Event {
    if event.need_rescan() {
        event.add_path(rescan_sentinel_path())
    } else {
        event
    }
}

/// Map one debounced path onto a [`WatchEvent`].
///
/// Why: the debouncer collapses creates/modifies into `Any`, so path existence
/// is the only available discriminator between a write and a delete. The
/// sentinel must be tested FIRST — it never exists on disk and would otherwise
/// be misread as a deletion of a file that was never indexed.
///
/// Test: `classify_debounced_maps_each_shape` in `watch_rescan_tests`.
pub(crate) fn classify_debounced(path: PathBuf) -> WatchEvent {
    if path == rescan_sentinel_path() {
        WatchEvent::Rescan
    } else if path.exists() {
        WatchEvent::Modified(path)
    } else {
        WatchEvent::Removed(path)
    }
}

/// `notify` watcher that forwards every raw event through [`tag_rescan`] on its
/// way to the debouncer.
///
/// Why: `notify_debouncer_mini` constructs its own watcher via the `T: Watcher`
/// type parameter of `new_debouncer_opt` and never exposes the raw event
/// stream, so wrapping the watcher is the only seam at which `Flag::Rescan` can
/// be read. Delegating to `RecommendedWatcher` keeps exactly one OS watch per
/// root — a second watcher would double this daemon's FSEvents streams.
///
/// What: a newtype over `RecommendedWatcher` whose `new` wraps the debouncer's
/// own event handler; every other trait method delegates unchanged.
struct RescanTapWatcher {
    inner: RecommendedWatcher,
}

impl Watcher for RescanTapWatcher {
    fn new<F: EventHandler>(mut handler: F, config: notify::Config) -> notify::Result<Self> {
        let inner = RecommendedWatcher::new(
            move |res: notify::Result<Event>| handler.handle_event(res.map(tag_rescan)),
            config,
        )?;
        Ok(Self { inner })
    }

    fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
        self.inner.watch(path, recursive_mode)
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        self.inner.unwatch(path)
    }

    fn configure(&mut self, option: notify::Config) -> notify::Result<bool> {
        self.inner.configure(option)
    }

    fn kind() -> WatcherKind {
        RecommendedWatcher::kind()
    }
}

/// Recursive filesystem watcher with a 500ms debounce window.
///
/// Owns the underlying `Debouncer<RescanTapWatcher>` so that dropping the
/// `FileWatcher` (or calling [`Self::stop`]) terminates the OS watch.
pub struct FileWatcher {
    _debouncer: Debouncer<RescanTapWatcher>,
}

impl FileWatcher {
    /// Begin watching `root_path` recursively. Each debounced event is mapped
    /// into a [`WatchEvent`] and pushed to `tx`. If the receiver has been
    /// dropped the send is silently ignored (the watcher will simply continue
    /// firing into the void until `self` is dropped).
    pub fn start(root_path: PathBuf, tx: UnboundedSender<WatchEvent>) -> Result<Self> {
        let mut debouncer: Debouncer<RescanTapWatcher> = new_debouncer_opt(
            DebouncerConfig::default().with_timeout(Duration::from_millis(DEBOUNCE_MS)),
            move |res: DebounceEventResult| match res {
                Ok(events) => {
                    for ev in events {
                        // Receiver dropped → nothing to do, the channel is closed.
                        let _ = tx.send(classify_debounced(ev.path));
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
