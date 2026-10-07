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
//! What: a `Mutex<HashMap<IndexId, IndexWatches>>` plus `spawn_for_index`,
//! `stop_for_index`, and `stop_all`. Each watcher shares the same
//! `Arc<RwLock<CodeIndexer>>` as the handle so incremental `index_file` /
//! `remove_chunk_ids_committed` calls land in the live index. One
//! `IndexedFiles` tracker per index, shared by every root's watch, lets
//! deletions locate the chunk IDs to evict.
//!
//! #7434 — one watch per INDEX ROOT. Each root of a multi-root index gets its
//! own task and [`RootWatchState`], so one root's failed or stuck start (#9339)
//! leaves the others watching and is a recorded fact on
//! `GET /indexes/:id/status`. `spawn_for_index` is a resync against the
//! handle's current root table.
//!
//! Test: `watcher_manager_tests.rs` (idempotent spawn, the
//! `TRUSTY_DISABLE_WATCHER` opt-out, stop-for-index, stop-all) and
//! `watcher_roots_7434_tests.rs` (the per-root behaviours).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::core::registry::{IndexHandle, IndexId};
use crate::service::indexed_files::IndexedFiles;
use crate::service::network_fs::{classify_root, MountKind};
use crate::service::watch_loop::{spawn_watch_loop_for_root, WatcherTask};
use crate::service::watch_roots::{RootWatchReport, RootWatchState, WatchedRoot};
use crate::service::watcher_start::StartInFlight;

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
/// taken for brief inserts and removals, never across a start or a stop.
/// What: maps `IndexId` → [`IndexWatches`], one [`RootWatch`] per index root
/// (#7434). Dropping or `stop`-ing a `WatcherTask` aborts its consumer task and
/// releases the OS watch within the #9315 bound.
/// Test: see `watcher_manager_tests.rs`.
#[derive(Clone, Default)]
pub struct WatcherManager {
    inner: Arc<Mutex<HashMap<IndexId, IndexWatches>>>,
    registry: Option<crate::core::registry::IndexRegistry>,
}

/// One index's watches: the shared chunk tracker plus one entry per root.
#[derive(Default)]
struct IndexWatches {
    /// Every root's watch records into this one tracker; its keys are
    /// whole-corpus keys (`@root<n>/…`), so the rescan sees every root.
    tracker: IndexedFiles,
    roots: Vec<RootWatch>,
}

/// One root's watch slot (#7434). `task` is `Some` only while `state` is
/// [`RootWatchState::Watching`]; `root` is the configured spelling, the
/// entry's identity in the index's root table.
struct RootWatch {
    root: std::path::PathBuf,
    slot: Option<usize>,
    state: RootWatchState,
    task: Option<WatcherTask>,
}

/// The outcome of one root's start attempt.
enum Built {
    /// Watching, degraded or failed — recorded against the root.
    Recorded(RootWatchState, Option<WatcherTask>),
    /// #9339: another start for this root is in flight and decides it.
    InFlight,
}

impl WatcherManager {
    /// Construct an empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind daemon watchers to live policy updates in the shared registry.
    pub(crate) fn with_registry(registry: crate::core::registry::IndexRegistry) -> Self {
        Self {
            registry: Some(registry),
            ..Self::default()
        }
    }

    /// Start watching every root of `handle`, forwarding changes into the
    /// handle's indexer. Idempotent and opt-out aware.
    ///
    /// Why: called from every registration path (warm-boot restore,
    /// `POST /indexes`, `POST /indexes/:id/roots`, the query-time wake) so an
    /// index becomes self-updating without a manual reindex.
    /// What: no-op when `TRUSTY_DISABLE_WATCHER=1`; otherwise a resync of this
    /// index's watches against the handle's root table, classifying each root's
    /// mount with `statfs` (#3408) — see [`Self::sync_roots_with`].
    /// Test: `spawn_is_idempotent`, `disable_env_gate_only_matches_one`,
    /// `every_index_root_is_watched`,
    /// `a_stuck_root_start_does_not_block_the_other_roots`.
    pub async fn spawn_for_index(&self, handle: &Arc<IndexHandle>) {
        if watcher_disabled() {
            tracing::debug!(
                index_id = %handle.id,
                "file watcher disabled via {DISABLE_WATCHER_ENV}=1 — not watching",
            );
            return;
        }
        self.sync_roots_with(handle, classify_root).await;
    }

