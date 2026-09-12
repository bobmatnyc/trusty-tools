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
//! What: a `Mutex<HashMap<IndexId, Vec<RootWatch>>>` plus `spawn_for_index`,
//! `stop_for_index`, and `stop_all`. Each watcher shares the same
//! `Arc<RwLock<CodeIndexer>>` as the handle so incremental `index_file` /
//! `remove_chunk` calls land in the live index. A fresh `IndexedFiles` tracker
//! is created per index — SHARED by every root's watch — so deletions can
//! locate the chunk IDs to evict.
//!
//! #7434 — one watch per INDEX ROOT: an index can now cover several directory
//! trees ([`crate::core::index_roots`]), and the map used to hold exactly one
//! task and one degraded string per index. That gave a multi-root index
//! incremental indexing for its primary root only, with `watcher.active: true`
//! reported regardless — a tree silently going stale behind a healthy status.
//! Each root now gets its own task and its own
//! [`crate::service::watch_roots::RootWatchState`], so one root's spawn failure
//! leaves the others running and is a recorded fact rather than a `warn!` line.
//! `spawn_for_index` is a RESYNC against the handle's current root table, which
//! is what lets `POST /indexes/:id/roots` start one more watch and a relocate
//! swap the primary's watch while keeping the rest.
//!
//! Test: unit tests at the bottom of this module cover idempotent spawn, the
//! `TRUSTY_DISABLE_WATCHER` opt-out, stop-for-index, stop-all, and the #7434
//! per-root behaviours.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::core::registry::{IndexHandle, IndexId};
use crate::service::indexed_files::IndexedFiles;
use crate::service::network_fs::{classify_root, MountKind};
use crate::service::watch_loop::{spawn_watch_loop_for_root, WatcherTask};
// #7434: one watch per index root, each with its own state.
use crate::service::watch_roots::{RootWatchReport, RootWatchState, WatchedRoot};

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
/// What: maps `IndexId` → one [`RootWatch`] per index root (#7434). Dropping or
/// `stop`-ing a `WatcherTask` aborts its consumer task and releases the OS
/// watch.
/// Test: see module tests.
///
/// #7434 re-key: the map used to hold ONE `WatcherTask` and one degraded string
/// per index, which is why a multi-root index got incremental indexing for its
/// primary root only. Holding a vector means each root's outcome — watching,
/// network-degraded, or a spawn failure — is recorded independently, so one
/// root's failure can never take another root's watch down with it, and
/// `GET /indexes/:id/status` can name which tree stopped updating.
#[derive(Clone, Default)]
pub struct WatcherManager {
    inner: Arc<Mutex<HashMap<IndexId, Vec<RootWatch>>>>,
}

/// One index root's watch slot inside the manager (#7434).
///
/// Why: the per-root state and the task that produces it have to live together
/// or a stop can leave a state saying `watching` with nothing behind it.
/// What: `root` is the CONFIGURED spelling, which is this entry's identity in
/// the index's root table — a resync compares against it to decide what to
/// stop and what to start. `task` is `Some` only when `state` is
/// [`RootWatchState::Watching`].
/// Test: see module tests.
struct RootWatch {
    root: std::path::PathBuf,
    slot: Option<usize>,
    state: RootWatchState,
    task: Option<WatcherTask>,
    /// The chunk tracker every root of this index shares. Carried on each entry
    /// so a later resync (an added root) finds it without a live watch to read
    /// it off — a wholly-degraded index still owns its bookkeeping.
    tracker: IndexedFiles,
}

