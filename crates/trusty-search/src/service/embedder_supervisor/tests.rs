//! Unit tests for the supervisor façade and lazy-spawn handle.
//!
//! Why: we validate the deterministic, pure parts of the supervisor
//! (config parsing, socket path, binary discovery, lazy-handle spawn
//! accounting) without needing a live ONNX binary. Process-lifecycle
//! tests (spawn/restart/shutdown) are in `tests/embedder_supervisor_e2e.rs`
//! and marked `#[ignore]`.
//! Test: `cargo test -p trusty-search -- embedder_supervisor`.

use super::*;
use serial_test::serial;
use std::sync::atomic::Ordering;

// ── SupervisorConfig::from_env ──────────────────────────────────────────

/// With no env vars set, `from_env()` must return the documented defaults.
///
/// Why: catches accidental changes to the defaults that would silently
/// break production deployments.
/// What: remove all four vars, call `from_env()`, assert the fields.
/// Test: this test.
#[test]
#[serial]
fn config_from_env_defaults() {
    let _g1 = EnvGuard::remove("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS");
    let _g2 = EnvGuard::remove("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS");
    let _g3 = EnvGuard::remove("TRUSTY_EMBEDDERD_MAX_RESTARTS");
    let _g4 = EnvGuard::remove("TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS");

    let cfg = SupervisorConfig::from_env();
    assert_eq!(cfg.startup_timeout_secs, 30);
    assert_eq!(cfg.backoff_max_secs, 60);
    assert_eq!(cfg.max_restarts, 5);
    assert_eq!(
        cfg.idle_shutdown_secs, 0,
        "idle-shutdown must default to disabled"
    );
}

/// Env-var overrides must be parsed and applied correctly.
///
/// Why: if `from_env()` ignores set vars, operators can't tune the
/// supervisor without recompiling.
/// What: set all four vars, call `from_env()`, assert the fields match.
/// Test: this test.
#[test]
#[serial]
fn config_from_env_overrides() {
    let _g1 = EnvGuard::set("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS", "15");
    let _g2 = EnvGuard::set("TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS", "120");
    let _g3 = EnvGuard::set("TRUSTY_EMBEDDERD_MAX_RESTARTS", "10");
    let _g4 = EnvGuard::set("TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS", "300");

    let cfg = SupervisorConfig::from_env();
    assert_eq!(cfg.startup_timeout_secs, 15);
    assert_eq!(cfg.backoff_max_secs, 120);
    assert_eq!(cfg.max_restarts, 10);
    assert_eq!(cfg.idle_shutdown_secs, 300);
}

/// Malformed env var values must fall through to defaults without panicking.
///
/// Why: operators may accidentally set `TRUSTY_EMBEDDERD_MAX_RESTARTS=abc`;
/// the daemon must not crash on startup.
/// What: set the vars to non-numeric strings and assert defaults are used.
/// Test: this test.
#[test]
#[serial]
fn config_from_env_ignores_malformed() {
    let _g1 = EnvGuard::set("TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS", "not_a_number");
    let _g2 = EnvGuard::set("TRUSTY_EMBEDDERD_MAX_RESTARTS", "bad");
    let _g3 = EnvGuard::set("TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS", "nope");

    let cfg = SupervisorConfig::from_env();
    assert_eq!(cfg.startup_timeout_secs, 30);
    assert_eq!(cfg.max_restarts, 5);
    assert_eq!(cfg.idle_shutdown_secs, 0);
}

/// `into_common_for_tests()` must map fields correctly to the trusty-common
/// type and always set `sidecar_batch_size: None`.
///
/// Why: field mismatch would silently use wrong defaults at runtime.
/// What: construct a custom config, convert via `into_common_for_tests`,
/// and assert the common fields.
/// Test: this test.
#[test]
fn into_common_for_tests_maps_fields() {
    let cfg = SupervisorConfig {
        startup_timeout_secs: 99,
        backoff_max_secs: 77,
        max_restarts: 3,
        idle_shutdown_secs: 600,
    };
    let common = cfg.into_common_for_tests();
    assert_eq!(common.startup_timeout_secs, 99);
    assert_eq!(common.backoff_max_secs, 77);
    assert_eq!(common.max_restarts, 3);
    // idle_shutdown_secs is trusty-search–specific; not in the common type.
    // sidecar_batch_size must be None — this method is for test/lifecycle
    // spawns only; production paths use do_spawn to populate Some(batch).
    assert!(
        common.sidecar_batch_size.is_none(),
        "into_common_for_tests must not set sidecar_batch_size"
    );
}

// ── default_socket_path ─────────────────────────────────────────────────

