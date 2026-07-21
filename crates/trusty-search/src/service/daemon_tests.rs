//! Unit tests for `service::daemon` (extracted from `daemon.rs` to keep the
//! production file under the 500-SLOC cap — mirrors the `persistence_tests.rs`
//! split, issue #1372's pattern).
//!
//! Why: `daemon.rs` carries the lockfile/port-file/http_addr resolution and
//! the `run_daemon()` entry point itself; the #3602-review fix (shared
//! discovery registration/deregistration) plus its regression tests pushed
//! the file over the production SLOC cap. Splitting the tests into this
//! sibling `#[path]`-included module restores compliance without changing
//! coverage.
//! What: lockfile/port-file/PID-liveness/auto-port tests, the `TRUSTY_DATA_DIR`
//! path-resolution regression tests (issue #3545), and the shared-discovery
//! registration/deregistration regression tests (issue #3602 review).
//! Test: this module IS the tests.

use super::*;
use serial_test::serial;
use std::net::TcpListener as StdTcpListener;

#[test]
fn http_addr_file_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("http_addr");
    write_http_addr_file(&path, "127.0.0.1:54321").unwrap();
    let read = std::fs::read_to_string(&path).unwrap();
    assert_eq!(read.trim(), "127.0.0.1:54321");
}

#[test]
fn port_file_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.port");
    write_port_file(&path, 12345).unwrap();
    let read = std::fs::read_to_string(&path).unwrap();
    assert_eq!(read.trim(), "12345");
}

#[test]
fn pid_alive_current_process_is_alive() {
    // Why: smoke-test the PID-aliveness predicate so the launchd
    // crash-loop fix has explicit coverage. Our own PID must register
    // as alive; a clearly-invalid PID must not.
    assert!(pid_alive(std::process::id()));
    // Find a clearly-dead PID. macOS `pid_max` defaults to 99999 and
    // Linux to 4194304; on both, a value just under i32::MAX is well
    // beyond the legal range and `kill(pid, 0)` returns ESRCH.
    // (u32::MAX would narrow to -1 on i32 cast, which `kill` interprets
    // as "every process the caller can signal" — never ESRCH.)
    assert!(!pid_alive(2_000_000_000));
}

#[test]
fn read_lockfile_pid_parses_pid() {
    // Why: `running_daemon_pid` depends on this parser. A malformed
    // file must return None rather than panic.
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.lock");
    std::fs::write(&good, "12345\n").unwrap();
    assert_eq!(read_lockfile_pid(&good), Some(12345));

    let bad = dir.path().join("bad.lock");
    std::fs::write(&bad, "not-a-pid").unwrap();
    assert_eq!(read_lockfile_pid(&bad), None);
}

#[test]
fn lockfile_contention_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.lock");
    let _first = acquire_lock(&path).unwrap();
    let err = acquire_lock(&path).unwrap_err();
    assert!(matches!(err, DaemonError::AlreadyRunning(_)));
}

#[tokio::test]
async fn auto_port_walks_forward() {
    // Bind a port, then ask the auto-port allocator to start there.
    let occupied = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let occupied_port = occupied.local_addr().unwrap().port();
    let next = bind_with_auto_port(occupied_port, 64).await.unwrap();
    assert_ne!(next.local_addr().unwrap().port(), occupied_port);
}

#[tokio::test]
async fn auto_port_zero_uses_os() {
    // Note: port 0 is special — the shared helper delegates to the OS.
    let l = bind_with_auto_port(0, 1).await.unwrap();
    assert!(l.local_addr().unwrap().port() > 0);
}

/// Why: `daemon_dir()` must respect `TRUSTY_DATA_DIR` so an isolated daemon
/// can run alongside the production daemon without lockfile conflicts (#281).
/// What: set env var to a tempdir path; assert `daemon_dir()` returns it.
/// Test: `daemon_dir_respects_trusty_data_dir_env_var` (this test).
///
/// `#[serial]` is required because this test mutates the `TRUSTY_DATA_DIR`
/// process env var; running it concurrently with other `TRUSTY_DATA_DIR`
/// mutations in `daemon_paths_under_data_dir_override` or the `start.rs`
/// auto-discover tests causes a flaky race condition.
#[test]
#[serial]
fn daemon_dir_respects_trusty_data_dir_env_var() {
    let tmp = tempfile::tempdir().unwrap();
    let override_path = tmp.path().to_path_buf();
    // SAFETY: test-only, single-threaded portion; no other thread reads
    // TRUSTY_DATA_DIR in this test binary at the same time.
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", &override_path);
    }
    let result = daemon_dir();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }
    let dir = result.expect("daemon_dir with TRUSTY_DATA_DIR should succeed");
    assert_eq!(dir, override_path, "daemon_dir should return the override");
    assert!(dir.exists(), "daemon_dir should create the directory");
}

