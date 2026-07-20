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
use crate::service::network_fs::{classify_root, MountKind};
use crate::service::watch_loop::{spawn_watch_loop, WatcherTask};

/// Human-readable, actionable message logged (and surfaced via `/health` +
/// `GET /indexes/:id/status`) when an index root is detected as
/// network-mounted (issue #3408).
///
/// Why: inotify/FSEvents cannot observe writes made by another host onto a
/// shared network mount — an OS-level limitation, not something retrying or
/// reconfiguring the watcher can fix. Starting the watcher anyway would leave
/// the daemon reporting healthy while silently never reacting to cross-host
/// changes. This message names the supported alternative so an operator
/// reading logs or `/health` knows exactly what to do next.
fn network_mount_degraded_reason(index_id: &IndexId, root_path: &std::path::Path) -> String {
    format!(
        "index {index_id} root {root} is on a network-mounted filesystem (NFS/EFS/SMB/CIFS); \
         the file watcher cannot reliably observe changes written by other hosts on a network \
         mount, so it has been disabled for this index rather than silently never firing. \
         Drive incremental updates explicitly via `POST /indexes/{index_id}/index-file` and \
         `POST /indexes/{index_id}/remove-file` instead (see the operator docs for the \
         supported network-mount pattern).",
        root = root_path.display(),
    )
}

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
    /// Indexes whose watcher was refused because `root_path` was detected as
    /// network-mounted (issue #3408), keyed to the actionable message logged
    /// at spawn time. Surfaced via `/health`
    /// (`indexes_watcher_network_degraded`) and
    /// `GET /indexes/:id/status` so the condition is visible without tailing
    /// logs — the whole point of this feature is that the daemon must NOT
    /// look silently healthy on a network mount.
    network_degraded: Arc<Mutex<HashMap<IndexId, String>>>,
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
    /// Test: `spawn_is_idempotent`, `disable_env_gate_only_matches_one`,
    /// `network_mount_root_is_refused_and_reported`,
    /// `local_root_is_unaffected_by_network_check`.
    pub async fn spawn_for_index(&self, handle: &Arc<IndexHandle>) {
        if watcher_disabled() {
            tracing::debug!(
                index_id = %handle.id,
                "file watcher disabled via {DISABLE_WATCHER_ENV}=1 — not watching",
            );
            return;
        }

        // Issue #3408: the mount-kind check is factored into a testable inner
        // function (`spawn_for_index_with_mount_kind`) so tests can inject
        // `MountKind::Network` / `MountKind::Local` directly rather than
        // requiring a real NFS/CIFS mount in CI. Production always resolves
        // the real kind here via `classify_root`.
        self.spawn_for_index_with_mount_kind(handle, classify_root(&handle.root_path))
            .await;
    }

    /// Same as [`Self::spawn_for_index`], but with the network-mount
    /// classification passed in explicitly instead of resolved via `statfs`.
    ///
    /// Why: isolates the one platform-dependent decision
    /// (`network_fs::classify_root`) from the rest of the spawn logic so unit
    /// tests can pin the network-degraded behaviour and the normal-spawn
    /// behaviour deterministically, without needing a real network mount in
    /// CI (issue #3408).
    /// What: (1) refuses to spawn and records the actionable degraded reason
    /// when `mount_kind` is `Network`; (2) otherwise proceeds with the
    /// existing idempotent build-then-insert spawn path unchanged.
    /// Test: `network_mount_root_is_refused_and_reported`,
    /// `local_root_is_unaffected_by_network_check`.
    ///
    /// `pub(crate)` rather than private: `server::tests_health` also injects a
    /// `MountKind` directly to cover the `/health` JSON surface end-to-end
    /// without a real network mount. Not part of the public API.
    pub(crate) async fn spawn_for_index_with_mount_kind(
        &self,
        handle: &Arc<IndexHandle>,
        mount_kind: MountKind,
    ) {
        // Refuse to start a watcher whose root is positively identified as
        // network-mounted (EFS/NFS/SMB/CIFS). inotify/FSEvents cannot observe
        // another host's writes to a shared network mount — starting the
        // watcher anyway would leave the daemon reporting healthy while
        // silently never reacting to cross-host changes. This check runs
        // BEFORE `spawn_watch_loop` so we never pay the OS-watch install cost
        // (recursive inotify walk) for a mount that can't work anyway.
        // `classify_root` fails open to `Local` on any detection error, so
        // this can only ever produce a false negative, never a false
        // positive that blocks a legitimate local watcher.
        if mount_kind.is_network() {
            let reason = network_mount_degraded_reason(&handle.id, &handle.root_path);
            tracing::warn!(
                index_id = %handle.id,
                root = %handle.root_path.display(),
                "{reason}"
            );
            self.network_degraded
                .lock()
                .await
                .insert(handle.id.clone(), reason);
            return;
        }
        // Clear any stale degraded entry from a previous root-path change for
        // this id (defensive — ids are not normally re-registered with a
        // different root, but this keeps the map from lying if they are).
        self.network_degraded.lock().await.remove(&handle.id);

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
        // Issue #3408: `DELETE /indexes/:id` should not leave a stale
        // network-mount-degraded entry behind for an id that no longer exists.
        self.network_degraded.lock().await.remove(id);
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

    /// Whether a specific index currently has a live watcher.
    ///
    /// Why: the query path uses this to detect an index being served without a
    /// watcher — either idle-suspended (`server::tickers`) or lazily restored
    /// from the cold store — so it can resume watching and reconcile on wake.
    /// What: `contains_key` under the lock.
    /// Test: `is_watching_reflects_spawn_and_stop`.
    pub async fn is_watching(&self, id: &IndexId) -> bool {
        self.inner.lock().await.contains_key(id)
    }

    /// Number of indexes whose watcher was refused because their root was
    /// detected as network-mounted (issue #3408).
    ///
    /// Why: surfaced on `GET /health` as `indexes_watcher_network_degraded` so
    /// operators/monitors can detect the condition without tailing logs or
    /// polling every index's status individually.
    /// What: length of the `network_degraded` map under the lock.
    /// Test: `network_mount_root_is_refused_and_reported`.
    pub async fn network_degraded_count(&self) -> usize {
        self.network_degraded.lock().await.len()
    }

    /// The actionable degraded-reason message for a specific index, if its
    /// watcher was refused due to a network-mounted root.
    ///
    /// Why: `GET /indexes/:id/status` surfaces this so an operator looking at
    /// one index gets the full actionable message, not just a boolean.
    /// What: `Some(reason)` when `id` is in the `network_degraded` map,
    /// `None` otherwise (including for unknown ids).
    /// Test: `network_mount_root_is_refused_and_reported`.
    pub async fn network_degraded_reason(&self, id: &IndexId) -> Option<String> {
        self.network_degraded.lock().await.get(id).cloned()
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
}