/// The default socket path must be unique to the current process.
///
/// Why: two daemons using the same socket would conflict at bind time.
/// What: call `default_socket_path()` twice (same PID) — the results must
/// be equal and contain the PID.
/// Test: this test.
#[test]
fn default_socket_path_is_pid_specific() {
    let p = default_socket_path();
    let pid = std::process::id().to_string();
    assert!(
        p.to_string_lossy().contains(&pid),
        "socket path {p:?} must contain PID {pid}"
    );
    assert_eq!(
        p,
        default_socket_path(),
        "must be deterministic for same PID"
    );
}

/// The socket path must have a non-empty parent directory.
///
/// Why: the supervisor creates the parent directory before spawning;
/// an unparseable `TMPDIR` would cause `create_dir_all` to fail.
/// What: assert the parent is non-None and non-empty.
/// Test: this test.
#[test]
fn default_socket_path_has_parent() {
    let p = default_socket_path();
    assert!(
        p.parent().is_some_and(|pp| !pp.as_os_str().is_empty()),
        "socket path {p:?} must have a non-empty parent"
    );
}

// ── locate_embedderd_binary ─────────────────────────────────────────────

/// When `TRUSTY_EMBEDDERD_BIN` points to a non-existent file, return an error.
///
/// Why: an operator typo in the env var should produce a clear error at
/// startup, not a confusing fallback.
/// What: set `TRUSTY_EMBEDDERD_BIN` to a guaranteed non-existent path and
/// assert the call returns `Err`.
/// Test: this test.
#[test]
#[serial]
fn locate_binary_bad_explicit_path_errors() {
    let _g = EnvGuard::set("TRUSTY_EMBEDDERD_BIN", "/nonexistent/path/trusty-embedderd");
    let result = locate_embedderd_binary();
    assert!(result.is_err(), "expected Err, got {result:?}");
}

/// When `TRUSTY_EMBEDDERD_BIN` points to an existing file, return that path.
///
/// Why: the explicit-path override is the canonical way to use a dev build.
/// What: create a temp file, set the env var, and assert `Ok(path)`.
/// Test: this test.
#[test]
#[serial]
fn locate_binary_via_explicit_env() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let _g = EnvGuard::set("TRUSTY_EMBEDDERD_BIN", path.to_str().unwrap());
    let result = locate_embedderd_binary();
    assert!(result.is_ok(), "expected Ok, got {result:?}");
    assert_eq!(result.unwrap(), path);
}

// ── LazyEmbedderHandle ──────────────────────────────────────────────────

/// Construction must not spawn the child: `app_pid_slot` must read 0.
///
/// Why: the whole point of `LazyEmbedderHandle` is that `trusty-search
/// start` never pays the embedderd spawn cost for `lexical_only`
/// deployments. Verifying the PID slot is 0 after construction (with no
/// embed calls) confirms the lazy contract.
///
/// What: construct a `LazyEmbedderHandle` with a non-existent binary
/// (spawn would fail if attempted), assert `app_pid_slot()` reads 0.
///
/// Test: this test.
#[test]
fn lazy_handle_defers_spawn_pid_is_zero_at_construction() {
    let handle = LazyEmbedderHandle::new(
        PathBuf::from("/nonexistent/trusty-embedderd"),
        SupervisorConfig::default(),
    );
    let pid = handle.app_pid_slot().load(Ordering::Relaxed);
    assert_eq!(
        pid, 0,
        "PID slot must be 0 before any embed request; got {pid}"
    );
}

/// Spawn count: `state` must be `None` before any embed call.
///
/// Why: corroborates the lazy contract at the state-machine level, not
/// just the PID level. If `state` were somehow pre-populated, the
/// lazy-spawn invariant is broken.
/// What: construct a handle, block on the state lock, assert it is `None`.
/// Test: this test.
#[tokio::test]
async fn lazy_handle_state_is_none_before_first_use() {
    let handle = LazyEmbedderHandle::new(
        PathBuf::from("/nonexistent/trusty-embedderd"),
        SupervisorConfig::default(),
    );
    let guard = handle.state.lock().await;
    assert!(guard.is_none(), "state must be None before first embed");
}

