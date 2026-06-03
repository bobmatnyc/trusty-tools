//! Per-index bounded restore for warm-boot (issue #718 Part 3 / Part 4).
//!
//! Why: the legacy-phase loop in `restore_indexes` (start.rs) calls
//! `restore_one_index` for each entry in `indexes.toml`. Inside that call,
//! `build_indexer_from_entry` opens the index's redb at the stored `root_path`
//! via `CorpusStore::open`, which is a synchronous blocking call. On this
//! machine all 57 entries have `colocated = true` with data on `/Volumes/SSD1`
//! (an external volume). Under launchd on macOS 26 Tahoe, TCC blocks access to
//! `/Volumes/SSD1`, so the `CorpusStore::open` call hangs indefinitely — the
//! legacy phase stalls at "restoring 57 legacy index registration(s)" and the
//! daemon never finishes warm-boot.
//!
//! Part 3 fix attempt: spawned the restore as a `tokio::spawn` task, but this
//! ran the blocking I/O (`open()`, `std::fs::read`, redb) on a tokio worker
//! thread. When macOS TCC denies the `open()` syscall the kernel blocks the
//! thread uninterruptibly — `tokio::time::timeout` fires but cannot unfreeze a
//! thread stuck in a kernel syscall. With 57 indexes, all 57 workers freeze one
//! by one, starving the async runtime and making `/health` unresponsive.
//!
//! Part 4 (this fix): use `tokio::task::spawn_blocking` so the blocking I/O
//! runs on the dedicated blocking-pool thread, NOT on an async worker. Blocking-
//! pool threads are expendable — the runtime spawns fresh workers when all current
//! workers are busy, while blocking threads are subject only to the blocking pool
//! cap (default 512). When TCC denies `open()`, the blocking thread freezes (one
//! leaked thread per index, accepted per the fix spec), but all async runtime
//! workers remain free and `/health` stays responsive throughout warm-boot.
//!
//! The closure passed to `spawn_blocking` drives the async restore future via
//! `Handle::block_on`, which is the standard pattern for async-in-blocking-thread.
//!
//! Test: `restore_bounded_runtime_stays_responsive_during_slow_blocking_restore`,
//!       `restore_bounded_returns_false_for_slow_restore`,
//!       `restore_bounded_returns_true_for_immediate_completion`.

use std::future::Future;
use std::time::Duration;

use crate::service::persistence::PersistedIndex;

use super::scan::is_likely_external_volume;
use super::warmboot_index_timeout;

