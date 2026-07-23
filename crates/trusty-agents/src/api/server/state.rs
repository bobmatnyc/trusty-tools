//! Shared server state + task persistence (#151, #212, #371, #450).
//!
//! Why: Background workflow tasks need a place to deposit their results so
//! polling handlers can read them; restarts must not lose in-flight history;
//! recap generation and live tmux management hang off the same shared state.
//! What: `AppState` holds the in-memory `TaskStore` (behind a `Mutex`), an
//! optional docs index, the recap tracker, and an optional `TmManager`.
//! Persistence reads/writes `.trusty-agents/state/tasks.json` atomically.
//! Test: `app_state_*` and `session_e2e_*` in `super::tests`.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::task::AbortHandle;

use crate::api::types::{PhaseProgress, PmResponse, PmStatus};
use crate::events::{self, Event};
use crate::recap::{RecapConfig, RecapTracker};
use crate::tm::TmManager;

/// Maximum number of terminal responses retained in memory.
pub(super) const MAX_RETAINED: usize = 20;

/// Filesystem location for runtime state (recaps, tasks.json, etc.).
///
/// Why: Centralised so production code, tests, and `load_recap` agree on the
/// directory. Mirrors `tasks_persistence_path()` which is hard-coded to the
/// same root.
/// What: Returns `.trusty-agents/state` relative to cwd.
/// Test: Indirectly via recap + persistence round-trip tests.
pub(super) fn state_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(".trusty-agents/state")
}

/// Shared server state.
///
/// Why: Background workflow tasks need somewhere to deposit their results so
/// polling handlers can read them. A simple `HashMap` behind a `Mutex` is
/// ample for a single-node dev server; revisit with sled/redb if persistence
/// becomes a requirement.
/// What: Holds the task store, optional docs index, recap tracker, and
/// optional tmux manager, all behind `Arc`/`Mutex` for cheap cloning into
/// background futures.
/// Test: `app_state_*` in `super::tests`.
#[derive(Clone)]
pub struct AppState {
    pub(super) inner: Arc<Mutex<TaskStore>>,
    /// #187: Optional in-memory TF-IDF index over project documentation.
    /// `None` when the server starts without a docs corpus (tests, etc.).
    pub(super) docs_index: Option<Arc<crate::docs_index::DocsIndex>>,
    /// #371: Per-session task counter driving recap generation. Wrapped in
    /// `Arc<Mutex>` so background `run_task` futures can tick the counter
    /// without taking ownership of the tracker.
    pub(super) recap_tracker: Arc<Mutex<RecapTracker>>,
    /// #450: Optional TM (tmux) manager for live session management. `None`
    /// when tmux is not available on the host or initialization failed; the
    /// `/api/tm/*` routes return 503 in that case so the UI can degrade
    /// gracefully without crashing the server.
    pub(super) tm_manager: Option<Arc<Mutex<TmManager>>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            docs_index: None,
            recap_tracker: Arc::new(Mutex::new(RecapTracker::new(RecapConfig::default()))),
            tm_manager: None,
        }
    }
}

impl AppState {
    /// #187: Construct an `AppState` with a docs index attached.
    ///
    /// Why: `--api` mode builds the index at startup and threads it into
    /// the server so `GET /api/docs/search` can query it. Tests use the
    /// `Default::default` path (no index) and the search route falls back
    /// to "not ready".
    /// What: Same as `Default` but with `docs_index = Some(index)`.
    /// Test: `docs_search_*` in `super::tests`.
    pub fn with_docs_index(index: Arc<crate::docs_index::DocsIndex>) -> Self {
        Self {
            inner: Arc::default(),
            docs_index: Some(index),
            recap_tracker: Arc::new(Mutex::new(RecapTracker::new(RecapConfig::default()))),
            tm_manager: None,
        }
    }

