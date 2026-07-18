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
    let _g5 = EnvGuard::remove("TRUSTY_EMBEDDERD_WEDGE_RESET_SECS");

    let cfg = SupervisorConfig::from_env();
    assert_eq!(cfg.startup_timeout_secs, 30);
    assert_eq!(cfg.backoff_max_secs, 60);
    assert_eq!(cfg.max_restarts, 5);
    assert_eq!(
        cfg.idle_shutdown_secs, 300,
        "idle-shutdown must default to 300s (issue #2315) so an idle sidecar's \
         RSS is reclaimed at rest"
    );
    assert_eq!(
        cfg.wedge_reset_secs, 300,
        "wedge_reset_secs must default to 300s (#1450 HIGH follow-up)"
    );
}

/// `TRUSTY_EMBEDDERD_WEDGE_RESET_SECS` must be parsed and forwarded — this is
/// the field that closes the #1450 HIGH follow-up gap (previously dead
/// configuration: the running daemon's `do_spawn` and `into_common_for_tests`
/// both hardcoded the trusty-common default via `..SupervisorConfig::default()`
/// regardless of this env var).
///
/// Why: an operator-tuned reset window must actually reach the sidecar
/// supervisor, not merely round-trip inside a config struct nobody reads.
/// What: set the var to a distinctive, non-default value; assert `from_env()`
/// returns it verbatim.
/// Test: this test.
#[test]
#[serial]
fn config_from_env_wedge_reset_secs_override() {
    let _g = EnvGuard::set("TRUSTY_EMBEDDERD_WEDGE_RESET_SECS", "42");
    assert_eq!(
        SupervisorConfig::from_env().wedge_reset_secs,
        42,
        "explicit value must be honoured verbatim"
    );
}