/// Why: `daemon_lock_path()` and `daemon_port_path()` (service side) must
/// land under the override directory, not the platform default.
/// What: set env var, call both path functions, confirm they start with the
/// override root rather than the default data-local dir.
/// Test: `daemon_paths_under_data_dir_override` (this test).
///
/// `#[serial]` is required because this test mutates the `TRUSTY_DATA_DIR`
/// process env var; running it concurrently with other `TRUSTY_DATA_DIR`
/// mutations (e.g. `daemon_dir_respects_trusty_data_dir_env_var` or the
/// `start.rs` auto-discover tests) causes a flaky race on the env-var read
/// inside `daemon_dir()`.
#[test]
#[serial]
fn daemon_paths_under_data_dir_override() {
    let tmp = tempfile::tempdir().unwrap();
    let override_path = tmp.path().to_path_buf();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", &override_path);
    }
    let lock = daemon_lock_path();
    let port = daemon_port_path();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }
    let lock = lock.expect("lock path must resolve");
    let port = port.expect("port path must resolve");
    assert!(
        lock.starts_with(&override_path),
        "lock path {lock:?} should be under override {override_path:?}"
    );
    assert!(
        port.starts_with(&override_path),
        "port path {port:?} should be under override {override_path:?}"
    );
}

/// Regression for issue #3545: `http_addr_path()` must respect
/// `TRUSTY_DATA_DIR` exactly like `daemon_dir()`/`daemon_port_path()` do,
/// so an isolated daemon's discovery file never lands in the shared
/// `$HOME/.trusty-search/http_addr` location used by the production daemon.
/// What: set `TRUSTY_DATA_DIR` to a tempdir; assert the returned path is
/// `{tempdir}/http_addr`, not the `$HOME`-relative default.
/// Test: `http_addr_path_respects_trusty_data_dir` (this test).
///
/// `#[serial]` for the same reason as the other `TRUSTY_DATA_DIR` tests in
/// this module: the env var is process-global.
#[test]
#[serial]
fn http_addr_path_respects_trusty_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let override_path = tmp.path().to_path_buf();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR", &override_path);
    }
    let path = http_addr_path();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
    }
    let path = path.expect("http_addr_path should resolve with TRUSTY_DATA_DIR set");
    assert_eq!(
        path,
        override_path.join("http_addr"),
        "http_addr_path should land under the TRUSTY_DATA_DIR override, not $HOME"
    );
}

/// Regression for the #3602 review finding: `register_shared_discovery`
/// must populate the generic `trusty_common` registry for the default
/// (non-isolated) instance, since `resolve_search_url`
/// (`trusty-search monitor status`/`monitor indexes`/`monitor tui`) and
/// trusty-installer's `resolve_base_url` have no other way to discover it.
///
/// Why safe: only `trusty_common::write_daemon_addr`/`read_daemon_addr`
/// are exercised here, both redirected into a tempdir via
/// `TRUSTY_DATA_DIR_OVERRIDE` -- this never touches `daemon_lock_path`/
/// `daemon_port_path`/`http_addr_path` (which would resolve to a REAL
/// production path with `TRUSTY_DATA_DIR` unset), so it cannot collide
/// with a real running daemon on this machine.
/// What: unset `TRUSTY_DATA_DIR`, call `register_shared_discovery`, assert
/// the address round-trips through `trusty_common::read_daemon_addr`.
/// Test: this function.
#[test]
#[serial]
fn register_shared_discovery_writes_when_default_instance() {
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", tmp.path());
    }
    let addr: SocketAddr = "127.0.0.1:54321".parse().unwrap();
    register_shared_discovery(&addr);
    let got = trusty_common::read_daemon_addr("trusty-search").unwrap();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
    }
    assert_eq!(
        got.as_deref(),
        Some("127.0.0.1:54321"),
        "default instance must populate the shared discovery registry"
    );
}