    /// #212: Construct an `AppState` pre-populated from `tasks.json` if present.
    ///
    /// Why: When the launchd-managed API server is restarted (deploy, reboot,
    /// crash), in-flight task results held only in `Arc<Mutex<HashMap>>` are
    /// lost — clients polling `GET /api/task/:id` see a 404 forever. Loading
    /// the persisted snapshot at startup lets the UI continue showing prior
    /// task history across restarts.
    /// What: Reads `.trusty-agents/state/tasks.json` (relative to cwd) and seeds
    /// the in-memory map. Missing/unreadable file is non-fatal — we start
    /// empty. Subsequent `upsert` calls write the file atomically (temp +
    /// rename) so a crash mid-write can't corrupt the snapshot.
    /// Test: `app_state_persists_and_reloads_tasks` — upsert a task, drop
    /// the AppState, call `with_docs_index_and_persistence`, assert the
    /// task is present.
    pub async fn with_persistence(index: Option<Arc<crate::docs_index::DocsIndex>>) -> Self {
        let store = load_persisted_tasks().await.unwrap_or_default();
        // #450: Best-effort TmManager init. tmux may not be installed (CI,
        // minimal Docker images); in that case TmManager::new fails and the
        // `/api/tm/*` routes return 503 rather than crashing the server.
        let tm_manager = TmManager::new(&state_dir())
            .map(|m| Arc::new(Mutex::new(m)))
            .map_err(|e| {
                tracing::warn!(error = %e, "TmManager init failed; /api/tm/* will return 503");
                e
            })
            .ok();
        Self {
            inner: Arc::new(Mutex::new(store)),
            docs_index: index,
            recap_tracker: Arc::new(Mutex::new(RecapTracker::new(RecapConfig::default()))),
            tm_manager,
        }
    }

    /// Insert or update the response for `id`. When a response transitions
    /// to terminal state we record its position for LRU trimming.
    ///
    /// Why: Background futures finalize results here; polling reads them back.
    /// What: Upserts into the map, tracks insertion order, trims to
    /// `MAX_RETAINED`, and persists the snapshot outside the lock.
    /// Test: `app_state_trims_to_max_retained`, `app_state_get_returns_stored`.
    pub(super) async fn upsert(&self, id: String, resp: PmResponse) {
        let snapshot = {
            let mut store = self.inner.lock().await;
            insert_and_trim(&mut store, id, resp)
        };
        // #212: Persist outside the lock — disk I/O shouldn't block readers.
        persist_tasks(&snapshot).await;
    }

    /// Store a background task's terminal result, unless the task was
    /// already marked `Cancelled` by `DELETE /api/task/:id` (#3063).
    ///
    /// Why: `try_cancel` and a task's own completion race on the same
    /// `AppState`: the client may call `DELETE /api/task/:id` at nearly the
    /// same instant the background future finishes computing its result.
    /// `AbortHandle::abort()` is cooperative — it only takes effect at the
    /// task's next `.await` point — so a task that already produced its
    /// final value before the abort signal lands would otherwise overwrite
    /// the cancelled record with a stale success/failure a moment later.
    /// Both `submit_task` code paths call this instead of `upsert` for their
    /// final result so cancellation, once recorded, is sticky.
    /// What: Removes the task's stored `AbortHandle` (it's done either way)
    /// and, only if the current stored status isn't already `Cancelled`,
    /// performs the same insert/trim/persist `upsert` does.
    /// Test: `cancel_running_task_marks_cancelled` covers this directly —
    /// its final assertions check that a late `finalize_task` call from an
    /// aborted future does not clobber the already-cancelled record.
    pub(super) async fn finalize_task(&self, id: String, resp: PmResponse) {
        let snapshot = {
            let mut store = self.inner.lock().await;
            store.handles.remove(&id);
            let already_cancelled = matches!(
                store.responses.get(&id),
                Some(r) if r.status == PmStatus::Cancelled
            );
            if already_cancelled {
                None
            } else {
                Some(insert_and_trim(&mut store, id, resp))
            }
        };
        if let Some(snapshot) = snapshot {
            persist_tasks(&snapshot).await;
        }
    }

