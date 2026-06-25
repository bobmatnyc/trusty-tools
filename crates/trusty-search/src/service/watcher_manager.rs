//! Daemon-owned registry of live [`WatcherTask`]s, one per watched index.
//!
//! Why: issue #1621 (epic #1619 WI-2) activates the dormant `FileWatcher` so a
//! file save triggers incremental indexing within the existing 500ms debounce
//! window. The watcher itself (`spawn_watch_loop`) was fully implemented but
//! never started by the daemon (issue #6). This manager is the missing glue: it
//! starts a watcher when an index is registered (warm-boot restore or
//! `POST /indexes`), stops it when the index is deleted, and tears every watcher
//! down on graceful shutdown (SIGTERM) so no OS watch or tokio task outlives the
//! daemon.
//!
//! Allowlist / opt-in invariant: trusty-search uses a default-deny allowlist
//! model (issue #767/#1619). An `IndexHandle` only ever exists for a repo that
//! passed through the allowlist (warm-boot reads `indexes.toml`; `POST /indexes`
//! is the sanctioned registration path). Because this manager is driven purely
//! off handle registration, it is automatically a **no-op for unregistered
//! repos** — there is no handle, so no watcher. No extra allowlist check is
//! needed here, and adding one would risk diverging from the registry's own
//! gate.
//!
//! What: a `Mutex<HashMap<IndexId, WatcherTask>>` plus `spawn_for_index`,
//! `stop_for_index`, and `stop_all`. Each watcher shares the same
//! `Arc<RwLock<CodeIndexer>>` as the handle so incremental `index_file` /
//! `remove_chunk` calls land in the live index. A fresh `IndexedFiles` tracker
//! is created per index so deletions can locate the chunk IDs to evict.
//!
//! Test: unit tests at the bottom of this module cover idempotent spawn, the
//! `TRUSTY_DISABLE_WATCHER` opt-out, stop-for-index, and stop-all.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::core::registry::{IndexHandle, IndexId};
use crate::service::indexed_files::IndexedFiles;
use crate::service::watch_loop::{spawn_watch_loop, WatcherTask};

/// Environment variable that disables the filesystem watcher entirely.
///
/// Why: operators on network-backed or very large filesystems may prefer the
/// reindex-on-commit hook (WI-1) and want to avoid the OS watch cost. Setting
/// this to `1` makes every `spawn_for_index` a no-op without removing the
/// machinery.
const DISABLE_WATCHER_ENV: &str = "TRUSTY_DISABLE_WATCHER";

/// Pure decision for the watcher opt-out gate, given a raw env-var value.
///
/// Why: the disable gate must be unit-testable without mutating process-global
/// env state (which is unsound to set/unset concurrently in a multi-threaded
/// test binary). Splitting the pure decision out from the env read lets a test
/// exercise the *real* logic across every relevant value directly. Issue #1641.
/// What: returns `true` only for the exact value `Some("1")`; any other value —
/// unset (`None`), `Some("0")`, `Some("true")`, whitespace, … — leaves the
/// watcher enabled, so an operator who sets a non-`1` value is never silently
/// opted out of incremental indexing.
/// Test: `disable_env_gate_only_matches_one`.
fn watcher_disabled_for_value(val: Option<&str>) -> bool {
    val == Some("1")
}

/// True when the watcher is disabled via `TRUSTY_DISABLE_WATCHER=1`.
///
/// Why: centralises the opt-out check so both the manager and any future caller
/// read the same flag.
/// What: reads `TRUSTY_DISABLE_WATCHER` from the process env and delegates the
/// decision to the pure [`watcher_disabled_for_value`] — so only the exact
/// value `"1"` disables the watcher.
/// Test: covered indirectly via `watcher_disabled_for_value`'s unit test
/// (`disable_env_gate_only_matches_one`); the env read itself is a thin,
/// side-effect-only wrapper.
fn watcher_disabled() -> bool {
    watcher_disabled_for_value(std::env::var(DISABLE_WATCHER_ENV).ok().as_deref())
}

/// Registry of running watchers keyed by index id.
///
/// Why: the daemon needs a single place to own watcher lifetimes so they can be
/// started on registration, stopped on deletion, and all torn down on shutdown.
/// Cheap to clone (`Arc` inside `SearchAppState`); the inner `Mutex` is only
/// taken for the brief insert/remove operations, never across a file event.
/// What: maps `IndexId` → `WatcherTask`. Dropping or `stop`-ing a `WatcherTask`
/// aborts its consumer task and releases the OS watch.
/// Test: see module tests.
#[derive(Clone, Default)]
pub struct WatcherManager {
    inner: Arc<Mutex<HashMap<IndexId, WatcherTask>>>,
}

