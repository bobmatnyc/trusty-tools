//! Reindex concurrency semaphores (issue #458).
//!
//! Why: startup auto-discover can queue 40+ background reindex tasks.
//! Separating interactive from background semaphores means user requests always
//! acquire a permit promptly regardless of backlog depth.
//!
//! What: two process-global `OnceLock<Semaphore>` instances — one for
//! interactive (user-initiated) reindexes and one for background (startup
//! auto-discover) reindexes — plus helpers to select and observe them.
//!
//! Test: `interactive_reindex_not_starved_by_background` and
//! `reindex_semaphore_selection_routes_by_priority` in `tests.rs`.

use std::sync::{Arc, OnceLock};
use tokio::sync::Semaphore;

use crate::core::registry::IndexId;
use dashmap::DashMap;

/// Maximum number of concurrent interactive (user-initiated) reindex tasks.
/// 2 permits allow a small burst (e.g. indexing two new projects at once)
/// without letting an unbounded fan-out overwhelm the redb + HNSW write locks.
pub(crate) const MAX_PARALLEL_REINDEXES: usize = 2;

/// Maximum concurrent background reindex tasks. 1 serialises the startup
/// auto-discover storm: tasks run one at a time and never consume the
/// interactive semaphore's slots.
pub(crate) const MAX_PARALLEL_BACKGROUND_REINDEXES: usize = 1;

/// Interactive (user-initiated) reindex semaphore (issue #458).
///
/// Why: Startup auto-discover can queue 40+ background reindex tasks, all of
/// which contend for the same semaphore. A user running `trusty-search index
/// <new>` then queues behind the entire backlog and sits at "pending" for
/// minutes. Separating interactive from background requests means a user
/// request always gets a permit promptly, regardless of how many background
/// tasks are queued.
///
/// What: A small N-permit semaphore (default 2) reserved exclusively for
/// interactive (user-initiated) reindexes. Background/startup reindexes use
/// `background_reindex_semaphore()` instead.
///
/// Test: `interactive_reindex_not_starved_by_background`.
fn reindex_semaphore() -> &'static Semaphore {
    static SEM: OnceLock<Semaphore> = OnceLock::new();
    SEM.get_or_init(|| Semaphore::new(MAX_PARALLEL_REINDEXES))
}

/// Background (startup / auto-discover) reindex semaphore (issue #458).
///
/// Why: all startup auto-discover reindexes drain through this single-permit
/// semaphore so they run sequentially and never consume the interactive
/// semaphore's slots.
///
/// What: 1-permit semaphore. Background tasks queue here; interactive tasks
/// never touch this semaphore.
///
/// Test: `interactive_reindex_not_starved_by_background`.
pub(crate) fn background_reindex_semaphore() -> &'static Semaphore {
    static BG_SEM: OnceLock<Semaphore> = OnceLock::new();
    BG_SEM.get_or_init(|| Semaphore::new(MAX_PARALLEL_BACKGROUND_REINDEXES))
}

/// Atomic counter tracking how many background tasks are queued (waiting or
/// in-flight on the background semaphore). Incremented when a background task
/// enters `spawn_reindex_with_cleanup`; decremented when the permit is released.
pub(crate) static BACKGROUND_QUEUE_DEPTH: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Returns the number of background reindex tasks currently waiting for a
/// permit (queued in `background_reindex_semaphore()`). Exposed for the
/// `/health` payload so operators can see the startup backlog drain.
///
/// Why: without this counter, an operator watching `/health` has no way to
/// tell whether the daemon is still processing the startup reindex storm or
/// has finished. The number ticks down as each background job completes.
///
/// What: the number of available permits in the background semaphore is
/// `MAX_PARALLEL_BACKGROUND_REINDEXES - in_flight`, so the queue depth is
/// approximately the number of tasks blocked on `acquire()`. We approximate
/// this by tracking it with an `AtomicUsize` incremented before acquire and
/// decremented after.
///
/// Test: covered by `background_reindex_queue_depth_counts_waiting_tasks`.
pub fn background_reindex_queue_depth() -> usize {
    BACKGROUND_QUEUE_DEPTH.load(std::sync::atomic::Ordering::Relaxed)
}

