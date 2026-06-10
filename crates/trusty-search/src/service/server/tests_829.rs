//! Regression tests for issue #829: pid-slot task leak, ungraceful admin_stop,
//! and blocking canonicalize in async handlers.
//!
//! Why: three pre-existing bugs in the trusty-search daemon required targeted
//! fixes; this file documents the expected behaviour after each fix so a future
//! refactor cannot silently regress them.
//! What: three test groups — one per bug.
//! Test: `cargo test -p trusty-search -- tests_829` runs all tests in this file.
use super::*;
use axum::extract::State;
use axum::Json;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Issue #829 — Bug 1: pid-slot forwarder task leak
// ---------------------------------------------------------------------------

/// Issue #829 — each call to `install_embedderd_pid_slot` must cancel the
/// previously-running forwarder task before spawning a new one.
///
/// Why: on idle-shutdown cycles the sidecar is re-spawned with a fresh
/// `Arc<AtomicU32>` PID slot. The old forwarder's slot never resets to 0
/// (the supervisor moves on to a new slot), so without cancellation the old
/// task loops forever — one leak per lifecycle. The fix stores the previous
/// task's `AbortHandle` and aborts it before spawning a replacement.
///
/// What: calls `install_embedderd_pid_slot` twice with distinct slots. After
/// the second call the PID stored in `embedderd_pid_slot` must reflect the
/// NEW slot's value, confirming the new forwarder is active and the old one
/// has been superseded.
///
/// Test: this test (tokio).
#[tokio::test]
async fn pid_slot_forwarder_does_not_leak_tasks() {
    use crate::core::registry::IndexRegistry;
    use std::sync::atomic::{AtomicU32, Ordering};

    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));

    // First install: spawn forwarder #1.
    let slot1 = Arc::new(AtomicU32::new(42));
    state.install_embedderd_pid_slot(Arc::clone(&slot1)).await;

    // The handle must be recorded after first install.
    {
        let guard = state.embedderd_pid_forwarder_handle.lock().await;
        assert!(
            guard.is_some(),
            "after first install, abort handle must be Some"
        );
    }

    // Second install: the first forwarder should be aborted.
    let slot2 = Arc::new(AtomicU32::new(99));
    state.install_embedderd_pid_slot(Arc::clone(&slot2)).await;

    // After a brief yield the forwarder has had time to propagate the new PID.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // The PID stored in embedderd_pid_slot must reflect the NEW slot (99),
    // not the old slot (42), confirming the new forwarder is active.
    let stored = state.embedderd_pid_slot.load(Ordering::Relaxed);
    assert_eq!(
        stored, 99,
        "embedderd_pid_slot must reflect the second slot's PID after re-install"
    );
}

// ---------------------------------------------------------------------------
// Issue #829 — Bug 2: graceful admin_stop via watch channel
// ---------------------------------------------------------------------------

/// Issue #829 — `POST /admin/stop` must signal the graceful-shutdown watch
/// channel rather than calling `std::process::exit(0)`.
///
/// Why: `process::exit` bypasses Rust destructors and the redb index flush,
/// which can corrupt the on-disk corpus. Using a `watch` channel lets
/// `run_daemon` handle the stop via the same drain-and-flush path as SIGTERM.
///
/// What: calls `admin_stop_handler` and asserts (a) the response is `200 ok`,
/// (b) `shutdown_tx` has been triggered (the receiver sees the updated value).
///
/// Test: this test (tokio).
#[tokio::test]
async fn admin_stop_triggers_graceful_shutdown() {
    use crate::core::registry::IndexRegistry;

    let state = Arc::new(SearchAppState::new(IndexRegistry::new()));
    // Subscribe to the shutdown channel BEFORE calling the handler.
    let mut shutdown_rx = state.shutdown_tx.subscribe();

    let Json(resp) = admin_stop_handler(State(Arc::clone(&state))).await;
    assert_eq!(
        resp.get("ok"),
        Some(&serde_json::json!(true)),
        "admin_stop must return ok:true"
    );

    // The channel must have been signalled. `changed()` resolves immediately
    // because the value was already updated by the handler before we poll.
    let changed =
        tokio::time::timeout(std::time::Duration::from_millis(500), shutdown_rx.changed()).await;
    assert!(
        changed.is_ok(),
        "shutdown channel must be signalled within 500 ms"
    );
    assert!(
        *shutdown_rx.borrow(),
        "shutdown_tx value must be true after admin_stop"
    );
}