impl WatcherManager {
    /// Construct an empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start watching EVERY root of `handle` — `root_path` plus each of
    /// `additional_roots` — forwarding changes into the handle's indexer.
    /// Idempotent and opt-out aware.
    ///
    /// Why: called from every index-registration path (warm-boot restore,
    /// `POST /indexes`, `POST /indexes/:id/roots`, relocate) so a freshly
    /// registered or re-rooted index becomes self-updating without a manual
    /// reindex. Before #7434 it watched `root_path` alone, so an index that
    /// covered several trees updated one of them incrementally and reported a
    /// live watcher regardless.
    /// What: resyncs this index's watches against the handle's CURRENT root
    /// table — starts a watch for every root that has none, stops any watch
    /// whose root has left the table, and leaves a live watch alone. That last
    /// property is what makes it idempotent and what makes a relocate keep the
    /// additional roots' watches while the primary's restarts. A
    /// `spawn_watch_loop` failure for one root is RECORDED against that root
    /// and leaves the others running; registration never fails because of the
    /// watcher.
    /// Test: `spawn_is_idempotent`, `every_index_root_is_watched`,
    /// `one_root_spawn_failure_leaves_other_roots_watched_and_is_reported`,
    /// `disable_env_gate_only_matches_one`,
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
        // function (`sync_roots_with`) so tests can inject
        // `MountKind::Network` / `MountKind::Local` directly rather than
        // requiring a real NFS/CIFS mount in CI. Production always resolves
        // the real kind here via `classify_root`.
        //
        // #7434: resolved PER ROOT. An index whose primary root is local and
        // whose second root is an NFS mount must watch the first and refuse the
        // second, which a single index-wide classification cannot express.
        self.sync_roots_with(handle, classify_root).await;
    }

    /// Same as [`Self::spawn_for_index`], but with the network-mount
    /// classification passed in explicitly instead of resolved via `statfs`.
    ///
    /// Why: isolates the one platform-dependent decision
    /// (`network_fs::classify_root`) from the rest of the spawn logic so unit
    /// tests can pin the network-degraded behaviour and the normal-spawn
    /// behaviour deterministically, without needing a real network mount in
    /// CI (issue #3408).
    /// What: applies `mount_kind` to EVERY root of the handle and delegates to
    /// [`Self::sync_roots_with`].
    /// Test: `network_mount_root_is_refused_and_reported`,
    /// `local_root_is_unaffected_by_network_check`.
    ///
    /// `pub(crate)` rather than private: `server::tests_health` also injects a
    /// `MountKind` directly to cover the `/health` JSON surface end-to-end
    /// without a real network mount. Not part of the public API.
    ///
    /// `#[cfg(test)]` since #7434: production now classifies PER ROOT inside
    /// [`Self::sync_roots_with`] (an index whose primary root is local and whose
    /// second is an NFS mount must watch one and refuse the other, which one
    /// index-wide `MountKind` cannot express), so `spawn_for_index` no longer
    /// routes through this and it has exactly two callers, both tests.
    #[cfg(test)]
    pub(crate) async fn spawn_for_index_with_mount_kind(
        &self,
        handle: &Arc<IndexHandle>,
        mount_kind: MountKind,
    ) {
        self.sync_roots_with(handle, move |_| mount_kind).await;
    }

    /// Bring this index's watches into line with `handle`'s root table (#7434).
    ///
    /// Why: four callers need this and they need it to mean the same thing —
    /// registration (nothing watched yet), a repeat registration (leave it
    /// alone), an added root (start one more), and a relocate (swap the
    /// primary, keep the rest). Expressing all four as "make reality match the
    /// table" is what keeps a relocate from tearing down watches it should have
    /// kept.
    /// What: (1) stops and drops every entry whose root is no longer in the
    /// table; (2) for every table root without a LIVE watch, classifies the
    /// mount, then either records the #3408 refusal or builds a watch with
    /// `spawn_watch_loop_for_root`; (3) records a build failure as
    /// [`RootWatchState::Failed`] for that root alone. Every task is built
    /// BEFORE the lock is taken (issue #1640): the `Mutex` is never held across
    /// the potentially-blocking OS-watch installation, so `stop_for_index` /
    /// `stop_all` can never block on a spawn.
    ///
    /// A root already in `Failed` or `Degraded` is retried on the next sync.
    /// That is deliberate: an added root whose tree was briefly unreadable
    /// should start watching when the next registration event comes round,
    /// rather than staying dead until the daemon restarts.
    ///
    /// The `IndexedFiles` tracker is created once and SHARED by every root's
    /// watch. Its keys are whole-corpus keys (`@root<n>/…`), so one tracker is
    /// the only shape in which the dropped-event reconcile can see every root's
    /// files — see `watch_rescan::reconcile_after_rescan_roots`.
    /// Test: `every_index_root_is_watched`,
    /// `one_root_spawn_failure_leaves_other_roots_watched_and_is_reported`,
    /// `spawn_is_idempotent`, `relocate_restarts_the_primary_and_keeps_the_rest`.
    async fn sync_roots_with<F>(&self, handle: &Arc<IndexHandle>, classify: F)
    where
        F: Fn(&std::path::Path) -> MountKind,
    {
        let table = WatchedRoot::table(&handle.root_path, &handle.additional_roots);

        // Drop watches for roots that have left the table (a relocate's old
        // primary). Done first so the OS watch is released before a new one is
        // installed on a possibly-overlapping tree.
        let stale: Vec<RootWatch> = {
            let mut guard = self.inner.lock().await;
            let entries = guard.entry(handle.id.clone()).or_default();
            let keep: Vec<std::path::PathBuf> =
                table.iter().map(|r| r.raw().to_path_buf()).collect();
            let mut removed = Vec::new();
            let mut i = 0;
            while i < entries.len() {
                if keep.contains(&entries[i].root) {
                    i += 1;
                } else {
                    removed.push(entries.remove(i));
                }
            }
            removed
        };
        for entry in stale {
            if let Some(task) = entry.task {
                task.stop().await;
                tracing::info!(
                    index_id = %handle.id,
                    root = %entry.root.display(),
                    "file watcher stopped — root left this index's table (#7434)",
                );
            }
        }

        // The shared tracker: reused when this index already has watches so a
        // newly-added root's watch sees the chunks the existing roots recorded.
        let indexed_files = self.tracker_for(&handle.id).await;

        for root in &table {
            if self.is_watching_root(&handle.id, root.raw()).await {
                continue;
            }
            let built = self.build_root_watch(handle, root, &indexed_files, &classify);
            self.install_root_watch(&handle.id, root, built, &indexed_files)
                .await;
        }
    }

    /// Build one root's watch, or the state that says why there is none.
    ///
    /// Why: separated from [`Self::sync_roots_with`] so the #3408 refusal, the
    /// spawn, and the failure record read as one three-armed decision rather
    /// than as control flow interleaved with lock handling.
    /// What: `Degraded` for a network mount (checked BEFORE `spawn_watch_loop`
    /// so the recursive OS-watch install is never paid for a mount that cannot
    /// work); `Failed` with the rendered error when the spawn errors;
    /// `Watching` plus the task otherwise. `classify_root` fails open to
    /// `Local`, so the refusal can only ever be a false negative.
    /// Test: `network_mount_root_is_refused_and_reported`,
    /// `one_root_spawn_failure_leaves_other_roots_watched_and_is_reported`.
    fn build_root_watch<F>(
        &self,
        handle: &Arc<IndexHandle>,
        root: &WatchedRoot,
        indexed_files: &IndexedFiles,
        classify: &F,
    ) -> (RootWatchState, Option<WatcherTask>)
    where
        F: Fn(&std::path::Path) -> MountKind,
    {
        if classify(root.raw()).is_network() {
            let reason = network_mount_degraded_reason(&handle.id, root.raw());
            tracing::warn!(
                index_id = %handle.id,
                root = %root.raw().display(),
                "{reason}"
            );
            return (RootWatchState::Degraded { reason }, None);
        }

        match spawn_watch_loop_for_root(
            root.clone(),
            // #7434: every root, so a dropped-event rescan on THIS watch
            // reconciles the whole index rather than sweeping the other roots'
            // chunks out of the corpus.
            WatchedRoot::table(&handle.root_path, &handle.additional_roots),
            // #3049: the watcher takes this index's teardown-lock read side.
            handle.id.clone(),
            Arc::clone(&handle.indexer),
            indexed_files.clone(),
            // #6524: the watcher populates this index's file-change feed.
            Arc::clone(&handle.file_events),
        ) {
            Ok(task) => (RootWatchState::Watching, Some(task)),
            Err(e) => {
                let reason = format!("{e:#}");
                tracing::warn!(
                    index_id = %handle.id,
                    root = %root.raw().display(),
                    "could not start file watcher for this root (incremental indexing disabled \
                     for it; the index's other roots are unaffected): {reason}",
                );
                (RootWatchState::Failed { reason }, None)
            }
        }
    }

    /// Record one root's freshly-built watch, resolving a concurrent insert.
    ///
    /// Why: two concurrent `spawn_for_index` calls for the same index could
    /// both find a root unwatched before either inserted — the TOCTOU window
    /// #1640 closed for the single-root map, re-asked per root.
    /// What: re-checks under the lock; if a racing sync already installed a
    /// LIVE watch for this root, the task just built is stopped and discarded.
    /// Otherwise the entry replaces any non-live record for the same root.
    /// Test: `spawn_is_idempotent`.
    async fn install_root_watch(
        &self,
        id: &IndexId,
        root: &WatchedRoot,
        built: (RootWatchState, Option<WatcherTask>),
        tracker: &IndexedFiles,
    ) {
        let (state, task) = built;
        let mut guard = self.inner.lock().await;
        let entries = guard.entry(id.clone()).or_default();
        if let Some(existing) = entries.iter().position(|e| e.root == root.raw()) {
            if entries[existing].state.is_watching() {
                drop(guard);
                if let Some(task) = task {
                    task.stop().await;
                }
                return;
            }
            entries.remove(existing);
        }
        let watching = state.is_watching();
        entries.push(RootWatch {
            root: root.raw().to_path_buf(),
            slot: root.slot(),
            state,
            task,
            tracker: tracker.clone(),
        });
        drop(guard);
        if watching {
            tracing::info!(
                index_id = %id,
                root = %root.raw().display(),
                slot = ?root.slot(),
                "file watcher active — saves trigger incremental indexing (issue #1621)",
            );
        }
    }

    /// The `IndexedFiles` tracker this index's watches share (#7434).
    ///
    /// Why: a per-root tracker would split one index's chunk bookkeeping across
    /// N maps, and the dropped-event reconcile walks exactly one of them to
    /// decide what has been deleted. One tracker per index keeps that decision
    /// whole.
    /// What: clones the tracker off any existing entry — `IndexedFiles` is an
    /// `Arc` inside, so the clone is the same map — or makes a fresh one for an
    /// index with no watches yet.
    /// Test: `add_root_spawns_a_watch_for_the_new_root`.
    async fn tracker_for(&self, id: &IndexId) -> IndexedFiles {
        let guard = self.inner.lock().await;
        guard
            .get(id)
            .and_then(|entries| entries.first().map(|e| e.tracker.clone()))
            .unwrap_or_default()
    }

    /// Whether one specific root of `id` currently has a live watch.
    async fn is_watching_root(&self, id: &IndexId, root: &std::path::Path) -> bool {
        self.inner.lock().await.get(id).is_some_and(|entries| {
            entries
                .iter()
                .any(|e| e.root == root && e.state.is_watching())
        })
    }

    /// Stop watching a single index, aborting its consumer task and releasing
    /// the OS watch. No-op when the index is not being watched.
    ///
    /// Why: `DELETE /indexes/:id` must not leave a watcher firing into a
    /// dropped indexer — and, since #3049, must not return while the watcher
    /// still HOLDS that indexer. The watcher's `Arc<RwLock<CodeIndexer>>` keeps
    /// the index's redb corpus open, and redb is single-open, so a recreate
    /// under the same id fails to open the corpus until this returns.
    /// What: removes EVERY root's entry for this index (#7434) and awaits
    /// `WatcherTask::stop` on each, which aborts the consumer task and waits
    /// for it to terminate. Returns `true` when at least one live watcher was
    /// stopped. Removing the whole vector also clears any #3408 degraded or
    /// #7434 failed record, so `/health` cannot keep reporting a degraded root
    /// of an index that no longer exists.
    /// Test: `stop_for_index_removes_entry`, `stop_for_index_stops_every_root_watch`,
    /// `stop_for_index_releases_the_indexer_before_it_returns`,
    /// `stop_for_index_clears_network_degraded_entry`.
    pub async fn stop_for_index(&self, id: &IndexId) -> bool {
        let entries = self.inner.lock().await.remove(id).unwrap_or_default();
        let mut stopped = false;
        for entry in entries {
            if let Some(task) = entry.task {
                task.stop().await;
                stopped = true;
                tracing::debug!(
                    index_id = %id,
                    root = %entry.root.display(),
                    "file watcher stopped",
                );
            }
        }
        stopped
    }

    /// Stop every watcher, returning the number stopped.
    ///
    /// Why: the daemon's graceful-shutdown path (`run_daemon`, after the axum
    /// server drains) calls this so the OS watches and consumer tasks are gone
    /// before the process exits — honouring SIGTERM cleanly (issue #534/#1621).
    /// What: drains the map and awaits `stop` on each `WatcherTask`, so every
    /// consumer task has terminated — and released its indexer `Arc` — before
    /// the process exits. Drained OUT of the map first so the lock is not held
    /// across the waits.
    /// Test: `stop_all_clears_all`.
    pub async fn stop_all(&self) -> usize {
        let tasks: Vec<WatcherTask> = {
            let mut guard = self.inner.lock().await;
            // #7434: every ROOT's task, across every index.
            guard
                .drain()
                .flat_map(|(_id, entries)| entries.into_iter().filter_map(|e| e.task))
                .collect()
        };
        let count = tasks.len();
        for task in tasks {
            task.stop().await;
        }
        if count > 0 {
            tracing::info!("stopped {count} file watcher(s) on shutdown");
        }
        count
    }

    /// Number of INDEXES with at least one live root watch.
    ///
    /// Why: surfaced for tests and potential `/health` reporting. Counting
    /// indexes rather than roots is what keeps this number meaning the same
    /// thing it did before #7434 — "how many indexes are self-updating" — for
    /// every single-root index, which is nearly all of them.
    /// What: counts map entries holding at least one [`RootWatchState::Watching`]
    /// root. Use [`Self::watched_root_count`] for the per-root figure.
    /// Test: used throughout the module tests.
    pub async fn watched_count(&self) -> usize {
        self.inner
            .lock()
            .await
            .values()
            .filter(|entries| entries.iter().any(|e| e.state.is_watching()))
            .count()
    }

    /// Number of live ROOT watches across every index (#7434).
    ///
    /// Why: `watched_count` cannot distinguish a three-root index watching all
    /// three from one watching only its primary — the exact condition this
    /// slice exists to make visible.
    /// What: counts entries in [`RootWatchState::Watching`].
    /// Test: `every_index_root_is_watched`,
    /// `one_root_spawn_failure_leaves_other_roots_watched_and_is_reported`.
    pub async fn watched_root_count(&self) -> usize {
        self.inner
            .lock()
            .await
            .values()
            .map(|entries| entries.iter().filter(|e| e.state.is_watching()).count())
            .sum()
    }

    /// Every root's watch state for one index, in root-table order (#7434).
    ///
    /// Why: `GET /indexes/:id/status` has to name WHICH tree stopped updating;
    /// `active` plus one reason string cannot say that for a multi-root index.
    /// What: one [`RootWatchReport`] per known root, primary first. Empty for
    /// an index with no watches recorded at all (never registered, stopped, or
    /// `TRUSTY_DISABLE_WATCHER=1`), which the status endpoint reports as an
    /// empty array rather than inventing rows.
    /// Test: `server::tests_7434_watch::status_reports_every_root_watch_state`.
    pub async fn root_watch_states(&self, id: &IndexId) -> Vec<RootWatchReport> {
        let guard = self.inner.lock().await;
        let Some(entries) = guard.get(id) else {
            return Vec::new();
        };
        let mut rows: Vec<&RootWatch> = entries.iter().collect();
        // Primary (slot `None`) first, then additional slots in table order.
        rows.sort_by_key(|e| e.slot.map_or(0, |n| n + 1));
        rows.into_iter()
            .map(|e| RootWatchReport {
                root: e.root.display().to_string(),
                slot: e.slot,
                primary: e.slot.is_none(),
                state: e.state.label(),
                reason: e.state.reason().map(str::to_string),
            })
            .collect()
    }

    /// Whether a specific index currently has a live watcher.
    ///
    /// Why: the query path uses this to detect an index being served without a
    /// watcher — either idle-suspended (`server::tickers`) or lazily restored
    /// from the cold store — so it can resume watching and reconcile on wake.
    /// What: `true` when at least one root has a live watch (#7434). A
    /// PARTIALLY watched index answers `true` here on purpose: the wake-up
    /// path's job is to restart a suspended index, and `spawn_for_index` is
    /// idempotent per root, so the unwatched roots are picked up by the next
    /// registration event rather than by a full teardown. Which roots are
    /// unwatched is what [`Self::root_watch_states`] answers.
    /// Test: `is_watching_reflects_spawn_and_stop`.
    pub async fn is_watching(&self, id: &IndexId) -> bool {
        self.inner
            .lock()
            .await
            .get(id)
            .is_some_and(|entries| entries.iter().any(|e| e.state.is_watching()))
    }

    /// Number of indexes whose watcher was refused because their root was
    /// detected as network-mounted (issue #3408).
    ///
    /// Why: surfaced on `GET /health` as `indexes_watcher_network_degraded` so
    /// operators/monitors can detect the condition without tailing logs or
    /// polling every index's status individually.
    /// What: counts INDEXES holding at least one network-degraded ROOT (#7434),
    /// which is the same number this returned before #7434 for every
    /// single-root index. The `/health` field it feeds is a count of affected
    /// indexes, not of affected trees.
    /// Test: `network_mount_root_is_refused_and_reported`.
    pub async fn network_degraded_count(&self) -> usize {
        self.inner
            .lock()
            .await
            .values()
            .filter(|entries| entries.iter().any(|e| e.state.is_network_degraded()))
            .count()
    }

    /// The actionable degraded-reason message for a specific index, if its
    /// watcher was refused due to a network-mounted root.
    ///
    /// Why: `GET /indexes/:id/status` surfaces this so an operator looking at
    /// one index gets the full actionable message, not just a boolean.
    /// What: the FIRST network-degraded root's reason in root-table order
    /// (#7434), `None` otherwise — including for unknown ids. For a single-root
    /// index this is exactly what it returned before #7434; for a multi-root
    /// one, `watcher.roots` in the status response is the per-tree answer and
    /// this stays the index-level summary the pre-existing field promised.
    /// Test: `network_mount_root_is_refused_and_reported`.
    pub async fn network_degraded_reason(&self, id: &IndexId) -> Option<String> {
        let guard = self.inner.lock().await;
        let entries = guard.get(id)?;
        let mut degraded: Vec<&RootWatch> = entries
            .iter()
            .filter(|e| e.state.is_network_degraded())
            .collect();
        degraded.sort_by_key(|e| e.slot.map_or(0, |n| n + 1));
        degraded
            .first()
            .and_then(|e| e.state.reason().map(str::to_string))
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

    /// #7434: the same handle, covering `additional` trees beyond `primary`.
    fn multi_root_handle(
        id: &str,
        primary: &std::path::Path,
        additional: Vec<std::path::PathBuf>,
    ) -> Arc<IndexHandle> {
        let indexer = Arc::new(RwLock::new(CodeIndexer::new(id, primary)));
        let mut handle = IndexHandle::bare(IndexId::new(id), indexer, primary.to_path_buf());
        handle.additional_roots = additional;
        Arc::new(handle)
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
                    std::fs::write(&file, format!("fn busy{generation}() {{}}\n"))
                        .expect("write file");
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

    // ── #7434: one watch per index root ────────────────────────────────────

    /// Why: the core acceptance criterion of this slice. Before #7434 the
    /// manager keyed ONE `WatcherTask` per index and spawned it on
    /// `handle.root_path` alone, so an index covering three trees watched one
    /// and reported `watcher.active: true` — the other two went stale between
    /// reindexes with nothing saying so.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_index_root_is_watched() {
        let primary = tempfile::tempdir().expect("tempdir primary");
        let second = tempfile::tempdir().expect("tempdir second");
        let third = tempfile::tempdir().expect("tempdir third");
        let handle = multi_root_handle(
            "multi-watch",
            primary.path(),
            vec![second.path().to_path_buf(), third.path().to_path_buf()],
        );
        let mgr = WatcherManager::new();

        mgr.spawn_for_index(&handle).await;

        assert_eq!(
            mgr.watched_root_count().await,
            3,
            "every root of a three-root index must have its own live watch"
        );
        let rows = mgr.root_watch_states(&handle.id).await;
        assert_eq!(rows.len(), 3, "one report row per root, got {rows:?}");
        assert!(
            rows.iter().all(|r| r.state == "watching"),
            "every root must report `watching`, got {rows:?}"
        );
        assert!(rows[0].primary && rows[0].slot.is_none(), "primary first");
        assert_eq!(rows[1].slot, Some(0), "additional roots in table order");
        assert_eq!(rows[2].slot, Some(1));

        mgr.stop_all().await;
    }

    /// Why: the pre-#7434 spawn answered a `spawn_watch_loop` error with
    /// `warn!` and a bare `return`, which for a multi-root index would mean one
    /// unreadable tree silently costing the index every OTHER root's watch —
    /// and leaving no record of which root failed. Both halves matter: the
    /// survivors keep watching, and the failure is a readable fact.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_root_spawn_failure_leaves_other_roots_watched_and_is_reported() {
        let primary = tempfile::tempdir().expect("tempdir primary");
        let third = tempfile::tempdir().expect("tempdir third");
        // A path that does not exist: `notify`'s `watch()` refuses it, which is
        // the real production failure (a root unmounted or deleted between
        // registration and the spawn).
        let missing = primary.path().join("gone");
        let handle = multi_root_handle(
            "partial-watch",
            primary.path(),
            vec![missing.clone(), third.path().to_path_buf()],
        );
        let mgr = WatcherManager::new();

        mgr.spawn_for_index(&handle).await;

        assert_eq!(
            mgr.watched_root_count().await,
            2,
            "the two readable roots must still be watched"
        );
        assert!(
            mgr.is_watching(&handle.id).await,
            "the index is still partially self-updating"
        );

        let rows = mgr.root_watch_states(&handle.id).await;
        let failed: Vec<_> = rows.iter().filter(|r| r.state == "failed").collect();
        assert_eq!(
            failed.len(),
            1,
            "exactly the one unspawnable root must be recorded as failed, got {rows:?}"
        );
        assert_eq!(failed[0].slot, Some(0), "and it must name WHICH root");
        let reason = failed[0]
            .reason
            .as_deref()
            .expect("a failed root must carry the reason, not just a log line");
        assert!(
            reason.contains(&missing.display().to_string()),
            "the reason must name the root that could not be watched, got {reason:?}"
        );
        assert!(
            !mgr.network_degraded_reason(&handle.id)
                .await
                .is_some_and(|_| true),
            "a spawn failure is not the #3408 network-mount refusal"
        );

        mgr.stop_all().await;
    }

    /// Why: a save under an additional root must update THAT root's
    /// `@root<n>/…` chunks. Keyed against the primary root instead, the edit
    /// would either create a second set of chunks under a bare name that
    /// decodes to a file in the wrong tree, or overwrite a same-named primary
    /// file's chunks outright.
    /// Test: this test. Drives the real manager, the real OS watcher and the
    /// real admission path — `await_watch_condition` re-applies the save until
    /// the index reacts, the same way every other watcher test in this crate
    /// absorbs a dropped FSEvents batch (#4731).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn modified_file_under_additional_root_updates_its_root_relative_chunks() {
        let primary = tempfile::tempdir().expect("tempdir primary");
        let extra = tempfile::tempdir().expect("tempdir extra");
        let handle = multi_root_handle(
            "root-relative-watch",
            primary.path(),
            vec![extra.path().to_path_buf()],
        );
        let mgr = WatcherManager::new();
        mgr.spawn_for_index(&handle).await;

        let file = extra.path().join("extra.rs");
        let indexed = {
            let probe = Arc::clone(&handle);
            crate::service::watch_test_support::await_watch_condition(
                |generation| {
                    std::fs::write(&file, format!("fn extra{generation}() {{}}\n"))
                        .expect("write file");
                },
                move || {
                    let probe = Arc::clone(&probe);
                    async move {
                        !probe
                            .indexer
                            .read()
                            .await
                            .chunk_ids_for_file("@root1/extra.rs")
                            .await
                            .is_empty()
                    }
                },
            )
            .await
        };

        let stored: Vec<String> = handle
            .indexer
            .read()
            .await
            .all_chunks()
            .await
            .into_iter()
            .filter_map(|c| c.path)
            .collect();
        assert!(
            indexed,
            "no chunk was stored under `@root1/extra.rs` — the additional root's \
             save was either not watched or keyed against the primary root; \
             stored paths: {stored:?}"
        );
        assert!(
            !stored.iter().any(|p| p == "extra.rs"),
            "the file must NOT also be stored under a bare primary-root key: {stored:?}"
        );

        mgr.stop_all().await;
    }

    /// Why: `DELETE /indexes/:id` must not leave ANY of a multi-root index's
    /// watchers firing into a dropped indexer. A stop that removed one entry
    /// would leave N-1 tasks holding the index's `Arc<RwLock<CodeIndexer>>` and
    /// therefore its open redb corpus, which is exactly the #3049 recreate
    /// failure, once per surviving root.
    /// Test: this test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_for_index_stops_every_root_watch() {
        let primary = tempfile::tempdir().expect("tempdir primary");
        let second = tempfile::tempdir().expect("tempdir second");
        let third = tempfile::tempdir().expect("tempdir third");
        let handle = multi_root_handle(
            "stop-every-root",
            primary.path(),
            vec![second.path().to_path_buf(), third.path().to_path_buf()],
        );
        let mgr = WatcherManager::new();
        mgr.spawn_for_index(&handle).await;
        assert_eq!(mgr.watched_root_count().await, 3);

        assert!(
            mgr.stop_for_index(&handle.id).await,
            "stop_for_index must report it stopped watchers"
        );

        assert_eq!(
            mgr.watched_root_count().await,
            0,
            "every root's watch must be stopped, not just the first"
        );
        assert!(!mgr.is_watching(&handle.id).await);
        assert!(
            mgr.root_watch_states(&handle.id).await.is_empty(),
            "and no per-root record may survive the stop"
        );
        assert_eq!(
            mgr.stop_all().await,
            0,
            "stop_all must find nothing left to stop"
        );
    }

    /// Why: a relocate moves the PRIMARY root only. Restarting every watch
    /// would drop the additional roots' OS watches for no reason (re-installing
    /// a recursive watch is the expensive half of the call); not restarting the
    /// primary's would leave it watching a tree the index no longer covers.
    /// Test: this test drives the manager's resync directly — the handler-level
    /// path is `indexes_relocate`'s `spawn_for_index` call.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn relocate_restarts_the_primary_and_keeps_the_rest() {
        let old_primary = tempfile::tempdir().expect("tempdir old");
        let new_primary = tempfile::tempdir().expect("tempdir new");
        let extra = tempfile::tempdir().expect("tempdir extra");
        let mgr = WatcherManager::new();

        let before = multi_root_handle(
            "relocate-watch",
            old_primary.path(),
            vec![extra.path().to_path_buf()],
        );
        mgr.spawn_for_index(&before).await;
        assert_eq!(mgr.watched_root_count().await, 2);

        // The replacement handle a relocate registers: same id, new primary,
        // same additional roots.
        let after = multi_root_handle(
            "relocate-watch",
            new_primary.path(),
            vec![extra.path().to_path_buf()],
        );
        mgr.spawn_for_index(&after).await;

        let rows = mgr.root_watch_states(&after.id).await;
        assert_eq!(
            rows.len(),
            2,
            "the old primary's entry must be gone, not accumulated: {rows:?}"
        );
        assert_eq!(
            rows[0].root,
            new_primary.path().display().to_string(),
            "the primary watch must have moved to the new root"
        );
        assert_eq!(
            rows[1].root,
            extra.path().display().to_string(),
            "the additional root's watch must be kept"
        );
        assert!(rows.iter().all(|r| r.state == "watching"));

        mgr.stop_all().await;
    }
}