    /// [`Self::spawn_for_index`] with every root's mount kind injected
    /// (#3408), so tests need no real network mount. Bypasses the opt-out.
    #[cfg(test)]
    pub(crate) async fn spawn_for_index_with_mount_kind(
        &self,
        handle: &Arc<IndexHandle>,
        mount_kind: MountKind,
    ) {
        self.sync_roots_with(handle, move |_| mount_kind).await;
    }

    /// Bring this index's watches in line with `handle`'s root table (#7434).
    ///
    /// Why: registration, a repeat registration, an added root and a resumed
    /// index all need "make the watches match the table".
    /// What: (1) a serve-only index gets none (#8883); (2) stops every watch
    /// whose root left the table; (3) for each root without a live watch,
    /// records the #3408 network refusal or starts a watch through
    /// `spawn_watch_loop_for_root` on the blocking pool — `FileWatcher::start`
    /// bounds each start by its own root (#9339) — with all roots starting
    /// concurrently, so one stuck root delays no other; (4) records each
    /// outcome against its root. A start already in flight records nothing. No
    /// lock is held across a start or a stop (#1640).
    /// Test: `every_index_root_is_watched`,
    /// `a_stuck_root_start_does_not_block_the_other_roots`,
    /// `network_mount_root_is_refused_and_reported`.
    async fn sync_roots_with<F>(&self, handle: &Arc<IndexHandle>, classify: F)
    where
        F: Fn(&std::path::Path) -> MountKind,
    {
        // #8883: a watcher writes every save into the index; a serve-only
        // index must stay the one it was shipped. Debug: search re-asks per query.
        if handle.serve_only {
            tracing::debug!(index_id = %handle.id, "serve-only index: no file watcher (#8883)");
            return;
        }
        let table = WatchedRoot::table(&handle.root_path, &handle.additional_roots);
        let (stale, todo, tracker) = {
            let mut guard = self.inner.lock().await;
            let watches = guard.entry(handle.id.clone()).or_default();
            let (keep, stale): (Vec<RootWatch>, Vec<RootWatch>) =
                std::mem::take(&mut watches.roots)
                    .into_iter()
                    .partition(|w| table.iter().any(|r| r.raw() == w.root));
            watches.roots = keep;
            let todo: Vec<WatchedRoot> = table
                .iter()
                .filter(|r| {
                    !watches
                        .roots
                        .iter()
                        .any(|w| w.root == r.raw() && w.state.is_watching())
                })
                .cloned()
                .collect();
            (stale, todo, watches.tracker.clone())
        };
        stop_tasks(stale.into_iter().filter_map(|w| w.task)).await;
        let builds = todo.iter().map(|root| {
            let network = classify(root.raw()).is_network();
            self.build_root_watch(handle, root, &table, &tracker, network)
        });
        let built = futures::future::join_all(builds).await;
        for (root, outcome) in todo.iter().zip(built) {
            if let Built::Recorded(state, task) = outcome {
                self.install(&handle.id, root, state, task).await;
            }
        }
    }

    /// Start one root's watch, or say why there is none.
    ///
    /// What: `Degraded` for a network mount, checked before any OS-watch cost;
    /// `Failed` when the bounded start errors or times out; `InFlight` when
    /// another start for this root is running (#9339); `Watching` otherwise.
    async fn build_root_watch(
        &self,
        handle: &Arc<IndexHandle>,
        root: &WatchedRoot,
        table: &[WatchedRoot],
        tracker: &IndexedFiles,
        network: bool,
    ) -> Built {
        if network {
            let reason = network_mount_degraded_reason(&handle.id, root.raw());
            tracing::warn!(index_id = %handle.id, root = %root.raw().display(), "{reason}");
            return Built::Recorded(RootWatchState::Degraded { reason }, None);
        }
        let (watched, table) = (root.clone(), table.to_vec());
        let (id, indexer) = (handle.id.clone(), Arc::clone(&handle.indexer));
        // #6524: the watcher populates this index's file-change feed.
        let file_events = Arc::clone(&handle.file_events);
        let (tracker, registry) = (tracker.clone(), self.registry.clone());
        // #9339: the OS-watch start waits up to `WATCHER_START_BOUND`; run it
        // on the blocking pool so no tokio worker waits with it.
        let built = tokio::task::spawn_blocking(move || {
            spawn_watch_loop_for_root(watched, table, id, indexer, tracker, file_events, registry)
        })
        .await;
        match built {
            Ok(Ok(task)) => Built::Recorded(RootWatchState::Watching, Some(task)),
            Ok(Err(e)) if e.downcast_ref::<StartInFlight>().is_some() => {
                tracing::debug!(index_id = %handle.id, "{e:#}");
                Built::InFlight
            }
            Ok(Err(e)) => self.failed(handle, root, format!("{e:#}")),
            Err(e) => self.failed(handle, root, format!("watcher build task failed: {e}")),
        }
    }

