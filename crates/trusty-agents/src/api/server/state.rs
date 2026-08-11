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
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::task::AbortHandle;

use crate::api::types::{PhaseProgress, PmResponse, PmStatus};
use crate::events::{self, Event};
use crate::recap::{RecapConfig, RecapTracker};
use crate::tm::TmManager;

/// Maximum number of responses retained in memory PER addressed assistant
/// (#4355).
///
/// Why: this used to be a single server-wide cap of 20. Once tasks are split
/// into per-assistant streams a server-wide cap divides across the roster —
/// six assistants would get roughly three turns of history each, which makes
/// "switch assistant, see its stream" useless the moment it works. The bound
/// that matters to a user is "how far back does MY assistant's stream go", so
/// that is the bound the server enforces. The number is unchanged from the old
/// global one, so a single-assistant user sees exactly the depth they saw
/// before.
/// What: `insert_and_trim` evicts the oldest non-`Running` row of an
/// assistant's own stream once that stream exceeds this.
/// Test: `per_agent_retention_keeps_n_for_each_assistant`.
pub(super) const MAX_RETAINED_PER_AGENT: usize = 20;

/// Server-wide backstop across all assistants (#4355).
///
/// Why: per-assistant retention alone is unbounded in the number of DISTINCT
/// assistant ids — `TaskRequest.agent` is a free-form string, so a buggy or
/// hostile client can mint a new stream per request and grow the store
/// forever. This caps total memory (and the size of `tasks.json`, rewritten in
/// full on every upsert) at 10× the old global bound regardless of roster
/// size. At a ~4 KB typical `PmResponse` that is ~800 KB; a pathological
/// 64 KB-narrative store would be ~13 MB, which is the ceiling this constant
/// buys and the reason it is not larger.
/// What: after per-stream trimming, `insert_and_trim` evicts the oldest
/// non-`Running` row overall while the store exceeds this.
/// Test: `global_backstop_caps_total_across_many_assistants`.
pub(super) const MAX_RETAINED_TOTAL: usize = 200;

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
    /// #4703: where `submit_task` records attendance, resolved ONCE here
    /// rather than per-request inside the handler.
    ///
    /// Why: the handler used to call the `$HOME`-resolving `note_turn`, which
    /// made "submitting a task records attendance" untestable — the natural
    /// test wrote into the developer's real `~/.trusty-agents` and had nothing
    /// to assert against, so it passed whether or not the hook worked. Held on
    /// the state — the shape `Repl::attendance_root` already uses — a test
    /// overwrites it with a tempdir exactly as `repl::tests` does.
    /// `None` means no home directory could be resolved; recording is then a
    /// documented no-op, never an error.
    /// Test: `submit_task_records_attendance_under_the_injected_root`.
    pub(super) attendance_root: Option<PathBuf>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            docs_index: None,
            recap_tracker: Arc::new(Mutex::new(RecapTracker::new(RecapConfig::default()))),
            tm_manager: None,
            attendance_root: crate::attendance::default_attendance_root().ok(),
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
        // #4703: built ON `Default` rather than re-listing every field, so a
        // field added later cannot be silently dropped from this constructor.
        Self {
            docs_index: Some(index),
            ..Self::default()
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
            tm_manager,
            ..Self::default()
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
            // Snapshot the two facts needed about the existing row so the read
            // borrow ends before the write below.
            let existing = store
                .responses
                .get(id)
                .map(|r| (r.status.clone(), r.addressed_agent.clone()));
            match existing {
                None => (CancelOutcome::NotFound, None),
                Some((status, _)) if status != PmStatus::Running => {
                    (CancelOutcome::AlreadyTerminal(status), None)
                }
                Some((_, agent)) => {
                    // #4355: `cancelled()` starts from the Concierge default,
                    // so carry the original stream over — otherwise cancelling
                    // a task silently moves it into another assistant's history.
                    let handle = store.handles.remove(id);
                    store.responses.insert(
                        id.to_string(),
                        PmResponse::cancelled(id).addressed_to(agent),
                    );
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
    /// Why: recap assembly needs a recency-ordered snapshot of everything,
    /// unfiltered.
    /// What: `list_stream(None, None)`.
    /// Test: `list_tasks_empty_store_returns_empty_array`.
    pub(super) async fn list(&self) -> Vec<PmResponse> {
        self.list_stream(None, None).await
    }

    /// One assistant's task stream (or the whole store), newest first (#4355).
    ///
    /// Why: this is the server-side half of "switching assistants loads that
    /// assistant's most recent task stream" — the owner's decision was that the
    /// association lives on the server, not in a per-client filter, so that all
    /// clients attached to one running agent see the same stream. Answering it
    /// here rather than by shipping every task to the client also means a
    /// client never has to receive another assistant's conversation to decide
    /// it should not display it.
    /// What: walks insertion order in reverse (newest first), keeping rows
    /// whose `addressed_agent` matches `agent` when one is given, and stops
    /// after `limit` rows when one is given. `agent = None` reproduces the
    /// pre-#4355 full listing exactly.
    /// Test: `tasks_filtered_by_agent_returns_only_that_stream_newest_first`.
    pub(super) async fn list_stream(
        &self,
        agent: Option<&str>,
        limit: Option<usize>,
    ) -> Vec<PmResponse> {
        let store = self.inner.lock().await;
        store
            .order
            .iter()
            .rev()
            .filter_map(|id| store.responses.get(id))
            .filter(|r| agent.is_none_or(|a| r.addressed_agent == a))
            .take(limit.unwrap_or(usize::MAX))
            .cloned()
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
    /// and return `(removed, retained_running)` (#3737).
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
    /// and returns `(removed, retained_running)`. BOTH counts are computed
    /// under the SINGLE lock guard so they are internally consistent — a prior
    /// version derived `retained_running` from a separate `list()` call in the
    /// handler, which could disagree under a concurrent status transition
    /// (code-critic TOCTOU finding). Emits no events: nothing was cancelled.
    /// Test: `clear_recent_tasks_removes_terminal_only`.
    pub(super) async fn clear_terminal_tasks(&self) -> (usize, usize) {
        let (removed, retained_running, snapshot) = {
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
            // Only `Running` entries remain, so the retained count is simply
            // what's left — computed here, under the same guard, so it can't
            // race a concurrent finalize/cancel.
            (to_remove.len(), responses.len(), responses.clone())
        };
        // Persist outside the lock — a restart reloads from tasks.json, so the
        // cleared list must be written back to actually stick.
        persist_tasks(&snapshot).await;
        (removed, retained_running)
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

/// Insert `resp` for `id`, tracking insertion order and trimming the store
/// to its per-assistant and server-wide bounds. Returns a clone of the
/// resulting map for the caller to persist outside the lock.
///
/// Why: Shared by `upsert` (always overwrites) and `finalize_task` (which
/// adds a cancellation guard before calling this). Keeping the
/// insert/track/trim logic in one place avoids the two call sites drifting.
/// A naive index-0 FIFO eviction would silently orphan a genuinely
/// `Running` task once the cap's worth of unrelated tasks churn through the
/// store: its `responses`/`order` row disappears while its `AbortHandle`
/// stays in `handles` forever, and `try_cancel` (which looks the id up in
/// `responses` first) then 404s a task that is very much still alive
/// (issue #3063 code review).
/// What: Inserts, appends to `order` iff the key was previously absent, then
/// trims twice (#4355) — first the touched assistant's own stream down to
/// `MAX_RETAINED_PER_AGENT`, then the whole store down to
/// `MAX_RETAINED_TOTAL`. Trimming the touched stream FIRST is what keeps one
/// busy assistant from evicting an idle assistant's history: the global pass
/// only fires when the roster as a whole is oversized. Both passes go through
/// `evict_oldest_terminal`, which never evicts a `Running` row; if nothing is
/// evictable the pass stops (never kill a live task to make room), so the
/// store may grow transiently — concurrently `Running` tasks are rare.
/// Test: `app_state_trims_to_max_retained` (single-stream case, via `upsert`);
/// `per_agent_retention_keeps_n_for_each_assistant`;
/// `global_backstop_caps_total_across_many_assistants`;
/// `cancel_survives_fifo_eviction_pressure` (a `Running` task planted before a
/// cap's worth of terminal tasks must still be reachable).
fn insert_and_trim(
    store: &mut TaskStore,
    id: String,
    resp: PmResponse,
) -> HashMap<String, PmResponse> {
    let agent = resp.addressed_agent.clone();
    let was_absent = !store.responses.contains_key(&id);
    store.responses.insert(id.clone(), resp);
    if was_absent {
        store.order.push(id);
    }
    while stream_len(store, &agent) > MAX_RETAINED_PER_AGENT
        && evict_oldest_terminal(store, Some(&agent))
    {}
    while store.order.len() > MAX_RETAINED_TOTAL && evict_oldest_terminal(store, None) {}
    store.responses.clone()
}

/// Number of retained rows addressed to `agent` (#4355).
fn stream_len(store: &TaskStore, agent: &str) -> usize {
    store
        .order
        .iter()
        .filter(|id| {
            store
                .responses
                .get(*id)
                .is_some_and(|r| r.addressed_agent == agent)
        })
        .count()
}

/// Evict the oldest non-`Running` row, optionally restricted to one
/// assistant's stream; `false` when there is nothing evictable (#4355).
///
/// Why: both trim passes in `insert_and_trim` need the same "oldest terminal
/// row wins, never a live task" rule, differing only in whether they look at
/// one stream or all of them. Splitting it out keeps that rule stated once —
/// a second copy is exactly how the `handles` map drifts out of lockstep with
/// `responses` and resurrects the #3063 orphaned-`AbortHandle` bug.
/// What: scans `order` oldest-first for the first row whose status isn't
/// `Running` (and, when `agent` is `Some`, whose `addressed_agent` matches),
/// then removes it from `order`, `responses`, and `handles` together.
/// Test: `per_agent_retention_keeps_n_for_each_assistant`,
/// `cancel_survives_fifo_eviction_pressure`.
fn evict_oldest_terminal(store: &mut TaskStore, agent: Option<&str>) -> bool {
    let TaskStore {
        responses,
        order,
        handles,
    } = store;
    let Some(idx) = order.iter().position(|oid| {
        responses.get(oid).is_some_and(|r| {
            r.status != PmStatus::Running && agent.is_none_or(|a| r.addressed_agent == a)
        })
    }) else {
        return false;
    };
    let old = order.remove(idx);
    responses.remove(&old);
    handles.remove(&old);
    true
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
/// Why: A naive `write` to the live file risks readers (or a crash) seeing a
/// half-written file. This used to open-code its own tmp+rename, which was a
/// second implementation of what `state_writer` — the crate's entry point for
/// exactly this — already owned, and it is how this file ended up as the one
/// state file written without the owner-only mode `state_writer` now applies:
/// `tasks.json` holds task narratives, including credential-scrubbed but not
/// provably secret-free child-process stderr (#5230). Routing through the
/// shared writer also buys the cross-process advisory lock the GUI, the
/// `--api` sidecar, and a `cargo run` build need when they share
/// `.trusty-agents/`.
/// What: Serializes the map, then hands the write to
/// `state_writer::atomic_write` (parent-dir creation, lock, `0600` tmp,
/// fsync, rename) on a blocking worker — `fs4`'s lock syscalls block, the same
/// reason `interaction_log` and `session_record` bridge through
/// `spawn_blocking`. Logs (but does not fail) on I/O errors: losing a snapshot
/// is preferable to crashing the running server.
/// Test: `atomic_write_creates_an_owner_only_file`,
/// `atomic_write_tightens_an_existing_world_readable_file`.
async fn persist_tasks(responses: &HashMap<String, PmResponse>) {
    let path = tasks_persistence_path();
    let json = match serde_json::to_vec(responses) {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!(?e, "failed to serialize tasks for persistence");
            return;
        }
    };
    match tokio::task::spawn_blocking(move || crate::state_writer::atomic_write(&path, &json)).await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(?e, "failed to persist tasks.json"),
        Err(e) => tracing::warn!(?e, "tasks.json persist worker failed to join"),
    }
}