    /// Register the abort handle for a freshly spawned background task
    /// (#3063).
    ///
    /// Why: `submit_task` calls this right after `tokio::spawn` so
    /// `DELETE /api/task/:id` has something to abort. Registration happens
    /// after spawn (the handle only exists once the `JoinHandle` does), so a
    /// pathologically fast task could finish and call `finalize_task` before
    /// this runs; we guard against orphaning the handle by skipping storage
    /// when the task has already reached a terminal status by the time we
    /// acquire the lock.
    /// What: Inserts `handle` keyed by `id`, unless `id`'s stored response is
    /// already non-`Running`.
    /// Test: exercised indirectly by every test in `tests::cancel` via
    /// `spawn_fake_running_task` (the normal not-yet-terminal insert path);
    /// the pathologically-fast-completion terminal-skip guard itself has no
    /// dedicated regression test yet.
    pub(super) async fn register_handle(&self, id: &str, handle: AbortHandle) {
        let mut store = self.inner.lock().await;
        let terminal = matches!(
            store.responses.get(id),
            Some(r) if r.status != PmStatus::Running
        );
        if !terminal {
            store.handles.insert(id.to_string(), handle);
        }
    }

    /// Abort an in-flight task and mark it `Cancelled` (#3063).
    ///
    /// Why: The primitive behind `DELETE /api/task/:id`. Checking status and
    /// removing/aborting the handle happen under a single lock acquisition
    /// so a concurrent `finalize_task` can't race between the check and the
    /// write.
    /// What: 404-equivalent (`NotFound`) for unknown ids; `AlreadyTerminal`
    /// (carrying the existing status) when the task isn't `Running`; on
    /// `Running`, removes+aborts the stored handle (which, for the
    /// subprocess path, triggers `Child`'s `kill_on_drop` and sends the OS
    /// process a kill signal once the future is dropped), overwrites the
    /// stored response with `PmResponse::cancelled(id)`, persists, and
    /// publishes `Event::SessionCancelled`.
    /// Test: `cancel_running_task_marks_cancelled`, `cancel_unknown_id_404`,
    /// `cancel_already_done_409`.
    pub(super) async fn try_cancel(&self, id: &str) -> CancelOutcome {
        let (outcome, handle) = {
            let mut store = self.inner.lock().await;
            match store.responses.get(id) {
                None => (CancelOutcome::NotFound, None),
                Some(r) if r.status != PmStatus::Running => {
                    (CancelOutcome::AlreadyTerminal(r.status.clone()), None)
                }
                Some(_) => {
                    let handle = store.handles.remove(id);
                    store
                        .responses
                        .insert(id.to_string(), PmResponse::cancelled(id));
                    (CancelOutcome::Cancelled, handle)
                }
            }
        };
        if let Some(h) = handle {
            h.abort();
        }
        if matches!(outcome, CancelOutcome::Cancelled) {
            let snapshot = self.inner.lock().await.responses.clone();
            persist_tasks(&snapshot).await;
            events::publish(Event::SessionCancelled {
                session_id: id.to_string(),
            });
        }
        outcome
    }

    /// Fetch a stored response by id.
    ///
    /// Why: `GET /api/task/:id` reads the cached result.
    /// What: Clones the stored `PmResponse` if present.
    /// Test: `app_state_get_returns_stored`.
    pub(super) async fn get(&self, id: &str) -> Option<PmResponse> {
        let store = self.inner.lock().await;
        store.responses.get(id).cloned()
    }

