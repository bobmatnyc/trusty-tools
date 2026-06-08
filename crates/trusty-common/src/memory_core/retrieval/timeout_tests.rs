//! Tests that the bounded-timeout wrappers on the memory hot path actually
//! fire (issue #906).
//!
//! Why: issue #906 required that every memory operation return within a bounded
//! time or an explicit error. Without a test that watches the timeout fire
//! we cannot be confident the wrappers are on the right await points.
//! What: Two flavours of test:
//!   1. `timeout_fires_on_embedder_init_with_tiny_limit` — sets
//!      `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS=0` (instant expiry) and
//!      calls `shared_embedder()`, which *would* cold-init a real ONNX
//!      session (potentially blocking for minutes on CoreML). Instead it
//!      must return an explicit timeout error within milliseconds.
//!   2. `remember_succeeds_with_mock_embedder` — seeds the shared cell with
//!      `MockEmbedder` (no ONNX download) and performs a full `remember`
//!      round-trip, asserting success with normal timeouts.
//!
//! WARNING: test (1) mutates a process-wide `OnceCell`. Because the cell can
//! only be set once, the two tests must not run concurrently and the timeout
//! test must run with an UNINITIALIZED cell. Rust's test harness does not
//! guarantee test isolation between `#[tokio::test]` tests in the same binary,
//! so each test is annotated `#[ignore]` and driven individually:
//!
//!   cargo test -p trusty-common --features memory-core \
//!     retrieval::timeout_tests::timeout_fires_on_embedder_init_with_tiny_limit \
//!     -- --include-ignored
//!
//! The `remember_succeeds_with_mock_embedder` test runs in the normal suite
//! (not ignored) because it only requires the mock path.
//!
//! Test: This file IS the test — see module docs for run instructions.

#[cfg(test)]
mod tests {
    use crate::memory_core::palace::{Palace, PalaceId};
    use crate::memory_core::retrieval::{PalaceHandle, shared_embedder};
    use tempfile::tempdir;

    /// Why: Verify that `shared_embedder()` returns an explicit error (not a
    /// hang) when the init ceiling is set to 0 seconds. This is the primary
    /// regression guard for issue #906 on the embedder-init path.
    /// What: Sets `TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS=0` so the timeout fires
    /// immediately, then calls `shared_embedder()` and asserts the result is
    /// an `Err` containing the timeout message.
    /// Test: itself. Marked `#[ignore]` because it mutates the process-wide
    /// `SHARED_EMBEDDER` cell — run with `--include-ignored` in isolation.
    #[tokio::test]
    #[ignore = "mutates process-wide OnceCell; run in isolation with --include-ignored"]
    async fn timeout_fires_on_embedder_init_with_tiny_limit() {
        // Force a 0-second timeout so the init times out before it can succeed.
        // SAFETY: single-threaded async test; env mutation safe here.
        unsafe {
            std::env::set_var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS", "0");
        }
        let result = shared_embedder().await;
        unsafe {
            std::env::remove_var("TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS");
        }
        // `expect_err` requires T: Debug which Arc<dyn Embedder> doesn't
        // satisfy, so we match instead.
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("shared_embedder() must return Err when init times out, got Ok"),
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("timed out") || msg.contains("FastEmbedder"),
            "error message must mention timeout: {msg}"
        );
    }

    /// Why: Confirm the full remember path succeeds with normal timeouts and
    /// a mock embedder (no ONNX download). This is the green-path regression
    /// guard: adding timeout wrappers must not break ordinary operation.
    /// What: Seeds `SHARED_EMBEDDER` with `MockEmbedder`, creates a
    /// temporary palace, calls `handle.remember(...)`, and asserts success.
    /// Test: itself.
    #[tokio::test]
    async fn remember_succeeds_with_mock_embedder() {
        // Seed the shared cell with the mock so no ONNX download is attempted.
        crate::memory_core::retrieval::seed_shared_embedder_with_mock();

        let dir = tempdir().unwrap();
        let palace = Palace {
            id: PalaceId::new("timeout-green"),
            name: "Timeout green".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("timeout-green"),
        };
        std::fs::create_dir_all(&palace.data_dir).unwrap();
        let handle = PalaceHandle::open(&palace).unwrap();

        let result = handle
            .remember(
                "bounded timeout green path".into(),
                crate::memory_core::palace::RoomType::General,
                vec![],
                0.5,
            )
            .await;

        assert!(
            result.is_ok(),
            "remember must succeed with mock embedder: {result:?}"
        );
    }

    /// Why: Verify that the write-lock timeout returns an explicit error when
    /// the lock is held indefinitely (simulated by holding it across an async
    /// sleep that exceeds the configured timeout).
    /// What: Acquires the per-palace `write_mutex` in a background task,
    /// then sets `TRUSTY_WRITE_LOCK_TIMEOUT_SECS=0` and attempts `remember`,
    /// which must time out on lock acquisition.
    /// Test: itself.
    #[tokio::test]
    async fn write_lock_timeout_returns_error_when_held() {
        crate::memory_core::retrieval::seed_shared_embedder_with_mock();

        let dir = tempdir().unwrap();
        let palace = Palace {
            id: PalaceId::new("lock-timeout"),
            name: "Lock timeout".into(),
            description: None,
            created_at: chrono::Utc::now(),
            data_dir: dir.path().join("lock-timeout"),
        };
        std::fs::create_dir_all(&palace.data_dir).unwrap();
        let handle = PalaceHandle::open(&palace).unwrap();

        // Hold the write lock in a background task indefinitely until we
        // signal it to release.
        let mutex = handle.write_mutex_for_test();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let holder = tokio::spawn(async move {
            let _guard = mutex.lock().await;
            // Hold the lock until released by the test.
            let _ = release_rx.await;
        });
        // Give the holder a moment to acquire.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Set a 0-second write-lock timeout so the next `remember` times out
        // immediately on lock acquisition.
        unsafe { std::env::set_var("TRUSTY_WRITE_LOCK_TIMEOUT_SECS", "0") };
        let result = handle
            .remember(
                "should time out on lock".into(),
                crate::memory_core::palace::RoomType::General,
                vec![],
                0.5,
            )
            .await;
        unsafe { std::env::remove_var("TRUSTY_WRITE_LOCK_TIMEOUT_SECS") };

        // Release the holder so it doesn't leak.
        let _ = release_tx.send(());
        let _ = holder.await;

        assert!(
            result.is_err(),
            "remember must return Err when write-lock times out, got Ok"
        );
        let err = result.expect_err("checked above");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("timed out") || msg.contains("write-lock"),
            "error must mention lock timeout: {msg}"
        );
    }
}
