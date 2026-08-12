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

use std::sync::atomic::AtomicBool;
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

/// Process-global registry of per-index teardown locks (issue #3049, round 2).
///
/// Why: the first #3049 fix used [`INDEX_LOCKS`] as the quiescence point, on the
/// premise that every long-running writer holds that permit. Review found the
/// premise false — `index_file_handler`, `remove_file_handler`,
/// `ingest_graph_handler`, the filesystem watch loop, boot reconcile, and the
/// relocate handler all write `handle.indexer` while holding NO permit. A delete
/// racing any of them acquired the uncontended permit instantly, reported
/// `quiesced: true`, and `remove_dir_all`'d the directory mid-write. Gating those
/// six on [`INDEX_LOCKS`] itself was rejected: it is a 1-permit MUTUAL-EXCLUSION
/// lock, so a single `index-file` call would then block for the entire duration
/// of a reindex, which is a serialisation regression on the crate's supported
/// incremental-indexing path.
///
/// What: a `DashMap<IndexId, Arc<RwLock<()>>>` used ONLY for teardown
/// ordering, never for mutual exclusion. Every writer holds the READ side for
/// the span of its write, so writers stay as concurrent with each other as they
/// are today; `unregister_index` takes the WRITE side, which is exactly "no
/// writer of any kind is in flight". Tokio's `RwLock` is write-preferring, so a
/// pending delete stops admitting new readers instead of starving behind a
/// stream of short writes.
///
/// This does NOT replace [`INDEX_LOCKS`], which keeps its unchanged job of
/// serialising reindex against deferred-embed against component catch-up.
///
/// EVERY path that mutates `handle.indexer` or the index's on-disk data:
///
/// | Path | Entry point | Guarded by |
/// |---|---|---|
/// | `POST /indexes/:id/index-file` | `server::files::index_file_handler` | read side, added #3049 rd2 |
/// | `POST /indexes/:id/remove-file` | `server::files::remove_file_handler` | read side, added #3049 rd2 |
/// | `POST /indexes/:id/graph` | `server::contrib_graph::ingest_graph_handler` | read side, added #3049 rd2 |
/// | `PATCH /indexes/:id` (relocate) | `server::indexes_relocate::relocate_index_handler` | read side, added #3049 rd2 |
/// | filesystem watcher | `service::watch_loop::{handle_modified, handle_removed}` | read side, added #3049 rd2; `handle_modified` widened to cover its stale-chunk removal in rd4 |
/// | boot reconcile | `service::reconcile` | read side, added #3049 rd2 |
/// | full reindex | `reindex::runner::run_reindex` | read side, added #3049 rd2 (also holds [`INDEX_LOCKS`]) |
/// | deferred embed | `reindex::defer_embed_queue` | read side, added #3049 rd2 (also holds [`INDEX_LOCKS`]) |
/// | `PATCH /indexes/:id/config` catch-up | `server::components::spawn_component_catch_up` | read side, added #3049 rd3 (also holds [`INDEX_LOCKS`]) |
/// | `POST /indexes` (recreate) | `server::indexes::create_index_handler` | read side, added #3049 rd3 |
/// | startup schema migration | `commands::start::daemon` spawn → `core::migration::run_migrations` | read side, added #3049 rd4 |
///
/// `create_index` is not a writer of an existing index, but it registers a
/// SECOND generation under the same id and the same on-disk paths, so it belongs
/// on the same lock.
///
/// Deliberately NOT guarded, with reasons:
/// - Read-only handlers (`search`, `grep`, `typeahead`, `status`, `chunks`,
///   `call_chain`, `graph` GETs) never mutate, so a delete racing one costs a
///   failed read, not corruption.
/// - `tickers` idle-eviction / memory reclaim and `shutdown_flush` operate on
///   in-memory caches that are rebuilt from the durable corpus; they write no
///   corpus bytes.
///
/// **This table is prose, and prose was wrong three rounds running.** It was
/// derived by hand-grepping mutating method names across `src/service`, which is
/// how round 2 missed the config catch-up, round 3 missed the startup migration
/// (in `src/commands`, outside the grepped subtree), and all three rounds missed
/// `handle_modified`'s stale-chunk removal (`remove_chunk`, a method name absent
/// from the hand-written grep list). `scripts/check_teardown_guard.sh` now
/// enumerates the call sites mechanically and fails when one is neither guarded
/// in its own scope nor declared in
/// `scripts/teardown-guard-manifest.tsv`; the seed method list lives in
/// `scripts/teardown-guard-methods.tsv`. Add a new write path to the manifest
/// and to this table in the same change — the gate enforces the manifest, and
/// this table is the human-readable view of it.
///
/// Test: `service::server::tests_3049::delete_waits_for_an_ungated_index_file_write`,
/// `service::server::tests_3049::unregister_index_waits_for_an_in_flight_writer_to_release_the_permit`,
/// `service::server::tests_3049::create_index_cannot_register_while_a_delete_is_tearing_the_id_down`.
static INDEX_TEARDOWN_LOCKS: OnceLock<DashMap<IndexId, Arc<tokio::sync::RwLock<()>>>> =
    OnceLock::new();