/// Select the correct reindex semaphore based on priority (issue #458).
///
/// Why: extracted so the routing decision can be unit-tested without wiring
/// a full reindex task. Keeping the selection in one function means future
/// changes to the priority model have exactly one edit site.
/// What: `priority=true` → interactive semaphore (2 permits); `priority=false`
/// → background semaphore (1 permit, serialises startup storm).
/// Test: `reindex_semaphore_selection_routes_by_priority` below.
pub(crate) fn reindex_semaphore_for(priority: bool) -> &'static Semaphore {
    if priority {
        reindex_semaphore()
    } else {
        background_reindex_semaphore()
    }
}

/// Process-global registry of per-index mutual-exclusion semaphores (issue
/// #2984 Phase 1 CRITICAL finding 2).
///
/// Why: `background_reindex_semaphore()` is a single process-wide 1-permit
/// semaphore shared across EVERY index's background reindex/deferred-embed
/// tasks. The `PATCH /indexes/:id/config` component-toggle handler used to
/// `try_acquire` that global semaphore as its same-index conflict guard,
/// which was wrong on both ends: it produced false `409`s for a completely
/// UNRELATED index whenever any background embed was in flight anywhere
/// (routine — `defer_embed` defaults `true` and startup auto-discover can
/// queue 40+), and it missed the real hazard — a genuinely concurrent
/// INTERACTIVE reindex on the SAME index (`reindex_semaphore()`, a separate
/// 2-permit semaphore never touched by the old guard) could still race a
/// component catch-up, corrupting `symbol_graph` with a lost update and
/// flapping the stage between `InProgress`/`Ready`. A semaphore keyed by
/// `IndexId` fixes both: unrelated indexes never contend with each other, and
/// every mutating path for the SAME index — reindex (`runner::run_reindex`),
/// deferred-embed (`defer_embed::spawn_deferred_embed_pass`), and component
/// catch-up (`server::components::spawn_component_catch_up`, via the
/// handler's own `try_acquire_owned`) — contends for the identical 1 permit.
/// `background_reindex_semaphore` keeps its existing system-wide throttling
/// role unchanged; this is purely additive per-index mutual exclusion.
/// What: a `DashMap<IndexId, Arc<Semaphore>>`, lazily populated — one entry
/// per index, created on first use, 1 permit each.
/// Test: `service::server::tests_components::patch_component_turn_on_409s_when_same_index_catch_up_in_progress`,
/// `patch_component_toggle_succeeds_for_different_index_while_global_background_semaphore_busy`.
static INDEX_LOCKS: OnceLock<DashMap<IndexId, Arc<Semaphore>>> = OnceLock::new();

/// Returns (creating on first use) the 1-permit mutual-exclusion semaphore
/// for `id`. See [`INDEX_LOCKS`] for the full rationale.
///
/// Why: a single accessor keeps the lazy-creation logic in one place so every
/// caller — the component-toggle handler, `run_reindex`, and
/// `spawn_deferred_embed_pass` — shares the exact same semaphore instance per
/// index.
/// What: `DashMap::entry` + `or_insert_with` — no lock is held across the
/// `Arc<Semaphore>` clone returned to the caller.
/// Test: see the handler-level tests referenced on [`INDEX_LOCKS`].
pub(crate) fn index_semaphore(id: &IndexId) -> Arc<Semaphore> {
    INDEX_LOCKS
        .get_or_init(DashMap::new)
        .entry(id.clone())
        .or_insert_with(|| Arc::new(Semaphore::new(1)))
        .clone()
}