// ---------------------------------------------------------------------------
// Issue #829 — Bug 3: non-blocking validate_root_path
// ---------------------------------------------------------------------------

/// Issue #829 — `validate_root_path` must be async and use tokio's non-blocking
/// filesystem operations rather than `std::fs::canonicalize` / `path.is_dir()`.
///
/// Why: the previous sync version called blocking syscalls on the tokio async
/// thread, parking the executor. The fix uses `tokio::fs::canonicalize` and
/// `tokio::fs::metadata` which offload to the blocking pool.
///
/// What: calls `validate_root_path` from an async context for three classes of
/// input — (a) empty, (b) relative, (c) non-existent absolute — and asserts
/// each returns `Err` without panicking or deadlocking. The function signature
/// being `async fn` is the primary assertion; if it were still sync the tests
/// would not compile.
///
/// Test: this test (tokio).
#[tokio::test]
async fn validate_root_path_is_non_blocking_and_async() {
    use std::path::Path;

    // Empty path — rejected before any I/O (fast path).
    let result = super::helpers::validate_root_path(Path::new("")).await;
    assert!(result.is_err(), "empty path must be rejected");

    // Relative path — rejected before any I/O (no is_absolute check, fast path).
    let result = super::helpers::validate_root_path(Path::new("relative/path")).await;
    assert!(result.is_err(), "relative path must be rejected");

    // Non-existent absolute path — exercises the async metadata check.
    let result =
        super::helpers::validate_root_path(Path::new("/this/path/does/not/exist/issue829")).await;
    assert!(result.is_err(), "non-existent path must be rejected");
}

/// Issue #829 — `file_is_within_root` slow path uses `block_in_place` for the
/// `canonicalize` call rather than parking the async executor thread.
///
/// Why: the slow path (absolute file path that doesn't lexically match root)
/// calls `std::fs::canonicalize(root)` to resolve symlink aliases. Without
/// wrapping in `block_in_place`, this blocks the tokio worker thread for the
/// duration of the syscall. The fix wraps the slow path canonicalize so the
/// runtime can reschedule other async tasks during the I/O.
///
/// What: calls `file_is_within_root` with an absolute path that triggers the
/// slow path (lexical check fails) and asserts the return value is correct.
/// `flavor = "multi_thread"` is required because `block_in_place` panics on
/// the single-threaded (`current_thread`) runtime — the same constraint as the
/// production daemon, which always uses the multi-thread builder.
///
/// Test: this test (tokio multi-thread).
#[tokio::test(flavor = "multi_thread")]
async fn file_is_within_root_slow_path_does_not_block_executor() {
    use super::helpers::file_is_within_root;
    use std::path::Path;

    // An absolute file path that is OUTSIDE the root triggers the slow path
    // (the lexical `p.starts_with(root)` check fails first, then canonicalize).
    // The canonicalize of the root path may fail (if the test root doesn't
    // exist) — that's fine; the function returns `false` and must not panic.
    let root = Path::new("/non/existent/root");
    let result = file_is_within_root("/non/existent/other/file.rs", root);
    // Root doesn't exist, canonicalize fails → returns false.
    assert!(!result, "non-existent root must return false");

    // Relative path: no canonicalize needed, fast path.
    let result = file_is_within_root("src/lib.rs", root);
    assert!(result, "clean relative path must be accepted");
}