/// Regression for the #3602 review finding: an isolated `TRUSTY_DATA_DIR`
/// instance must NEVER populate the shared registry -- that would
/// reintroduce the exact cross-instance pollution issue #3545 fixed.
///
/// Why safe: same reasoning as the sibling test above -- only the
/// `TRUSTY_DATA_DIR_OVERRIDE`-redirected generic registry is touched.
/// What: set both `TRUSTY_DATA_DIR` (isolation) and
/// `TRUSTY_DATA_DIR_OVERRIDE` (safety redirect); call
/// `register_shared_discovery`; assert the registry stays empty.
/// Test: this function.
#[test]
#[serial]
fn register_shared_discovery_noop_when_isolated() {
    let override_tmp = tempfile::tempdir().unwrap();
    let data_dir_tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", override_tmp.path());
        std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
    }
    let addr: SocketAddr = "127.0.0.1:54322".parse().unwrap();
    register_shared_discovery(&addr);
    let got = trusty_common::read_daemon_addr("trusty-search").unwrap();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
    }
    assert!(
        got.is_none(),
        "isolated instance must not populate the shared discovery registry; got {got:?}"
    );
}

/// Regression for the #3602 review finding: shutdown must clear the
/// shared registry entry for the default instance, mirroring the
/// `http_addr_written` cleanup.
///
/// Why safe: same `TRUSTY_DATA_DIR_OVERRIDE` redirection as the writer
/// tests above.
/// What: seed the registry, unset `TRUSTY_DATA_DIR`, call
/// `deregister_shared_discovery`, assert the entry is gone.
/// Test: this function.
#[test]
#[serial]
fn deregister_shared_discovery_removes_when_default_instance() {
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", tmp.path());
    }
    trusty_common::write_daemon_addr("trusty-search", "127.0.0.1:1").unwrap();
    deregister_shared_discovery();
    let got = trusty_common::read_daemon_addr("trusty-search").unwrap();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
    }
    assert!(
        got.is_none(),
        "default instance must clear the shared registry on shutdown"
    );
}

/// End-to-end regression for the #3602 review finding: a REAL isolated
/// `run_daemon()` instance must write its own `TRUSTY_DATA_DIR`-scoped
/// `http_addr` file (so its own CLI clients still work, issue #3545)
/// while never touching the shared, non-isolated registry that
/// `resolve_search_url`/`resolve_base_url` read.
///
/// Why this is the only `run_daemon()` variant safe to execute directly:
/// with `TRUSTY_DATA_DIR` set, `daemon_lock_path()`/`daemon_port_path()`/
/// `http_addr_path()` all resolve entirely under the tempdir -- never a
/// real production path -- so binding a real ephemeral port (`0`) and
/// running the full daemon lifecycle here cannot collide with, or
/// mutate, a real daemon on this machine. The mirror case (default
/// instance actually writes) is covered above by testing
/// `register_shared_discovery`/`deregister_shared_discovery` directly,
/// because that variant would otherwise need `TRUSTY_DATA_DIR` unset --
/// which would make `daemon_lock_path()` resolve to the REAL production
/// lockfile.
/// What: spawns `run_daemon(SearchAppState::new(..), 0)` with
/// `TRUSTY_DATA_DIR` + `TRUSTY_DATA_DIR_OVERRIDE` both pointed at
/// tempdirs; polls for the isolated `http_addr` file to appear; asserts
/// the shared registry stays empty throughout; triggers graceful
/// shutdown via `state.shutdown_tx`; asserts the isolated `http_addr`
/// file is cleaned up.
/// Test: this function.
#[tokio::test]
#[serial]
async fn run_daemon_isolated_instance_never_pollutes_shared_discovery() {
    use crate::core::registry::IndexRegistry;

    let override_tmp = tempfile::tempdir().unwrap();
    let data_dir_tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", override_tmp.path());
        std::env::set_var("TRUSTY_DATA_DIR", data_dir_tmp.path());
    }

    let state = SearchAppState::new(IndexRegistry::new());
    let shutdown_tx = state.shutdown_tx.clone();
    let handle = tokio::spawn(run_daemon(state, 0));

    let isolated_http_addr = data_dir_tmp.path().join("http_addr");
    let mut seen = false;
    for _ in 0..100 {
        if isolated_http_addr.exists() {
            seen = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        seen,
        "isolated instance must still write its own TRUSTY_DATA_DIR-scoped http_addr"
    );

    let shared_during = trusty_common::read_daemon_addr("trusty-search").unwrap();

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;

    let isolated_gone = !isolated_http_addr.exists();
    unsafe {
        std::env::remove_var("TRUSTY_DATA_DIR");
        std::env::remove_var("TRUSTY_DATA_DIR_OVERRIDE");
    }

    assert!(
        shared_during.is_none(),
        "isolated instance must never appear in the shared discovery registry; got {shared_during:?}"
    );
    assert!(
        isolated_gone,
        "isolated instance's own http_addr file must be removed on shutdown"
    );
}