    /// #149: Append (or replace by `name`) a phase progress event into the
    /// stored response so the polling client sees real-time updates.
    ///
    /// Why: While a workflow runs in a child subprocess, the server reads the
    /// child's stderr for `__OMPM_PROGRESS__ {…}` lines and forwards each one
    /// here. The Tauri UI poller then renders a live phase timeline without
    /// waiting for the workflow to finish.
    /// What: Looks up the response by `id`; if a progress entry with the same
    /// `name` already exists it's overwritten (so `running → done` collapses
    /// into the latest state); otherwise it's appended. #3063: a task
    /// already marked `Cancelled` ignores further progress — the subprocess
    /// may emit a few more lines before its kill signal lands, and those
    /// shouldn't resurrect detail onto a record pollers already treat as
    /// terminal.
    /// Test: Unit-tested via `app_state_append_progress_replaces_by_name`.
    pub(super) async fn append_progress(&self, id: &str, ev: PhaseProgress) {
        let mut store = self.inner.lock().await;
        if let Some(resp) = store.responses.get_mut(id) {
            if resp.status == PmStatus::Cancelled {
                return;
            }
            if let Some(slot) = resp.phases_completed.iter_mut().find(|p| p.name == ev.name) {
                *slot = ev;
            } else {
                resp.phases_completed.push(ev);
            }
        }
    }

    /// List all stored responses, newest first.
    ///
    /// Why: `GET /api/tasks` and recap assembly both need a recency-ordered
    /// snapshot.
    /// What: Walks the insertion order in reverse, cloning each response.
    /// Test: `list_tasks_empty_store_returns_empty_array`.
    pub(super) async fn list(&self) -> Vec<PmResponse> {
        let store = self.inner.lock().await;
        // Newest first.
        store
            .order
            .iter()
            .rev()
            .filter_map(|id| store.responses.get(id).cloned())
            .collect()
    }

    /// Clear all tasks and return the count of tasks that were cancelled.
    ///
    /// Why: `POST /api/clear-context` lets the UI offer a one-click "start
    /// fresh" action without restarting the server. Before #3063 this only
    /// wiped the in-memory record — spawned futures and subprocesses kept
    /// running to completion, orphaned, which violated the obvious user
    /// expectation that "clear" means "stop". Now it aborts every running
    /// task's stored handle (same mechanism `DELETE /api/task/:id` uses)
    /// before dropping the store, so in-process futures are dropped and
    /// subprocess children are killed via `kill_on_drop`.
    /// What: Drains the task store, aborts each running task's handle,
    /// emits `SessionCancelled` for every task that was still in `Running`
    /// state, and returns the cancellation count.
    /// Test: `clear_context_now_aborts_running_task`.
    pub(super) async fn clear_tasks(&self) -> usize {
        let (running_ids, handles) = {
            let mut store = self.inner.lock().await;
            let running_ids: Vec<String> = store
                .responses
                .iter()
                .filter(|(_, r)| r.status == PmStatus::Running)
                .map(|(id, _)| id.clone())
                .collect();
            let handles: Vec<AbortHandle> = running_ids
                .iter()
                .filter_map(|id| store.handles.remove(id))
                .collect();
            store.responses.clear();
            store.order.clear();
            store.handles.clear();
            (running_ids, handles)
        };
        for handle in handles {
            handle.abort();
        }
        let cancelled = running_ids.len();
        for id in running_ids {
            events::publish(Event::SessionCancelled { session_id: id });
        }
        cancelled
    }