    /// Record a start failure for one root; the index's other roots keep theirs.
    fn failed(&self, handle: &IndexHandle, root: &WatchedRoot, reason: String) -> Built {
        tracing::warn!(
            index_id = %handle.id,
            root = %root.raw().display(),
            "could not start file watcher for this root (incremental indexing disabled for it; \
             the index's other roots are unaffected): {reason}",
        );
        Built::Recorded(RootWatchState::Failed { reason }, None)
    }

    /// Record one root's outcome. A racing sync that already installed a live
    /// watch for this root wins, and the task just built is stopped (#1640).
    async fn install(
        &self,
        id: &IndexId,
        root: &WatchedRoot,
        state: RootWatchState,
        task: Option<WatcherTask>,
    ) {
        let watching = state.is_watching();
        // Decided under the lock, acted on after it: no borrow of the map
        // (which holds non-`Sync` tasks) may live across the `stop` await.
        let lost_race = {
            let mut guard = self.inner.lock().await;
            let watches = guard.entry(id.clone()).or_default();
            let pos = watches.roots.iter().position(|w| w.root == root.raw());
            match pos {
                Some(i) if watches.roots[i].state.is_watching() => Some(task),
                _ => {
                    if let Some(i) = pos {
                        watches.roots.remove(i);
                    }
                    watches.roots.push(RootWatch {
                        root: root.raw().to_path_buf(),
                        slot: root.slot(),
                        state,
                        task,
                    });
                    None
                }
            }
        };
        if let Some(task) = lost_race {
            stop_tasks(task).await;
            return;
        }
        if watching {
            tracing::info!(
                index_id = %id,
                root = %root.raw().display(),
                "file watcher active — saves trigger incremental indexing (issue #1621)",
            );
        }
    }

    /// Stop watching a single index, every root, and wait for the tasks.
    ///
    /// Why: `DELETE /indexes/:id` must not leave a watcher firing into a
    /// dropped indexer, or holding its redb corpus open (#3049).
    /// What: removes the index's entry — clearing any degraded or failed
    /// record — and stops every root's task concurrently, so the whole stop is
    /// bounded by one #9315 teardown bound however many roots there are.
    /// Returns `true` when a live watcher was stopped.
    /// Test: `stop_for_index_removes_entry`,
    /// `stop_for_index_releases_the_indexer_before_it_returns`,
    /// `stop_for_index_clears_network_degraded_entry`,
    /// `every_index_root_is_watched`.
    pub async fn stop_for_index(&self, id: &IndexId) -> bool {
        let entry = self.inner.lock().await.remove(id);
        let tasks: Vec<WatcherTask> = entry
            .map(|w| w.roots.into_iter().filter_map(|r| r.task).collect())
            .unwrap_or_default();
        let stopped = !tasks.is_empty();
        stop_tasks(tasks).await;
        if stopped {
            tracing::debug!(index_id = %id, "file watcher stopped");
        }
        stopped
    }

    /// Stop every watcher, returning the number of root watches stopped.
    ///
    /// Why: graceful shutdown must leave no OS watch or consumer task behind
    /// (#534/#1621).
    /// What: drains the map, then stops every task concurrently.
    /// Test: `stop_all_clears_all`.
    pub async fn stop_all(&self) -> usize {
        let tasks: Vec<WatcherTask> = {
            let mut guard = self.inner.lock().await;
            guard
                .drain()
                .flat_map(|(_, w)| w.roots.into_iter().filter_map(|r| r.task))
                .collect()
        };
        let count = tasks.len();
        stop_tasks(tasks).await;
        if count > 0 {
            tracing::info!("stopped {count} file watcher(s) on shutdown");
        }
        count
    }