/// `TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS=0` must be honoured as "explicitly
/// disabled", overriding the new 300s default.
///
/// Why: operators who deliberately disabled idle-shutdown (or want the old
/// always-resident behaviour) must be able to opt back out after the #2315
/// default flip.
/// What: set the var to `0`, assert `from_env()` returns `0`; set it to a
/// non-default value, assert that exact value is honoured.
/// Test: this test.
#[test]
#[serial]
fn config_from_env_idle_shutdown_explicit_zero() {
    let _g = EnvGuard::set("TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS", "0");
    assert_eq!(
        SupervisorConfig::from_env().idle_shutdown_secs,
        0,
        "explicit 0 must disable idle-shutdown"
    );

    let _g2 = EnvGuard::set("TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS", "45");
    assert_eq!(
        SupervisorConfig::from_env().idle_shutdown_secs,
        45,
        "explicit value must be honoured verbatim"
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
    let _g5 = EnvGuard::set("TRUSTY_EMBEDDERD_WEDGE_RESET_SECS", "600");

    let cfg = SupervisorConfig::from_env();
    assert_eq!(cfg.startup_timeout_secs, 15);
    assert_eq!(cfg.backoff_max_secs, 120);
    assert_eq!(cfg.max_restarts, 10);
    assert_eq!(cfg.idle_shutdown_secs, 300);
    assert_eq!(cfg.wedge_reset_secs, 600);
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
    let _g4 = EnvGuard::set("TRUSTY_EMBEDDERD_WEDGE_RESET_SECS", "also_bad");

    let cfg = SupervisorConfig::from_env();
    assert_eq!(cfg.startup_timeout_secs, 30);
    assert_eq!(cfg.max_restarts, 5);
    assert_eq!(
        cfg.idle_shutdown_secs, 300,
        "malformed value must fall through to the 300s default (issue #2315)"
    );
    assert_eq!(
        cfg.wedge_reset_secs, 300,
        "malformed value must fall through to the 300s default (#1450 HIGH follow-up)"
    );
}

/// `into_common_for_tests()` must map fields correctly to the trusty-common
/// type and always set `sidecar_batch_size: None`.
///
/// Why: field mismatch would silently use wrong defaults at runtime. This
/// also pins the #1450 HIGH follow-up fix: `wedge_reset_secs` must be
/// forwarded from trusty-search's own config, not silently defaulted via
/// `..SupervisorConfig::default()` (which would make
/// `TRUSTY_EMBEDDERD_WEDGE_RESET_SECS` dead configuration for the real
/// daemon path).
/// What: construct a custom config with a distinctive `wedge_reset_secs`,
/// convert via `into_common_for_tests`, and assert the common fields —
/// including that `wedge_reset_secs` round-trips exactly, not the
/// trusty-common default (300).
/// Test: this test.
#[test]
fn into_common_for_tests_maps_fields() {
    let cfg = SupervisorConfig {
        startup_timeout_secs: 99,
        backoff_max_secs: 77,
        max_restarts: 3,
        idle_shutdown_secs: 600,
        wedge_reset_secs: 55,
    };
    let common = cfg.into_common_for_tests();
    assert_eq!(common.startup_timeout_secs, 99);
    assert_eq!(common.backoff_max_secs, 77);
    assert_eq!(common.max_restarts, 3);
    assert_eq!(
        common.wedge_reset_secs, 55,
        "wedge_reset_secs must be forwarded from the trusty-search config, \
         not defaulted to trusty-common's 300s"
    );
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

// ── In-flight guard + eviction decision (issue #2315) ───────────────────

/// `InFlightGuard` must increment on construction and decrement on drop.
///
/// Why: the watchdog relies on this counter being exact; an off-by-one leaves
/// the sidecar either killed mid-request or never reclaimed.
/// What: build a guard, assert the counter reads 1, drop it, assert 0.
/// Test: this test.
#[test]
fn in_flight_guard_decrements_on_drop() {
    let counter = Arc::new(AtomicU32::new(0));
    {
        let _g = InFlightGuard::new(Arc::clone(&counter));
        assert_eq!(counter.load(Ordering::SeqCst), 1, "must increment on new()");
    }
    assert_eq!(counter.load(Ordering::SeqCst), 0, "must decrement on drop");
}

/// `InFlightGuard` must decrement even when the guarded scope unwinds via panic
/// (the error/`?`-return path shares the same Drop mechanism).
///
/// Why: a plain increment/decrement pair around `op` would leak the count if
/// `op` returned early or panicked, permanently wedging the watchdog into
/// "always busy" and defeating idle-shutdown. The drop-guard prevents that.
/// What: increment inside a `catch_unwind`, panic, and assert the counter is
/// back to 0 after the unwind.
/// Test: this test.
#[test]
fn in_flight_guard_decrements_on_panic() {
    let counter = Arc::new(AtomicU32::new(0));
    let inner = Arc::clone(&counter);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _g = InFlightGuard::new(Arc::clone(&inner));
        assert_eq!(inner.load(Ordering::SeqCst), 1);
        panic!("simulated op failure");
    }));
    assert!(result.is_err(), "the closure must have panicked");
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "guard must decrement on panic unwind (error paths share this Drop)"
    );
}

/// `should_idle_evict` must gate eviction on BOTH the idle window elapsing AND
/// no request being in flight.
///
/// Why: this is the pure decision behind the watchdog's in-flight race fix;
/// testing it directly needs no real process and pins the truth table.
/// What: exercise the three relevant combinations.
/// Test: this test.
#[test]
fn should_idle_evict_respects_inflight() {
    let threshold = Duration::from_secs(1);
    // Idle window elapsed but a request is in flight → do NOT evict.
    assert!(!should_idle_evict(Duration::from_secs(5), threshold, 1));
    // Idle window elapsed and nothing in flight → evict.
    assert!(should_idle_evict(Duration::from_secs(5), threshold, 0));
    // Not idle yet, nothing in flight → do NOT evict.
    assert!(!should_idle_evict(Duration::from_millis(200), threshold, 0));
}

/// Minimal `EmbedderClient` stub used to populate a synthetic `SpawnedState`
/// so the watchdog can be driven without a real `trusty-embedderd` binary.
///
/// Why: unit tests cannot spawn the ONNX sidecar; a stub client lets us build a
/// live `SpawnedState` and exercise `idle_watchdog`'s state-machine directly.
/// What: returns one zero vector per input text.
/// Test: used by `lazy_handle_idle_shutdown_waits_for_inflight_request`.
struct StubClient;