    /// Clear terminal (non-`Running`) tasks, preserving any still in flight,
    /// and return the count removed (#3737).
    ///
    /// Why: The GUI's "Recent tasks" panel needs a "Clear" affordance that
    /// wipes the finished-task history without the collateral damage
    /// `clear_tasks` (`POST /api/clear-context`) inflicts — that one aborts
    /// every running task and drops the whole store, which is the wrong
    /// semantics for "clear the history list": a user tidying the list should
    /// not thereby kill work that is still running. This retains `Running`
    /// entries (and their abort handles) untouched so an in-flight task stays
    /// visible and cancellable, and only drops rows that have reached a
    /// terminal status (`Success`/`Failed`/`Partial`/`Cancelled`).
    /// What: Removes every response whose status isn't `Running` from
    /// `responses`, prunes `order` to match, drops any stray handles for
    /// removed ids (there should be none — terminal tasks have already had
    /// their handle removed by `finalize_task`/`try_cancel` — but stay in
    /// lockstep defensively), re-persists the trimmed snapshot so the removal
    /// survives a restart (the store is reloaded from `tasks.json` on boot),
    /// and returns how many were removed. Emits no events: nothing was
    /// cancelled.
    /// Test: `clear_terminal_tasks_removes_terminal_keeps_running`.
    pub(super) async fn clear_terminal_tasks(&self) -> usize {
        let (removed, snapshot) = {
            let mut store = self.inner.lock().await;
            let to_remove: Vec<String> = store
                .responses
                .iter()
                .filter(|(_, r)| r.status != PmStatus::Running)
                .map(|(id, _)| id.clone())
                .collect();
            for id in &to_remove {
                store.responses.remove(id);
                store.handles.remove(id);
            }
            let TaskStore {
                responses, order, ..
            } = &mut *store;
            order.retain(|id| responses.contains_key(id));
            (to_remove.len(), responses.clone())
        };
        // Persist outside the lock — a restart reloads from tasks.json, so the
        // cleared list must be written back to actually stick.
        persist_tasks(&snapshot).await;
        removed
    }
}

/// In-memory task result store.
///
/// Why: Backs `AppState` polling + listing with insertion-order tracking for
/// LRU eviction.
/// What: `responses` maps task_id → response; `order` records insertion
/// order, newest last; `handles` (#3063) maps task_id → the abort handle for
/// its still-running background future, so `DELETE /api/task/:id` and
/// `clear_tasks` have something to abort. Entries are removed once a task
/// reaches a terminal status (`finalize_task`, `try_cancel`, `clear_tasks`).
/// Test: Exercised by `AppState` tests.
#[derive(Default)]
pub(super) struct TaskStore {
    /// task_id -> response (may be a `running` placeholder).
    pub(super) responses: HashMap<String, PmResponse>,
    /// Insertion order for eviction; newest last.
    pub(super) order: Vec<String>,
    /// task_id -> abort handle for an in-flight background task (#3063).
    pub(super) handles: HashMap<String, AbortHandle>,
}

/// Outcome of `AppState::try_cancel`.
///
/// Why: Gives `DELETE /api/task/:id` a typed result to translate into an
/// HTTP status instead of re-deriving "is this cancellable" from a
/// `PmResponse` after the fact.
/// What: `NotFound` (404), `AlreadyTerminal` (409, carries the existing
/// status), `Cancelled` (200 — the task was running and is now aborted).
/// Test: `cancel_running_task_marks_cancelled`, `cancel_unknown_id_404`,
/// `cancel_already_done_409`.
pub(super) enum CancelOutcome {
    NotFound,
    AlreadyTerminal(PmStatus),
    Cancelled,
}

/// Insert `resp` for `id`, tracking insertion order and trimming to
/// `MAX_RETAINED`. Returns a clone of the resulting map for the caller to
/// persist outside the lock.
///
/// Why: Shared by `upsert` (always overwrites) and `finalize_task` (which
/// adds a cancellation guard before calling this). Keeping the
/// insert/track/trim logic in one place avoids the two call sites drifting.
/// A naive index-0 FIFO eviction would silently orphan a genuinely
/// `Running` task once `MAX_RETAINED` unrelated tasks churn through the
/// store: its `responses`/`order` row disappears while its `AbortHandle`
/// stays in `handles` forever, and `try_cancel` (which looks the id up in
/// `responses` first) then 404s a task that is very much still alive
/// (issue #3063 code review).
/// What: Inserts, appends to `order` iff the key was previously absent,
/// then evicts entries past `MAX_RETAINED` — walking forward from the
/// oldest to find the first entry whose status isn't `Running` and
/// evicting that one instead of blindly evicting index 0, removing its
/// `handles` entry too so the maps stay in lockstep. If every retained row
/// happens to be `Running`, eviction is skipped entirely for this call
/// (never kill a live task to make room) — in practice this only lets the
/// store grow transiently, since concurrently `Running` tasks are rare.
/// Test: `app_state_trims_to_max_retained` (via `upsert`, all-terminal
/// case); `cancel_survives_fifo_eviction_pressure` (a `Running` task
/// planted before `MAX_RETAINED` terminal tasks must still be reachable).
fn insert_and_trim(
    store: &mut TaskStore,
    id: String,
    resp: PmResponse,
) -> HashMap<String, PmResponse> {
    let was_absent = !store.responses.contains_key(&id);
    store.responses.insert(id.clone(), resp);
    if was_absent {
        store.order.push(id);
    }
    while store.order.len() > MAX_RETAINED {
        let mut evict_idx = None;
        for (i, oid) in store.order.iter().enumerate() {
            let is_running = matches!(
                store.responses.get(oid),
                Some(r) if r.status == PmStatus::Running
            );
            if !is_running {
                evict_idx = Some(i);
                break;
            }
        }
        let Some(idx) = evict_idx else {
            // Every retained row is Running — don't evict a live task.
            break;
        };
        let old = store.order.remove(idx);
        store.responses.remove(&old);
        store.handles.remove(&old);
    }
    store.responses.clone()
}