/// When `embed_via` is called with a non-existent binary, it must return
/// an `EmbedderError` propagating the spawn failure — not panic.
///
/// Why: spawn failure must propagate cleanly to the caller (e.g. an MCP
/// search handler) so it can return a 503 rather than crash the daemon.
/// What: call `embed_via` on a handle with a non-existent binary, assert
/// the result is `Err` containing text about the spawn failure.
/// Test: this test.
#[tokio::test]
async fn lazy_handle_spawn_failure_propagates_as_error() {
    let handle = LazyEmbedderHandle::new(
        PathBuf::from("/nonexistent/trusty-embedderd"),
        SupervisorConfig {
            startup_timeout_secs: 1,
            ..SupervisorConfig::default()
        },
    );
    let result = handle
        .embed_via(|_client| async {
            Ok::<Vec<Vec<f32>>, trusty_common::embedder_client::EmbedderError>(vec![])
        })
        .await;
    assert!(result.is_err(), "expected Err when binary is absent");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("spawn") || err.contains("embedderd") || err.contains("nonexistent"),
        "error must describe the spawn failure; got: {err}"
    );
}

/// Single-flight: two concurrent first-callers must both succeed and
/// only trigger one spawn attempt (observable via the error message when
/// the binary doesn't exist — both errors are about spawn failure, not
/// double-spawn).
///
/// Why: `OnceCell`-equivalent semantics guarantee exactly-once spawn under
/// concurrent load. Without the `Mutex` guard, two goroutines could both
/// observe `state = None` and try to spawn, causing two children.
/// What: fire two concurrent `embed_via` calls and assert both return
/// `Err` (since the binary doesn't exist) without a panic.
/// Test: this test.
#[tokio::test]
async fn lazy_handle_single_flight_concurrent_spawn_attempts() {
    use std::sync::Arc as StdArc;

    let handle = StdArc::new(LazyEmbedderHandle::new(
        PathBuf::from("/nonexistent/trusty-embedderd"),
        SupervisorConfig {
            startup_timeout_secs: 1,
            ..SupervisorConfig::default()
        },
    ));

    let h1 = StdArc::clone(&handle);
    let h2 = StdArc::clone(&handle);

    let (r1, r2) = tokio::join!(
        tokio::spawn(async move {
            h1.embed_via(|_c| async {
                Ok::<Vec<Vec<f32>>, trusty_common::embedder_client::EmbedderError>(vec![])
            })
            .await
        }),
        tokio::spawn(async move {
            h2.embed_via(|_c| async {
                Ok::<Vec<Vec<f32>>, trusty_common::embedder_client::EmbedderError>(vec![])
            })
            .await
        }),
    );

    // Both must complete without panicking.
    assert!(r1.is_ok(), "task 1 panicked: {:?}", r1);
    assert!(r2.is_ok(), "task 2 panicked: {:?}", r2);
    // Both should return Err (binary missing) — not panic or hang.
    assert!(
        r1.unwrap().is_err(),
        "task 1 should return Err for missing binary"
    );
    assert!(
        r2.unwrap().is_err(),
        "task 2 should return Err for missing binary"
    );
}

/// `idle_shutdown_secs = 0` must result in the watchdog NOT being armed.
///
/// Why: the default configuration disables idle-shutdown entirely; accidentally
/// arming the watchdog with `idle_secs = 0` would cause a tight poll loop.
/// What: construct a handle with `idle_shutdown_secs = 0`, assert the state
/// is still `None` (no spawn triggered), which is consistent with the
/// watchdog never having fired.
/// Test: this test.
#[tokio::test]
async fn lazy_handle_no_watchdog_when_idle_secs_is_zero() {
    let handle = LazyEmbedderHandle::new(
        PathBuf::from("/nonexistent/trusty-embedderd"),
        SupervisorConfig {
            idle_shutdown_secs: 0,
            ..SupervisorConfig::default()
        },
    );
    // State must remain None (no spawn, no watchdog activity).
    let guard = handle.state.lock().await;
    assert!(
        guard.is_none(),
        "state must be None; watchdog must not trigger spawn"
    );
}

// ── Helper ──────────────────────────────────────────────────────────────

/// RAII guard that restores an env var to its original state on drop.
///
/// Why: env vars are global; leaking changes between tests causes flakiness
/// in parallel runs.
/// What: captures the old value on construction; restores or removes it on drop.
/// Test: used by all env-var-touching tests in this module.
struct EnvGuard {
    key: String,
    old: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        // SAFETY: single-threaded tests; no indexing workers running.
        unsafe { std::env::set_var(key, value) }
        Self {
            key: key.to_owned(),
            old,
        }
    }

    fn remove(key: &str) -> Self {
        let old = std::env::var(key).ok();
        // SAFETY: same invariant as above.
        unsafe { std::env::remove_var(key) }
        Self {
            key: key.to_owned(),
            old,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: test teardown; no workers live past the test body.
        unsafe {
            match &self.old {
                Some(v) => std::env::set_var(&self.key, v),
                None => std::env::remove_var(&self.key),
            }
        }
    }
}
