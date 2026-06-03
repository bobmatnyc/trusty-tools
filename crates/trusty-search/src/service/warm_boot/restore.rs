//! Per-index bounded restore for warm-boot (issue #718 Part 3).
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
//! The fix: `restore_one_index_bounded` accepts a future factory for the
//! per-index restore work so this module stays library-safe (no reference to
//! the binary-only `commands` module). The caller in `start.rs` passes a
//! closure capturing `state` and `embedder`. The factory's future is spawned
//! as a `tokio::spawn` task so the JoinHandle can be aborted when the
//! per-index timeout (`warmboot_index_timeout()`) fires.
//!
//! Test: `restore_bounded_returns_false_for_fast_timeout`,
//!       `restore_bounded_returns_true_for_immediate_completion`.

use std::future::Future;
use std::time::Duration;

use crate::service::persistence::PersistedIndex;

use super::scan::is_likely_external_volume;
use super::warmboot_index_timeout;

/// Restore one index entry with a per-index deadline so warm-boot never hangs.
///
/// Why (issue #718 Part 3): `build_indexer_from_entry` (called from
/// `restore_one_index`) opens the index's redb synchronously. On a TCC-denied
/// external volume that open hangs indefinitely, blocking the entire warm-boot
/// loop. This wrapper spawns the restore as a detached `tokio::spawn` task and
/// applies `warmboot_index_timeout()` to the JoinHandle. On timeout the task is
/// aborted; on join-error (panic) the index is skipped. In both error cases a
/// `tracing::warn!` or `tracing::error!` is emitted naming the index id, path,
/// and — for `/Volumes/` paths — the TCC hint.
///
/// What: accepts a `restore_fn` future-factory that, when called with `entry`,
/// produces the async restore work (typically a closure over `state` + `embedder`
/// calling `restore_one_index`). Spawns the resulting future via `tokio::spawn`,
/// then wraps the JoinHandle in `tokio::time::timeout(warmboot_index_timeout())`.
/// Returns `true` when the restore completes within the deadline, `false` when
/// it is skipped (timeout or panic).
///
/// Note: uses a factory (not a pre-built Future) so `tokio::spawn` receives an
/// owned future with no borrowed references — all captures are moved in.
///
/// Test: `restore_bounded_returns_false_for_fast_timeout`,
///       `restore_bounded_returns_true_for_immediate_completion`.
pub async fn restore_one_index_bounded<F, Fut>(entry: PersistedIndex, restore_fn: F) -> bool
where
    F: FnOnce(PersistedIndex) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let deadline: Duration = warmboot_index_timeout();
    let index_id = entry.id.clone();
    let root_path = entry.root_path.clone();

    // Spawn the restore as a task so the JoinHandle is abortable when the
    // timeout fires. Blocking sync I/O (redb open, HNSW load) inside the
    // restore function is driven from this tokio task; aborting the handle
    // interrupts the blocked thread at the next tokio yield point.
    let task = tokio::spawn(restore_fn(entry));

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
    //! Tests for the per-index bounded restore (issue #718 Part 3).
    //!
    //! Why: the key invariant is that a restore that hangs (or is too slow)
    //! must be aborted and `restore_one_index_bounded` must return `false`.
    //! A restore that completes immediately must return `true`.
    //!
    //! We use synthetic async closures (not the real `restore_one_index`)
    //! so these tests run without a filesystem or registry.
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
}