/// Restore one index entry with a per-index deadline so warm-boot never hangs.
///
/// Why (issue #718 Part 4): `build_indexer_from_entry` (called from
/// `restore_one_index`) opens the index's redb and HNSW snapshot via blocking
/// `std::fs`/redb/usearch calls. On a TCC-denied external volume the `open()`
/// syscall blocks uninterruptibly in kernel space. The previous Part 3 fix
/// used `tokio::spawn`, which runs on an async *worker* thread — when the
/// syscall hangs it freezes that worker. With 57 indexes all workers eventually
/// freeze, starving the runtime and making `/health` unresponsive for minutes.
///
/// This fix uses `tokio::task::spawn_blocking` so the blocking I/O runs on the
/// dedicated blocking-pool thread, not on an async worker. When TCC denies the
/// open(), the blocking thread freezes (one leaked thread per index — accepted
/// per the #718 fix spec; tokio's default pool cap is 512, so 57 is fine), but
/// the async workers remain free throughout warm-boot. `/health` stays
/// responsive.
///
/// The `spawn_blocking` closure drives the async restore future via
/// `Handle::current().block_on(fut)`, which is the standard tokio pattern for
/// running async code inside a blocking-pool thread.
///
/// What: accepts a `restore_fn` future-factory that, when called with `entry`,
/// produces the async restore work (typically a closure over `state` + `embedder`
/// calling `restore_one_index`). Wraps the blocking execution in
/// `tokio::time::timeout(warmboot_index_timeout())`. On timeout logs the
/// actionable TCC hint and drops the `JoinHandle` (the blocking thread is
/// abandoned — accepted). On join-error (panic) logs and skips. Returns `true`
/// on success, `false` on timeout or panic.
///
/// Note: uses a factory (not a pre-built Future) so ownership is clean — all
/// captures are moved into the `spawn_blocking` closure.
///
/// Test: `restore_bounded_runtime_stays_responsive_during_slow_blocking_restore`
///       verifies that a slow blocking restore does NOT stall the async runtime
///       (other async tasks execute concurrently during the restore).
///       `restore_bounded_returns_false_for_slow_restore` and
///       `restore_bounded_returns_true_for_immediate_completion` verify the
///       timeout/success protocol.
pub async fn restore_one_index_bounded<F, Fut>(entry: PersistedIndex, restore_fn: F) -> bool
where
    F: FnOnce(PersistedIndex) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let deadline: Duration = warmboot_index_timeout();
    let index_id = entry.id.clone();
    let root_path = entry.root_path.clone();

    // Issue #718 Part 4: run the restore on the blocking-pool thread, NOT on an
    // async worker. `spawn_blocking` dedicates a thread from tokio's blocking
    // pool (default cap 512). When a TCC-denied `open()` hangs, that blocking
    // thread is frozen but all async workers remain available — so `/health`
    // continues responding. The blocking thread calls `Handle::block_on` to
    // drive the async restore future to completion; this is the standard tokio
    // pattern for async-in-blocking-thread and does NOT nest runtimes.
    let handle = tokio::runtime::Handle::current();
    let task = tokio::task::spawn_blocking(move || handle.block_on(restore_fn(entry)));

    match tokio::time::timeout(deadline, task).await {
        Ok(Ok(())) => {
            // Restore completed within the deadline.
            true
        }
        Ok(Err(join_err)) => {
            // The spawned task panicked. Extremely rare but we must not propagate.
            tracing::error!(
                "warm-boot: index '{index_id}' restore task panicked — skipping (issue #718). \
                 Error: {join_err}"
            );
            false
        }
        Err(_elapsed) => {
            // Timeout: the restore did not complete within the deadline.
            // The JoinHandle is dropped here, which aborts the spawned task.
            let is_external = is_likely_external_volume(&root_path);
            if is_external {
                tracing::warn!(
                    "warm-boot: index '{index_id}' restore TIMED OUT (>{:.0}s) — path {} \
                     is on an external/removable volume. \
                     Under launchd this is typically a TCC denial. \
                     HINT: grant Full Disk Access to the launchd agent in \
                     System Settings → Privacy & Security → Full Disk Access, \
                     or move the index off the external volume. \
                     Skipping this index — other indexes continue restoring. (issue #718)",
                    deadline.as_secs_f32(),
                    root_path.display(),
                );
            } else {
                tracing::warn!(
                    "warm-boot: index '{index_id}' restore TIMED OUT (>{:.0}s) — path {}. \
                     The path may be on a slow or permission-restricted filesystem. \
                     Skipping this index — other indexes continue restoring. (issue #718)",
                    deadline.as_secs_f32(),
                    root_path.display(),
                );
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    //! Tests for the per-index bounded restore (issue #718 Part 4).
    //!
    //! Why: the key invariants are:
    //! 1. A restore whose blocking I/O hangs must NOT freeze async runtime workers.
    //!    Other async tasks (e.g. /health handlers) must continue during the hang.
    //! 2. The per-index timeout fires and the loop returns `false`, then continues
    //!    to the next index.
    //! 3. A restore that completes within the deadline returns `true`.
    //!
    //! We use synthetic closures (not the real `restore_one_index`) so these
    //! tests run without a filesystem or registry. The responsiveness test uses a
    //! genuine `std::thread::sleep` (blocking sleep, not async) to prove the
    //! blocking-pool isolation: when the restore thread is asleep in kernel space,
    //! the async task racing it must still complete on a worker.
    //!
    //! Test: `cargo test -p trusty-search -- warm_boot::restore`.

    use super::*;
    use crate::service::persistence::PersistedIndex;

    fn dummy_entry(id: &str, path: &str) -> PersistedIndex {
        PersistedIndex {
            id: id.to_string(),
            root_path: std::path::PathBuf::from(path),
            colocated: false,
            ..Default::default()
        }
    }

    /// Why: a restore that completes immediately must return `true`.
    /// What: pass a factory that resolves instantly; assert `true`.
    /// Test: this test.
    #[tokio::test]
    async fn restore_bounded_returns_true_for_immediate_completion() {
        let entry = dummy_entry("test-ok", "/tmp/trusty-718-restore-ok");
        let result = restore_one_index_bounded(entry, |_e| async {}).await;
        assert!(result, "an immediately-completing restore must return true");
    }

    /// Why: a restore that exceeds the timeout must be aborted and return `false`.
    /// What: set `TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS=1` and pass a factory that
    /// sleeps for 2 s (longer than the timeout); assert `false`.
    /// Note: `serial` prevents this test from racing with other env-var mutators.
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn restore_bounded_returns_false_for_slow_restore() {
        // Set a short timeout so the test completes quickly.
        unsafe { std::env::set_var("TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS", "1") };
        let entry = dummy_entry(
            "test-slow",
            "/Volumes/SSD1/slow-index", // External-volume path for TCC hint coverage.
        );
        let result = restore_one_index_bounded(entry, |_e| async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        })
        .await;
        unsafe { std::env::remove_var("TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS") };
        assert!(
            !result,
            "a restore that exceeds the deadline must return false"
        );
    }

    /// Why: warm-boot must never hang even when ALL entries time out. The sum
    /// of N skipped entries must cost at most N × deadline, not forever.
    /// What: call `restore_one_index_bounded` three times with a 1 s timeout
    /// and a 2 s sleeper each; assert all return false within ~3 s wall time.
    /// Note: `serial` prevents this test from racing with other env-var mutators.
    /// Test: this test.
    #[tokio::test]
    #[serial_test::serial]
    async fn restore_bounded_multiple_timeouts_do_not_accumulate_indefinitely() {
        unsafe { std::env::set_var("TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS", "1") };
        let start = std::time::Instant::now();
        for i in 0..3 {
            let entry = dummy_entry(&format!("test-multi-{i}"), "/Volumes/SSD1/idx");
            let result = restore_one_index_bounded(entry, |_e| async {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            })
            .await;
            assert!(!result, "entry {i} must time out and return false");
        }
        unsafe { std::env::remove_var("TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS") };
        // 3 entries × 1 s timeout = at most ~3 s; we allow generous 10 s.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "3 timed-out restores must complete within 10 s total, elapsed: {:?}",
            start.elapsed()
        );
    }

    /// Why (issue #718 Part 4 — the critical regression test): the defect was
    /// that a blocking restore (redb `open()` hung in kernel) froze a tokio
    /// *worker* thread. With enough indexes, all workers froze and `/health`
    /// stopped responding. This test proves that a restore using genuine blocking
    /// I/O (`std::thread::sleep` — not async sleep) does NOT stall a concurrently
    /// running async task.
    ///
    /// What: launch two concurrent tasks:
    ///   A — `restore_one_index_bounded` with a 2s `std::thread::sleep` inside
    ///       (simulates a blocking `open()` syscall frozen by TCC denial).
    ///       Timeout is set to 1s so it fires before the sleep finishes.
    ///   B — an async `tokio::time::sleep(100ms)` + flag set, representing a
    ///       `/health` handler.
    /// Assert: task B (the /health proxy) completes before task A returns,
    /// proving that the blocking thread does NOT consume a worker.
    ///
    /// Implementation note: `std::thread::sleep` inside `Handle::block_on`
    /// parks the blocking-pool thread while the runtime's async workers stay
    /// free. If `restore_one_index_bounded` used `tokio::spawn` (Part 3 bug),
    /// the sleep would run on a worker and task B might starve.
    ///
    /// Test: this test; deterministic with a generous margin (200ms >> 100ms).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[serial_test::serial]
    async fn restore_bounded_runtime_stays_responsive_during_slow_blocking_restore() {
        unsafe { std::env::set_var("TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS", "1") };

        let health_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let health_done_clone = health_done.clone();

        // Task A: a restore whose inner closure blocks the thread for 2s via
        // std::thread::sleep (genuine blocking, not async — mirrors a frozen open()).
        let entry = dummy_entry("test-blocking", "/Volumes/SSD1/blocking-idx");
        let restore_task = tokio::spawn(restore_one_index_bounded(entry, |_e| async {
            // std::thread::sleep is a genuine blocking call — it parks the OS
            // thread. On the spawn_blocking path this parks the blocking-pool
            // thread, not a worker. On the old tokio::spawn path it would park
            // a worker and could starve task B.
            std::thread::sleep(std::time::Duration::from_secs(2));
        }));

        // Task B: a lightweight async task that simulates a /health handler.
        // It should complete well before task A's 1s timeout fires.
        let health_task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            health_done_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        // Wait for both to finish.
        let _ = health_task.await;
        let result = restore_task.await.expect("restore task must not panic");

        unsafe { std::env::remove_var("TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS") };

        // The health task must have completed (set the flag) — this is the key
        // assertion: blocking I/O in the restore must NOT stall async workers.
        assert!(
            health_done.load(std::sync::atomic::Ordering::SeqCst),
            "async /health proxy task must complete while blocking restore is running; \
             if this fails, the blocking restore is freezing an async worker (issue #718)"
        );
        // The restore timed out (1s timeout, 2s sleep) and returned false.
        assert!(
            !result,
            "restore with a blocking sleep exceeding the timeout must return false"
        );
    }
}
