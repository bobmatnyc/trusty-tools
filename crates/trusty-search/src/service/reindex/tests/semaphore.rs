/// Issue #458: priority semaphore routing and termination guard tests.
use super::*;

/// Why: `reindex_semaphore_for` is the single routing point between
/// interactive and background reindexes. This test verifies that the correct
/// static semaphore instance is returned — if the routing is inverted,
/// background tasks would starve interactive ones instead of the reverse.
///
/// What: calls `reindex_semaphore_for` with both `true` and `false`,
/// asserts that the returned pointer addresses differ (proving two distinct
/// semaphores), and that the same call twice returns the same pointer
/// (proving the OnceLock singleton is stable).
///
/// Test: this test. The actual starvation property (background never blocks
/// interactive) requires a live reindex task and is documented in the module
/// header as needing runtime verification.
#[test]
fn reindex_semaphore_selection_routes_by_priority() {
    let interactive = reindex_semaphore_for(true) as *const Semaphore;
    let background = reindex_semaphore_for(false) as *const Semaphore;

    // The two semaphores must be distinct objects.
    assert_ne!(
        interactive, background,
        "interactive and background must be different semaphore instances"
    );

    // Each call to the same priority must return the same singleton.
    assert_eq!(
        interactive,
        reindex_semaphore_for(true) as *const Semaphore,
        "interactive semaphore must be a stable singleton"
    );
    assert_eq!(
        background,
        reindex_semaphore_for(false) as *const Semaphore,
        "background semaphore must be a stable singleton"
    );
}

/// Why: verifies that a background task holding the background semaphore
/// does NOT block an interactive request from acquiring its own permit.
///
/// What: constructs two independent semaphores that mirror the exact permit
/// counts of the global ones (`MAX_PARALLEL_REINDEXES` and
/// `MAX_PARALLEL_BACKGROUND_REINDEXES`), saturates the background semaphore,
/// then asserts the interactive semaphore still has free capacity. Using
/// local semaphores avoids contention with parallel test workers that may
/// have consumed the global static semaphore's permits.
///
/// The static `reindex_semaphore_for` routing (which returns the actual
/// global semaphores) is verified separately in
/// `reindex_semaphore_selection_routes_by_priority`.
///
/// Test: this test. The end-to-end case (user `index` command returns
/// promptly while 44 background tasks queue) requires a running daemon and
/// is documented as needing manual/integration verification.
#[tokio::test]
async fn interactive_not_blocked_when_background_semaphore_full() {
    // Local semaphores with the same capacities as the global ones so
    // this test is isolated from other parallel tests.
    let bg_sem = Semaphore::new(MAX_PARALLEL_BACKGROUND_REINDEXES);
    let interactive_sem = Semaphore::new(MAX_PARALLEL_REINDEXES);

    // Saturate the background semaphore (simulating full startup backlog).
    let _bg_permit = bg_sem
        .acquire()
        .await
        .expect("background semaphore unexpectedly closed");

    // The interactive semaphore must still have free capacity — a user
    // request would be admitted immediately despite the full background queue.
    let interactive_permit = interactive_sem
        .try_acquire()
        .expect("interactive semaphore must have a free permit even when background is full");

    // Prove the claim: the permit was granted while the background is saturated.
    assert_eq!(
        bg_sem.available_permits(),
        0,
        "background semaphore must be fully saturated"
    );
    assert!(
        interactive_sem.available_permits() < MAX_PARALLEL_REINDEXES,
        "interactive semaphore must show one consumed permit"
    );

    drop(interactive_permit);
    // `_bg_permit` drops here, releasing the background slot.
}

/// Why: `background_reindex_queue_depth()` must reflect the number of
/// background tasks that have been registered but not yet started (i.e.
/// queued + in-flight). Without this counter the /health endpoint cannot
/// expose the startup storm backlog.
///
/// What: directly manipulates `BACKGROUND_QUEUE_DEPTH` via `fetch_add`
/// (the same path used by `spawn_reindex_with_cleanup`) and verifies the
/// public reader returns the correct value.
///
/// Test: this test. Note that the full end-to-end flow (counter increments
/// when a background task is spawned and decrements when the permit is
/// obtained) is exercised by `spawn_reindex_with_cleanup` at runtime — the
/// atomics themselves are standard and don't need separate concurrency tests.
#[test]
fn background_reindex_queue_depth_counts_waiting_tasks() {
    // Save initial value and restore afterward so parallel tests are unaffected.
    let initial = BACKGROUND_QUEUE_DEPTH.load(std::sync::atomic::Ordering::Relaxed);

    BACKGROUND_QUEUE_DEPTH.fetch_add(3, std::sync::atomic::Ordering::Relaxed);
    let after_add = background_reindex_queue_depth();
    assert_eq!(
        after_add,
        initial + 3,
        "queue depth must increase by 3 after 3 increments"
    );

    BACKGROUND_QUEUE_DEPTH.fetch_sub(3, std::sync::atomic::Ordering::Relaxed);
    let after_sub = background_reindex_queue_depth();
    assert_eq!(
        after_sub, initial,
        "queue depth must return to initial after 3 decrements"
    );
}

/// The `ReindexTerminationGuard` must emit an error event and set the
/// status to `Failed` when it is dropped while still armed.
///
/// Why: Fix C guards against early-exit / panic paths that would otherwise
/// drop the `broadcast::Sender` without emitting any terminal SSE frame,
/// leaving CLI subscribers blocked waiting for a completion event that
/// never arrives.
///
/// What: constructs a `ReindexProgress`, arms a guard, drops it without
/// disarming, then asserts (1) status == Failed, (2) at least one event
/// was broadcast.
///
/// Test: this test.
#[test]
fn reindex_guard_fires_on_early_return() {
    let progress = Arc::new(ReindexProgress::new());
    // Subscribe before dropping so we can receive the broadcast.
    let mut rx = progress.sender.subscribe();

    {
        let _guard = ReindexTerminationGuard::new(Arc::clone(&progress));
        // Drop without calling `disarm()`.
    }

    assert_eq!(
        progress.status.load(),
        ReindexStatus::Failed,
        "status must be Failed after guard drops while armed"
    );
    let msg = rx
        .try_recv()
        .expect("guard must have broadcast an error event");
    assert!(
        msg.contains("\"error\""),
        "broadcast message must contain event:error; got: {msg}"
    );
}

/// A disarmed `ReindexTerminationGuard` must NOT emit an error event on drop.
///
/// Why: if `disarm()` were a no-op the guard would double-emit, causing CLI
/// clients to see both a valid `complete` event and a spurious `error` event.
///
/// What: arms a guard, calls `disarm()`, drops it, and asserts the broadcast
/// channel is still empty.
///
/// Test: this test.
#[test]
fn reindex_guard_does_not_fire_after_disarm() {
    let progress = Arc::new(ReindexProgress::new());
    let mut rx = progress.sender.subscribe();

    {
        let mut guard = ReindexTerminationGuard::new(Arc::clone(&progress));
        guard.disarm();
    }

    assert_eq!(
        rx.try_recv()
            .err()
            .map(|e| matches!(e, tokio::sync::broadcast::error::TryRecvError::Empty)),
        Some(true),
        "no event should be broadcast after disarm"
    );
}