    /// Number of indexes with at least one live root watch.
    pub async fn watched_count(&self) -> usize {
        let guard = self.inner.lock().await;
        guard
            .values()
            .filter(|w| any_root(w, RootWatchState::is_watching))
            .count()
    }

    /// Whether an index has at least one live root watch — the any-of-N
    /// reading `watcher.active`, `/health` and the idle-suspend ticker want.
    /// Test: `is_watching_reflects_spawn_and_stop`.
    pub async fn is_watching(&self, id: &IndexId) -> bool {
        let guard = self.inner.lock().await;
        guard
            .get(id)
            .is_some_and(|w| any_root(w, RootWatchState::is_watching))
    }

    /// Does a root of `handle` lack a watch worth (re)starting? (#7434)
    ///
    /// Why: `is_watching` is true for a PARTIALLY watched index, so a query
    /// gated on it never retried a root whose start failed or timed out.
    /// What: `true` when a root in the handle's table has no entry, or a
    /// `Failed` one. A `Degraded` root is #3408's deliberate refusal and is
    /// not re-asked per query.
    /// Test: `a_stuck_root_start_does_not_block_the_other_roots`.
    pub async fn needs_root_resync(&self, handle: &IndexHandle) -> bool {
        let guard = self.inner.lock().await;
        let roots = guard
            .get(&handle.id)
            .map(|w| w.roots.as_slice())
            .unwrap_or_default();
        std::iter::once(&handle.root_path)
            .chain(handle.additional_roots.iter())
            .any(|r| match roots.iter().find(|w| &w.root == r) {
                None => true,
                Some(w) => matches!(w.state, RootWatchState::Failed { .. }),
            })
    }

    /// Number of indexes with at least one network-degraded root (#3408).
    /// Test: `network_mount_root_is_refused_and_reported`.
    pub async fn network_degraded_count(&self) -> usize {
        let guard = self.inner.lock().await;
        guard
            .values()
            .filter(|w| any_root(w, RootWatchState::is_network_degraded))
            .count()
    }

    /// The first network-degraded root's actionable reason, in root order.
    /// Test: `network_mount_root_is_refused_and_reported`.
    pub async fn network_degraded_reason(&self, id: &IndexId) -> Option<String> {
        self.root_watch_states(id)
            .await
            .into_iter()
            .find(|r| r.state == "degraded")
            .and_then(|r| r.reason)
    }

    /// Every root's watch state for one index, primary first (#7434).
    ///
    /// Why: `GET /indexes/:id/status` must name WHICH tree stopped updating.
    /// What: one [`RootWatchReport`] per recorded root; empty when nothing is
    /// recorded (never spawned, stopped, or the watcher is disabled).
    /// Test: `status_lists_every_root_and_its_watch`.
    pub async fn root_watch_states(&self, id: &IndexId) -> Vec<RootWatchReport> {
        let guard = self.inner.lock().await;
        let Some(watches) = guard.get(id) else {
            return Vec::new();
        };
        let mut rows: Vec<&RootWatch> = watches.roots.iter().collect();
        rows.sort_by_key(|w| w.slot.map_or(0, |n| n + 1));
        rows.into_iter()
            .map(|w| RootWatchReport {
                root: w.root.display().to_string(),
                slot: w.slot,
                primary: w.slot.is_none(),
                state: w.state.label(),
                reason: w.state.reason().map(str::to_string),
            })
            .collect()
    }
}

/// `true` when any root of `watches` satisfies `pred`.
fn any_root(watches: &IndexWatches, pred: fn(&RootWatchState) -> bool) -> bool {
    watches.roots.iter().any(|w| pred(&w.state))
}

/// Stop `tasks` concurrently, so N roots cost one #9315 teardown bound.
async fn stop_tasks(tasks: impl IntoIterator<Item = WatcherTask>) {
    futures::future::join_all(tasks.into_iter().map(WatcherTask::stop)).await;
}

#[cfg(test)]
#[path = "watcher_roots_7434_tests.rs"]
mod roots_7434_tests;
#[cfg(test)]
#[path = "watcher_manager_tests.rs"]
mod tests;