impl WatcherManager {
    /// Construct an empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start watching `handle.root_path`, forwarding changes into the handle's
    /// indexer. Idempotent and opt-out aware.
    ///
    /// Why: called from every index-registration path (warm-boot restore and
    /// `POST /indexes`) so a freshly-registered index becomes self-updating
    /// without a manual reindex. Returns early when the watcher is globally
    /// disabled or when this index is already being watched, so callers can fire
    /// it unconditionally after `registry.register`.
    /// What: (1) no-op when `TRUSTY_DISABLE_WATCHER=1`; (2) builds the watcher
    /// task by calling `spawn_watch_loop` **before** acquiring the lock — the
    /// `Mutex` is then taken only for the brief `contains_key` + `insert`, never
    /// across the (potentially blocking) OS-watch installation; (3) the spawn is
    /// idempotent: if a racing `spawn_for_index` won the insert for this `id`
    /// while we were building our task, we drop the just-built task (`stop()`)
    /// and keep the existing one. A `spawn_watch_loop` failure (e.g. the root
    /// path vanished between registration and now) is logged at WARN and
    /// swallowed so registration never fails because of the watcher.
    /// Test: `spawn_is_idempotent`, `disable_env_gate_only_matches_one`.
    pub async fn spawn_for_index(&self, handle: &Arc<IndexHandle>) {
        if watcher_disabled() {
            tracing::debug!(
                index_id = %handle.id,
                "file watcher disabled via {DISABLE_WATCHER_ENV}=1 — not watching",
            );
            return;
        }

        // Build the watcher task BEFORE acquiring the lock (issue #1640). This
        // (a) closes the deadlock window — the `Mutex` is never held across the
        // potentially-blocking `spawn_watch_loop` (slow filesystem, inotify
        // limit exhaustion) so `stop_for_index`/`stop_all` can never block on a
        // spawn — and (b) keeps the critical section to a bare insert.
        let indexed_files = IndexedFiles::new();
        let task = match spawn_watch_loop(
            &handle.root_path,
            Arc::clone(&handle.indexer),
            indexed_files,
        ) {
            Ok(task) => task,
            Err(e) => {
                tracing::warn!(
                    index_id = %handle.id,
                    root = %handle.root_path.display(),
                    "could not start file watcher (incremental indexing disabled for this index): {e:#}",
                );
                return;
            }
        };

        // Lock ONLY to insert. The earlier check-then-insert had a TOCTOU
        // window: two concurrent `spawn_for_index` calls for the same id could
        // both pass `contains_key` before either inserted. Re-check under the
        // lock and, if a racing insert already won, stop the task we just built
        // so we keep exactly one watcher (idempotent).
        let mut guard = self.inner.lock().await;
        if guard.contains_key(&handle.id) {
            drop(guard);
            task.stop();
            return;
        }
        guard.insert(handle.id.clone(), task);
        drop(guard);
        tracing::info!(
            index_id = %handle.id,
            root = %handle.root_path.display(),
            "file watcher active — saves trigger incremental indexing (issue #1621)",
        );
    }

    /// Stop watching a single index, aborting its consumer task and releasing
    /// the OS watch. No-op when the index is not being watched.
    ///
    /// Why: `DELETE /indexes/:id` must not leave a watcher firing into a
    /// dropped indexer.
    /// What: removes the entry and calls `WatcherTask::stop`. Returns `true`
    /// when a watcher was actually stopped.
    /// Test: `stop_for_index_removes_entry`.
    pub async fn stop_for_index(&self, id: &IndexId) -> bool {
        let task = self.inner.lock().await.remove(id);
        match task {
            Some(task) => {
                task.stop();
                tracing::debug!(index_id = %id, "file watcher stopped");
                true
            }
            None => false,
        }
    }

    /// Stop every watcher, returning the number stopped.
    ///
    /// Why: the daemon's graceful-shutdown path (`run_daemon`, after the axum
    /// server drains) calls this so the OS watches and consumer tasks are gone
    /// before the process exits — honouring SIGTERM cleanly (issue #534/#1621).
    /// What: drains the map and calls `stop` on each `WatcherTask`.
    /// Test: `stop_all_clears_all`.
    pub async fn stop_all(&self) -> usize {
        let mut guard = self.inner.lock().await;
        let count = guard.len();
        for (_id, task) in guard.drain() {
            task.stop();
        }
        if count > 0 {
            tracing::info!("stopped {count} file watcher(s) on shutdown");
        }
        count
    }

    /// Number of indexes currently being watched.
    ///
    /// Why: surfaced for tests and potential `/health` reporting.
    /// What: returns the map length under the lock.
    /// Test: used throughout the module tests.
    pub async fn watched_count(&self) -> usize {
        self.inner.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
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
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn save_triggers_incremental_index_via_manager() {
        use std::time::Duration;

        let dir = tempfile::tempdir().expect("tempdir");
        let handle = handle_for("live", dir.path());
        let mgr = WatcherManager::new();
        mgr.spawn_for_index(&handle).await;

        // Allow the OS watcher to install.
        tokio::time::sleep(Duration::from_millis(150)).await;

        tokio::fs::write(dir.path().join("lib.rs"), "fn alpha() {}\nfn beta() {}\n")
            .await
            .expect("write file");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if handle.indexer.read().await.chunk_count() > 0 {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("chunk_count never grew — watcher did not index the save");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        mgr.stop_all().await;
    }
}