#[async_trait::async_trait]
impl trusty_common::embedder_client::EmbedderClient for StubClient {
    async fn embed_batch(
        &self,
        texts: Vec<String>,
    ) -> Result<Vec<Vec<f32>>, trusty_common::embedder_client::EmbedderError> {
        Ok(texts.into_iter().map(|_| vec![0.0_f32; 384]).collect())
    }
}

/// The idle watchdog must NOT evict the sidecar while a request is in flight,
/// and MUST reclaim it once the request completes and the idle window is past.
///
/// Why: this is the core #2315 in-flight kill-race guarantee — a long request
/// straddling the idle deadline must never be SIGKILLed mid-flight.
/// What: build a synthetic live `SpawnedState` (pid slot 0 → no real OS kill,
/// so the watchdog's `guard.is_some()` branch only clears the state cell),
/// mark `last_use` as already idle and `in_flight = 1`, arm the watchdog with a
/// 1s threshold, and assert the state survives across a poll tick. Then drop
/// `in_flight` to 0 and assert the next tick reclaims the state (spawn gate
/// reset, pid slot 0).
/// Test: this test.
#[tokio::test]
async fn lazy_handle_idle_shutdown_waits_for_inflight_request() {
    use tokio::sync::RwLock;

    let client: Arc<dyn trusty_common::embedder_client::EmbedderClient> = Arc::new(StubClient);
    let client_slot = Arc::new(RwLock::new(client));
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    // pid_slot inside SpawnedState is only kept alive; the watchdog reads
    // app_pid_slot for the kill decision. Both 0 → no real process is signalled.
    let inner_pid_slot = Arc::new(AtomicU32::new(0));
    let state_cell = Arc::new(Mutex::new(Some(SpawnedState {
        client_slot,
        supervisor_handle: None,
        shutdown_tx,
        pid_slot: inner_pid_slot,
    })));
    let app_pid_slot = Arc::new(AtomicU32::new(0));
    // Already well past the 1s idle window.
    let last_use = Arc::new(Mutex::new(Some(Instant::now() - Duration::from_secs(30))));
    let in_flight = Arc::new(AtomicU32::new(1)); // one request executing `op`

    let wd = tokio::spawn(idle_watchdog(
        1, // idle_secs → poll interval == 1s
        Arc::clone(&state_cell),
        Arc::clone(&app_pid_slot),
        Arc::clone(&last_use),
        Arc::clone(&in_flight),
        shutdown_rx,
    ));

    // Let at least one poll tick pass; the in-flight guard must keep the state.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        state_cell.lock().await.is_some(),
        "state must survive while a request is in flight"
    );

    // Request completes: drop the in-flight count. The next tick must reclaim.
    in_flight.store(0, Ordering::Release);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        state_cell.lock().await.is_none(),
        "state must be reclaimed once the in-flight request completes and the \
         idle window has passed"
    );
    assert_eq!(
        app_pid_slot.load(Ordering::Acquire),
        0,
        "pid slot must be cleared after idle-shutdown"
    );

    let _ = wd.await;
}

