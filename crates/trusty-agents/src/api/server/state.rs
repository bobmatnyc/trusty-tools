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
            let was_absent = !store.responses.contains_key(&id);
            store.responses.insert(id.clone(), resp);
            if was_absent {
                store.order.push(id);
            }
            // Trim to MAX_RETAINED by dropping the oldest entries.
            while store.order.len() > MAX_RETAINED {
                let old = store.order.remove(0);
                store.responses.remove(&old);
            }
            store.responses.clone()
        };
        // #212: Persist outside the lock — disk I/O shouldn't block readers.
        persist_tasks(&snapshot).await;
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
    /// into the latest state); otherwise it's appended.
    /// Test: Unit-tested via `app_state_append_progress_replaces_by_name`.
    pub(super) async fn append_progress(&self, id: &str, ev: PhaseProgress) {
        let mut store = self.inner.lock().await;
        if let Some(resp) = store.responses.get_mut(id) {
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
    /// fresh" action without restarting the server. Callers that had running
    /// sessions receive a `SessionCancelled` event so SSE subscribers can
    /// update their UI state before the page reloads.
    /// What: Drains the task store, emits `SessionCancelled` for every task
    /// that was still in `Running` state, and returns the cancellation count.
    /// Test: Submit a task (status=running), call clear_tasks, assert list
    /// returns empty and the count matches.
    pub(super) async fn clear_tasks(&self) -> usize {
        let mut store = self.inner.lock().await;
        let running_ids: Vec<String> = store
            .responses
            .iter()
            .filter(|(_, r)| r.status == PmStatus::Running)
            .map(|(id, _)| id.clone())
            .collect();
        let cancelled = running_ids.len();
        for id in running_ids {
            events::publish(Event::SessionCancelled { session_id: id });
        }
        store.responses.clear();
        store.order.clear();
        cancelled
    }
}

/// In-memory task result store.
///
/// Why: Backs `AppState` polling + listing with insertion-order tracking for
/// LRU eviction.
/// What: `responses` maps task_id → response; `order` records insertion
/// order, newest last.
/// Test: Exercised by `AppState` tests.
#[derive(Default)]
pub(super) struct TaskStore {
    /// task_id -> response (may be a `running` placeholder).
    pub(super) responses: HashMap<String, PmResponse>,
    /// Insertion order for eviction; newest last.
    pub(super) order: Vec<String>,
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
    Some(TaskStore { responses, order })
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