/// Path where the task snapshot is persisted.
///
/// Why: Centralized so production code and tests agree on location.
/// Located under `.trusty-agents/state/` to colocate with other runtime state
/// (build.json, processes.json) and stay outside committed config.
/// What: Returns `.trusty-agents/state/tasks.json`.
/// Test: Indirectly via persistence round-trip.
fn tasks_persistence_path() -> std::path::PathBuf {
    std::path::PathBuf::from(".trusty-agents/state/tasks.json")
}

/// Load persisted tasks from disk, if the file exists and is valid JSON.
///
/// Why: Non-fatal — a missing or malformed file should not prevent the
/// server from starting; we just begin with an empty store.
/// What: Reads the JSON file as `HashMap<String, PmResponse>`, then
/// reconstructs a `TaskStore` (responses + insertion order). Order is
/// rebuilt by sorting keys; the exact original order is not preserved
/// across restarts but newest-first listing remains stable thereafter.
/// Test: Persist a known map, call this fn, assert keys round-trip.
async fn load_persisted_tasks() -> Option<TaskStore> {
    let path = tasks_persistence_path();
    let bytes = tokio::fs::read(&path).await.ok()?;
    let responses: HashMap<String, PmResponse> = serde_json::from_slice(&bytes).ok()?;
    let mut order: Vec<String> = responses.keys().cloned().collect();
    order.sort(); // deterministic, even if not original order
    Some(TaskStore {
        responses,
        order,
        handles: HashMap::new(),
    })
}

/// Persist the given task map to disk atomically.
///
/// Why: A naive `write` to the live file risks readers (or a crash) seeing
/// a half-written file. Writing to a sibling temp path and renaming is
/// atomic on the same filesystem on POSIX, so observers either see the old
/// snapshot or the new one — never a corrupt one.
/// What: Ensures the parent directory exists, writes JSON to
/// `tasks.json.tmp`, then `rename`s onto `tasks.json`. Logs (but does not
/// fail) on I/O errors — losing a snapshot is preferable to crashing the
/// running server.
/// Test: Call with a sample map, assert the target file parses back to the
/// same map; force the parent dir to be missing and assert no panic.
async fn persist_tasks(responses: &HashMap<String, PmResponse>) {
    let path = tasks_persistence_path();
    let tmp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent()
        && let Err(e) = tokio::fs::create_dir_all(parent).await
    {
        tracing::warn!(?e, "failed to create state dir for tasks.json");
        return;
    }
    let json = match serde_json::to_vec(responses) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(?e, "failed to serialize tasks for persistence");
            return;
        }
    };
    if let Err(e) = tokio::fs::write(&tmp, &json).await {
        tracing::warn!(?e, "failed to write tasks.json.tmp");
        return;
    }
    if let Err(e) = tokio::fs::rename(&tmp, &path).await {
        tracing::warn!(?e, "failed to rename tasks.json.tmp -> tasks.json");
    }
}