/// Returns (creating on first use) the teardown lock for `id`.
///
/// Why: one accessor so every writer and the delete reach the same `RwLock`.
/// What: `DashMap::entry` + `or_insert_with`, same shape as [`index_semaphore`].
/// Writers call `.read_owned()`; only `unregister_index` calls `.write_owned()`.
/// Test: see [`INDEX_TEARDOWN_LOCKS`].
pub(crate) fn index_teardown_lock(id: &IndexId) -> Arc<tokio::sync::RwLock<()>> {
    INDEX_TEARDOWN_LOCKS
        .get_or_init(DashMap::new)
        .entry(id.clone())
        .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(())))
        .clone()
}

/// Take the SHARED side of `id`'s teardown lock, re-validating against eviction.
///
/// Why: [`remove_index_teardown_lock`] can evict the map entry while a caller is
/// parked on it, and the guard that caller then gets protects an `Arc` no other
/// operation can reach. Two writers on the same id would each hold a guard on a
/// DIFFERENT `RwLock`, and a delete would see no contention on the live one — the
/// exact false `quiesced: true` this whole mechanism exists to prevent (#3049
/// round 3). `Arc::ptr_eq` against the current map entry is what tells the two
/// apart: an orphan guard is never the entry [`index_teardown_lock`] hands out.
/// What: acquire, then compare; on a mismatch drop the orphan guard and retry
/// against the live entry.
/// Test: `service::server::tests_3049::a_writer_parked_across_teardown_is_visible_to_the_next_delete`.
pub(crate) async fn acquire_index_teardown_read(
    id: &IndexId,
) -> tokio::sync::OwnedRwLockReadGuard<()> {
    // #3049 round 4 (LOW): the loop has no iteration cap or backoff, and the
    // termination argument is not visible from the code, so it is written out
    // here rather than re-derived each review. One retry is the designed worst
    // case: a mismatch means `unregister_index` evicted the entry we were parked
    // on, and eviction happens only there, only while it holds the EXCLUSIVE
    // guard, and only when the quiesce wait succeeded — so the entry we re-fetch
    // was inserted after that delete finished and no delete has claimed it. A
    // second retry needs a second complete delete of the same id to land inside
    // the few instructions between our `index_teardown_lock` call and our
    // `read_owned`, and each such delete costs a registry write plus (for
    // `delete_data=true`) a `remove_dir_all`. Unbounded spinning therefore needs
    // an unbounded delete stream on one id, which is a caller pathology, not a
    // state this function can reach on its own. Nothing in the code PROVES the
    // bound; if a cap is ever added, it must fail the acquire rather than return
    // an orphan guard — returning one reintroduces the false `quiesced: true`
    // this revalidation exists to prevent.
    loop {
        let lock = index_teardown_lock(id);
        let guard = Arc::clone(&lock).read_owned().await;
        if Arc::ptr_eq(&index_teardown_lock(id), &lock) {
            return guard;
        }
        drop(guard);
    }
}

/// Take the EXCLUSIVE side of `id`'s teardown lock, re-validating against
/// eviction. The delete-side counterpart of [`acquire_index_teardown_read`];
/// two concurrent deletes of one id race the same way two writers do.
///
/// Test: see [`acquire_index_teardown_read`].
pub(crate) async fn acquire_index_teardown_write(
    id: &IndexId,
) -> tokio::sync::OwnedRwLockWriteGuard<()> {
    loop {
        let lock = index_teardown_lock(id);
        let guard = Arc::clone(&lock).write_owned().await;
        if Arc::ptr_eq(&index_teardown_lock(id), &lock) {
            return guard;
        }
        drop(guard);
    }
}

/// Remove `id`'s teardown lock once its delete has finished (issue #3049).
///
/// Why: same growth argument as [`remove_index_semaphore`] — the map would
/// otherwise only ever grow, one abandoned `Arc<RwLock>` per deleted id.
/// What: removes the entry if present; no-op otherwise. Two caller obligations,
/// both load-bearing (#3049 rd4): hold the EXCLUSIVE guard across the call, and
/// call it only when the quiesce wait actually SUCCEEDED. The first keeps a
/// racing caller from being handed a fresh lock before the destructive steps
/// have run; the second keeps a writer that outlasted the wait reachable, since
/// evicting around it leaves the next delete unable to see it and free to
/// `remove_dir_all` underneath it. See `unregister_index`.
/// Test: see [`INDEX_TEARDOWN_LOCKS`], plus
/// `service::server::tests_3049::a_timed_out_delete_leaves_the_writers_lock_reachable_to_the_next_delete`.
pub(crate) fn remove_index_teardown_lock(id: &IndexId) {
    if let Some(map) = INDEX_TEARDOWN_LOCKS.get() {
        map.remove(id);
    }
}