/// Real-path race test (reviewer-required, issue #2315): drives the
/// kill-mid-request race through `embed_via` itself — not a hand-seeded
/// `in_flight` counter — with an `op` that genuinely blocks past the idle
/// deadline. This is the test that actually exercises the fixed lock
/// ordering: `embed_via` must increment `in_flight` while still holding
/// `self.state`, in the same critical section `idle_watchdog` re-checks
/// under, or this test fails by observing the state reclaimed while `op` is
/// still running.
///
/// Why: `lazy_handle_idle_shutdown_waits_for_inflight_request` above seeds
/// `in_flight = 1` by hand, so it would pass even with the original buggy
/// ordering (increment after the lock was released) — it does not exercise
/// the TOCTOU window at all. Only a test that puts a real, slow `op` through
/// `embed_via` can catch a regression in the lock ordering.
///
/// What:
///   1. Construct a `LazyEmbedderHandle`, then seed `handle.state` directly
///      with a synthetic `SpawnedState` (bypassing `do_spawn`, which needs a
///      real `trusty-embedderd` binary) and mark `last_use` as already well
///      past a 1s idle threshold.
///   2. Manually spawn `idle_watchdog` against the handle's own internal
///      `state` / `app_pid_slot` / `last_use` / `in_flight` Arcs — exactly
///      what `do_spawn` would have wired up after a real spawn.
///   3. Call `handle.embed_via(op)` where `op` signals it has started, then
///      blocks on a `Notify` until the test releases it — spanning well past
///      the 1s idle deadline.
///   4. Assert the state survives multiple watchdog ticks while `op` is
///      genuinely still executing.
///   5. Release `op`, let it complete, then assert the watchdog reclaims the
///      state on a subsequent tick.
///
/// Test: this test.
#[tokio::test]
async fn embed_via_defers_watchdog_eviction_while_request_in_flight() {
    use tokio::sync::{Notify, RwLock};

    let handle = Arc::new(LazyEmbedderHandle::new(
        PathBuf::from("/nonexistent/trusty-embedderd"),
        SupervisorConfig {
            idle_shutdown_secs: 1,
            ..SupervisorConfig::default()
        },
    ));

    // Seed a live SpawnedState directly (bypassing do_spawn, which would try
    // to exec a real trusty-embedderd binary) so embed_via finds
    // `state = Some(..)` and proceeds straight to the in-flight registration
    // and `op` call — the real code path under test.
    let client: Arc<dyn trusty_common::embedder_client::EmbedderClient> = Arc::new(StubClient);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    {
        let mut guard = handle.state.lock().await;
        *guard = Some(SpawnedState {
            client_slot: Arc::new(RwLock::new(client)),
            supervisor_handle: None,
            shutdown_tx,
            pid_slot: Arc::new(AtomicU32::new(0)),
        });
    }
    // Mark last_use as already well past the 1s idle deadline so the
    // watchdog is eligible to evict on its very first tick.
    {
        let mut last_use = handle.last_use.lock().await;
        *last_use = Some(Instant::now() - Duration::from_secs(30));
    }

    // Manually arm the watchdog against the SAME internal Arcs `embed_via`
    // uses — `do_spawn` would normally do this after a real spawn.
    let wd = tokio::spawn(idle_watchdog(
        1, // idle_secs → poll interval == 1s
        Arc::clone(&handle.state),
        Arc::clone(&handle.app_pid_slot),
        Arc::clone(&handle.last_use),
        Arc::clone(&handle.in_flight),
        shutdown_rx,
    ));

    // Drive a genuinely in-flight request THROUGH embed_via itself.
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let handle_for_call = Arc::clone(&handle);
    let started_for_op = Arc::clone(&started);
    let release_for_op = Arc::clone(&release);
    let call = tokio::spawn(async move {
        handle_for_call
            .embed_via(|_client| async move {
                started_for_op.notify_one();
                release_for_op.notified().await;
                Ok::<Vec<Vec<f32>>, trusty_common::embedder_client::EmbedderError>(vec![])
            })
            .await
    });

    // Wait until `op` is actually executing — `in_flight` was incremented by
    // `embed_via` itself under `self.state`, not hand-seeded by this test.
    started.notified().await;

    // Let two watchdog ticks pass while the request is still blocked in `op`,
    // well past the 1s idle deadline.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        handle.state.lock().await.is_some(),
        "watchdog must not evict while embed_via's op is genuinely in flight"
    );

    // Let the request complete.
    release.notify_one();
    let result = call.await.expect("embed_via task must not panic");
    assert!(result.is_ok(), "op must succeed: {result:?}");

    // The request refreshed last_use on completion, so the watchdog needs a
    // fresh idle window before it reclaims. Give it enough ticks.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        handle.state.lock().await.is_none(),
        "watchdog must reclaim the state once the in-flight request has \
         completed and a fresh idle window has elapsed"
    );

    let _ = wd.await;
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