/// Remove `id`'s entry from [`INDEX_LOCKS`], e.g. when the index itself is
/// deleted (issue #2984 Phase 1 delta-review MEDIUM finding).
///
/// Why: [`INDEX_LOCKS`] only ever grows — `index_semaphore` lazily inserts an
/// entry per distinct `IndexId` it has ever seen and nothing previously
/// removed one, so repeatedly creating and deleting indexes (a common
/// integration-test / churn pattern) leaks one abandoned `Arc<Semaphore>` per
/// deleted id for the lifetime of the daemon process.
///
/// Semantics chosen: **always remove unconditionally**, with no `try_acquire`
/// probe first. This is safe because a task that is still mid-reindex/catch-up
/// when the delete lands already holds its own `Arc<Semaphore>` clone (and,
/// for an in-flight acquire, an `OwnedSemaphorePermit` derived from that same
/// `Arc`) obtained from an earlier `index_semaphore(id)` call — removing the
/// DashMap *entry* does not touch that `Arc`'s refcount or the semaphore
/// object it points to, so the in-flight task's permit remains completely
/// valid until the task finishes and drops it. The entry is purely a
/// "next caller gets this instance" registration, not the lock itself. The
/// only thing eviction changes is what a *future* `index_semaphore(id)` call
/// returns: a brand-new, uncontended `Semaphore::new(1)`. That is exactly the
/// right behaviour for a deleted id — an index recreated under the same id
/// afterwards is a logically new resource and must not inherit stale busy/409
/// state from a semaphore instance tied to the index that no longer exists.
/// A `try_acquire` probe would add complexity (and a spurious 409-shaped
/// signal to callers who don't expect one from a delete) without preventing
/// anything eviction alone doesn't already handle correctly.
/// What: removes the `(id, Arc<Semaphore>)` entry from [`INDEX_LOCKS`] if
/// present. No-op if the map was never initialised or `id` was never seen.
/// Test: `remove_index_semaphore_evicts_entry_and_next_call_gets_fresh_instance`,
/// `remove_index_semaphore_is_a_no_op_for_unknown_id`.
pub(crate) fn remove_index_semaphore(id: &IndexId) {
    if let Some(map) = INDEX_LOCKS.get() {
        map.remove(id);
    }
}

#[cfg(test)]
mod tests_index_lock_eviction {
    use super::*;

    /// After [`remove_index_semaphore`] evicts an id's entry, the next
    /// [`index_semaphore`] call for that SAME id must allocate a brand-new
    /// `Semaphore` instance rather than reusing the evicted one.
    ///
    /// Why: proves the DashMap entry is actually gone (not just untouched) —
    /// pointer identity is the only externally-observable signal available
    /// since `Semaphore` itself exposes no "was this ever used" state.
    /// What: grabs the semaphore for a unique test-only id, evicts it, grabs
    /// it again, and asserts the two `Arc<Semaphore>` pointers differ.
    /// Test: this IS the test.
    #[test]
    fn remove_index_semaphore_evicts_entry_and_next_call_gets_fresh_instance() {
        let id = IndexId::new("semaphore-evict-test-9f3a1c2d");
        let first = index_semaphore(&id);
        remove_index_semaphore(&id);
        let second = index_semaphore(&id);
        assert!(
            !Arc::ptr_eq(&first, &second),
            "after eviction, index_semaphore must allocate a fresh Semaphore \
             instance instead of reusing the evicted one (issue #2984 Phase 1 \
             delta-review MEDIUM finding)"
        );
    }

    /// Evicting an id that was never registered must be a silent no-op, not
    /// a panic.
    /// Why: the DELETE handler unconditionally calls this on every delete,
    /// including deletes of indexes that never had a background reindex/
    /// catch-up (and therefore never called `index_semaphore`).
    /// What: calls `remove_index_semaphore` on a fresh, never-seen id.
    /// Test: this IS the test (the absence of a panic is the assertion).
    #[test]
    fn remove_index_semaphore_is_a_no_op_for_unknown_id() {
        remove_index_semaphore(&IndexId::new("semaphore-evict-test-never-seen-4b21"));
    }
}