/// Process-global registry of per-index cancellation flags (issue #3049).
///
/// Why: [`INDEX_LOCKS`] tells a writer when it may START, and `unregister_index`
/// can now wait on that same permit to learn when every writer has FINISHED.
/// Waiting alone is not enough: a full reindex of a large corpus holds its
/// permit for minutes, so a `DELETE` that only waits either blocks for minutes
/// or times out and deletes the data directory out from under a live writer.
/// This flag is the other half — the delete SIGNALS it before waiting, and the
/// long-running writers poll it at their batch boundaries and stop, so the
/// permit is released in the time it takes to finish one batch instead of the
/// whole corpus.
///
/// What: a `DashMap<IndexId, Arc<AtomicBool>>` mirroring [`INDEX_LOCKS`] —
/// lazily populated, one entry per index, `false` until a delete sets it.
/// Eviction (via [`remove_index_cancel_flag`]) is what keeps a recreated index
/// from being born cancelled: the next [`index_cancel_flag`] call after an
/// eviction allocates a fresh `false` flag.
/// Test: `unregister_index_waits_for_an_in_flight_writer_to_release_the_permit`,
/// `index_cancel_flag_is_evicted_so_a_recreated_index_starts_uncancelled`.
static INDEX_CANCEL_FLAGS: OnceLock<DashMap<IndexId, Arc<AtomicBool>>> = OnceLock::new();

/// Returns (creating on first use) the cancellation flag for `id`.
///
/// Why: a single accessor keeps lazy creation in one place so a writer polling
/// the flag and the delete setting it always reach the same `AtomicBool`.
/// What: `DashMap::entry` + `or_insert_with`, same shape as [`index_semaphore`].
/// Callers should fetch this AFTER acquiring the index permit — a flag set by a
/// delete that has already completed is gone by then, so the fresh flag they
/// get is correctly `false`.
/// Test: see [`INDEX_CANCEL_FLAGS`].
pub(crate) fn index_cancel_flag(id: &IndexId) -> Arc<AtomicBool> {
    INDEX_CANCEL_FLAGS
        .get_or_init(DashMap::new)
        .entry(id.clone())
        .or_insert_with(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

/// Ask every in-flight writer on `id` to stop at its next checkpoint (#3049).
///
/// Why: called by `unregister_index` before it waits on the index permit. Setting
/// the flag first and waiting second is the ordering that matters: a writer that
/// checks the flag after we set it stops promptly, and one that already passed
/// its last checkpoint still holds the permit we are about to wait on, so
/// neither ordering lets the delete race ahead of a live writer.
/// What: sets the (lazily created) flag to `true` with `Release` ordering.
/// Test: see [`INDEX_CANCEL_FLAGS`].
pub(crate) fn signal_index_cancel(id: &IndexId) {
    index_cancel_flag(id).store(true, std::sync::atomic::Ordering::Release);
}

/// Undo [`signal_index_cancel`] on the SAME flag in-flight writers are holding.
///
/// Why: `unregister_index` signals the cancel before it knows whether the delete
/// will succeed. When the quiesce wait expires it now abandons the delete and
/// changes nothing (#3049 round 3) — but the flag it already set would outlive
/// that decision and abort the surviving index's next reindex. Clearing the
/// VALUE rather than evicting the entry is the load-bearing detail:
/// [`remove_index_cancel_flag`] only drops the map entry, so a writer that
/// already cloned the `Arc` would keep reading `true`.
/// What: stores `false` on the (lazily created) flag with `Release` ordering.
/// Test: `service::server::tests_3049::a_second_delete_after_an_abandoned_one_reclaims_the_data`
/// — the second delete only succeeds because the abandoned one cleared the flag
/// it had already set.
pub(crate) fn clear_index_cancel(id: &IndexId) {
    index_cancel_flag(id).store(false, std::sync::atomic::Ordering::Release);
}

/// Remove `id`'s cancellation flag, e.g. once its delete has finished (#3049).
///
/// Why: without eviction the flag stays `true` forever, so an index recreated
/// under the same id would abort its very first reindex. Same growth argument as
/// [`remove_index_semaphore`]: the map would otherwise only ever grow.
/// What: removes the entry if present; no-op when the map was never initialised
/// or `id` was never seen.
/// Test: `index_cancel_flag_is_evicted_so_a_recreated_index_starts_uncancelled`.
pub(crate) fn remove_index_cancel_flag(id: &IndexId) {
    if let Some(map) = INDEX_CANCEL_FLAGS.get() {
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
