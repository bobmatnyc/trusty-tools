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

use std::sync::OnceLock;
use tokio::sync::Semaphore;

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
